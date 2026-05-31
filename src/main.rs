use std::{
    collections::HashMap,
    ffi::OsStr,
    fmt::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::Parser;
use log::{debug, error, info};
use pulldown_cmark::Options;
use serde::Deserialize;
use template::Context;
use time::{
    format_description::{
        well_known::{
            iso8601::{self, EncodedConfig, TimePrecision},
            Iso8601,
        },
        FormatItem,
    },
    macros::format_description,
    OffsetDateTime,
};

mod config;
mod error;
mod json_ld;
mod template;

use crate::{
    config::{Config, SiteInfo},
    error::{Error, Result},
    json_ld::{Breadcrumb, SchemaType},
};

/// Date format used to display dates.
const DATE_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]Z");

/// Export configuration to export a date and time compatible with the datetime
/// attribute used in the HTML `<time>` element.
const DATE_ISO_CONFIG: EncodedConfig = iso8601::Config::DEFAULT
    .set_time_precision(TimePrecision::Second {
        decimal_digits: None,
    })
    .encode();

/// Command line options.
#[derive(Debug, clap::Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Path to the site config.
    #[arg(default_value = "config.toml", value_hint = clap::ValueHint::FilePath)]
    pub config_path: PathBuf,
    /// Build draft pages.
    #[arg(long, default_value_t = false)]
    pub drafts: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SortOrder {
    /// Sorts pages by their title
    Title,

    /// Sorts pages by their date
    Date,

    /// Sorts pages by a weight
    Weight,
}

/// Open Graph content type.
#[derive(Debug, Clone)]
enum OgType {
    Article,
    Website,
}

impl fmt::Display for OgType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OgType::Article => "article",
            OgType::Website => "website",
        })
    }
}

/// Frontmatter parsed directly from a page's TOML header.
#[derive(Debug, Clone, Deserialize)]
struct PageFrontmatter {
    /// ID used for URLs.
    id: String,

    /// Post title.
    title: String,

    /// Description of the page.
    description: Option<String>,

    /// If the page should be shown in the navigation.
    ///
    /// Given as an positive number at which position the page should be shown.
    /// Note: If two indices or pages have the same number, the ordering is
    /// unspecified between those two entries.
    #[serde(default)]
    display_in_nav: Option<usize>,

    /// A page weight is simply a number associated with the page.
    ///
    /// This can be used to sort pages, in this case the pages are ordered by
    /// their weight in normal numerical order
    weight: Option<i32>,

    /// Author of the page.
    #[serde(default)]
    author: Option<String>,

    /// Excerpt of the post content.
    #[serde(default)]
    excerpt: Option<String>,

    /// Date when the page was written
    #[serde(default)]
    #[serde(deserialize_with = "optional_datetime")]
    date: Option<OffsetDateTime>,

    /// Template file to use.
    ///
    /// This path is relative to `templates/`
    #[serde(default = "default_page_template")]
    template: PathBuf,

    /// Schema.org type for JSON-LD generation.
    #[serde(default)]
    schema: json_ld::SchemaType,

    /// Whether the page is a draft.
    ///
    /// If this is set, only a site build with the draft option enabled will output this page.
    #[serde(default)]
    draft: bool,
}

fn default_page_template() -> PathBuf {
    "page.html".into()
}

fn optional_datetime<'de, D>(d: D) -> std::result::Result<Option<OffsetDateTime>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Wrapper(#[serde(with = "time::serde::iso8601")] OffsetDateTime);

    let wrapper = Option::deserialize(d)?;
    Ok(wrapper.map(|Wrapper(external)| external))
}

/// Full metadata for a page.
///
/// This includes the parsed frontmatter and computed fields.
#[derive(Debug, Clone)]
struct PageMetadata {
    frontmatter: PageFrontmatter,
    /// The path to the markdown input file, relative to `content/`.
    filepath: PathBuf,
    canonical_url: String,
    og_type: OgType,
    breadcrumbs: Vec<Breadcrumb>,
}

impl PageMetadata {
    fn new(frontmatter: PageFrontmatter, filepath: PathBuf, base_url: &str) -> Self {
        let url_path = PathBuf::from("/")
            .join(filepath.parent().unwrap())
            .join(&frontmatter.id);
        let canonical_url = format!("{}{}/", base_url, url_path.display());
        let og_type = if frontmatter.date.is_some() {
            OgType::Article
        } else {
            OgType::Website
        };
        Self {
            frontmatter,
            filepath,
            canonical_url,
            og_type,
            breadcrumbs: Vec::new(),
        }
    }
}

/// A page is an HTML file within a folder.
#[derive(Debug, Clone)]
struct Page {
    metadata: PageMetadata,
    html: String,
}

impl Page {
    async fn parse_md(
        content_dir: impl AsRef<Path>,
        relpath: impl AsRef<Path>,
        config: Arc<Config>,
    ) -> Result<Self> {
        let file = content_dir.as_ref().join(&relpath);
        let content = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| Error::ReadInput(relpath.as_ref().to_path_buf(), e))?;

        let (frontmatter_str, markdown) = parse_file(&content, file)?;
        let frontmatter: PageFrontmatter = toml::from_str(frontmatter_str)
            .map_err(|e| Error::ParseMetadata(relpath.as_ref().to_path_buf(), e))?;
        let filepath = relpath.as_ref().to_path_buf();

        Ok(Self {
            metadata: PageMetadata::new(frontmatter, filepath, &config.site_info.base_url),
            html: convert_markdown(markdown),
        })
    }
}

/// Frontmatter parsed directly from an index's TOML header.
#[derive(Debug, Clone, Deserialize)]
struct IndexFrontmatter {
    /// Page title.
    title: String,

    /// Page description.
    description: Option<String>,

    /// If the index should be shown in the navigation.
    ///
    /// Given as an positive number at which position the page should be shown.
    /// Note: If two indices or pages have the same number, the ordering is
    /// unspecified between those two entries.
    #[serde(default)]
    display_in_nav: Option<usize>,

    /// Sort pages by the specified order
    sort_by: SortOrder,

    /// Template file to use.
    ///
    /// This path is relative to `templates/`
    #[serde(default = "default_index_template")]
    template: PathBuf,

    /// Schema.org type for JSON-LD generation.
    #[serde(default)]
    schema: SchemaType,
}

fn default_index_template() -> PathBuf {
    "index.html".into()
}

#[derive(Debug, Clone)]
struct IndexMetadata {
    frontmatter: IndexFrontmatter,
    /// The path to the markdown input file.
    ///
    /// This path is relative to `content/`
    filepath: PathBuf,
    canonical_url: String,
    breadcrumbs: Vec<Breadcrumb>,
}

impl IndexMetadata {
    fn new(frontmatter: IndexFrontmatter, filepath: PathBuf, site: &SiteInfo) -> Self {
        let parent = filepath.parent().expect("index always has a parent");
        let canonical_url = if parent == Path::new("") {
            format!("{}/", site.base_url)
        } else {
            let url_path = PathBuf::from("/").join(parent);
            format!("{}{}/", site.base_url, url_path.display())
        };
        Self {
            frontmatter,
            filepath,
            canonical_url,
            breadcrumbs: Vec::new(),
        }
    }
}

/// An index is the `_index.md` within a folder in the content.
#[derive(Debug, Clone)]
struct Index {
    metadata: IndexMetadata,
    html: String,
    pages: Vec<Page>,
}

impl Index {
    /// Reads and parses an input markdown file.
    ///
    /// Note: This does not read in any pages
    async fn parse_md(
        content_dir: impl AsRef<Path>,
        relpath: impl AsRef<Path>,
        config: Arc<Config>,
    ) -> Result<Self> {
        let file = content_dir.as_ref().join(&relpath);
        let content = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| Error::ReadInput(relpath.as_ref().to_path_buf(), e))?;

        let (frontmatter_str, markdown) = parse_file(&content, file)?;
        let frontmatter: IndexFrontmatter = toml::from_str(frontmatter_str)
            .map_err(|e| Error::ParseMetadata(relpath.as_ref().to_path_buf(), e))?;

        Ok(Self {
            metadata: IndexMetadata::new(
                frontmatter,
                relpath.as_ref().to_path_buf(),
                &config.site_info,
            ),
            html: convert_markdown(markdown),
            pages: Vec::new(),
        })
    }
}

#[derive(Debug)]
struct Website {
    /// Configuration for this website.
    config: Arc<Config>,
}

impl Website {
    /// Create a new website.
    fn new(config: Config) -> Self {
        Website {
            config: Arc::new(config),
        }
    }

    /// Build the website to HTML content.
    async fn build(self, opts: &Cli) -> Result<()> {
        // Copy all assets
        let from = self.config.content_path.join("assets");
        let to = self.config.output_path.clone();

        // Remove output directory
        tokio::fs::remove_dir_all(&to)
            .await
            .or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(Error::OutputPathClean(to.to_path_buf(), e)),
            })?;

        // Copy all assets
        let mirror_assets_handle = tokio::spawn(async move { mirror_assets(from, to).await });

        // Read and parse content
        let content_dir = self.config.content_path.join("content");
        let mut indices = load_and_parse_content(content_dir, Arc::clone(&self.config)).await?;
        populate_breadcrumbs(&mut indices);

        // Fill templating context
        let mut ctx = template::Context::new();
        ctx.insert("nav", build_navigation(&indices));
        ctx.insert("articles", build_article_list(&indices, opts));
        ctx.insert("site_title", self.config.site_info.title.to_string());
        ctx.insert(
            "site_description",
            self.config.site_info.description.to_string(),
        );

        export_indices_to_html(Arc::clone(&self.config), opts, ctx, indices).await?;

        mirror_assets_handle.await.map_err(Error::Join)??;

        Ok(())
    }
}

/// Loads and parses all content in the `content_dir`.
///
/// Returns the base index which contains all further pages.
async fn load_and_parse_content(content_dir: PathBuf, config: Arc<Config>) -> Result<Vec<Index>> {
    // Discovered indices
    let mut indices = Vec::new();
    // Stack storing the directories which remain to be processed
    let mut stack = vec![content_dir.clone()];

    while let Some(dir) = stack.pop() {
        let mut index = None;
        let mut pages_handles = Vec::new();

        // Iterate over the current directory entries
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .map_err(|e| Error::ReadDirectory(dir.clone(), e))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| Error::ReadDirectory(dir.clone(), e))?
        {
            let file = entry.path();
            if file.is_dir() {
                stack.push(file);
            } else if file.is_file() {
                if file.file_name() == Some(OsStr::new("_index.md")) {
                    index = Some(file);
                } else if file.extension() == Some(OsStr::new("md")) {
                    let content_dir = content_dir.clone();
                    let relpath = file
                        .strip_prefix(&content_dir)
                        .expect("starts with content directory")
                        .to_path_buf();
                    let config = Arc::clone(&config);
                    pages_handles.push(tokio::spawn(async move {
                        Page::parse_md(content_dir, relpath, config).await
                    }));
                }
            }
        }

        let mut pages = Vec::with_capacity(pages_handles.len());
        for handle in pages_handles {
            pages.push(handle.await.map_err(Error::Join)??);
        }

        // Read and process the index
        if let Some(file) = index {
            let content_dir = content_dir.clone();
            let relpath = file
                .strip_prefix(&content_dir)
                .expect("starts with content directory")
                .to_path_buf();

            let config = Arc::clone(&config);
            let mut index =
                tokio::spawn(async move { Index::parse_md(content_dir, relpath, config).await })
                    .await
                    .map_err(Error::Join)??;
            index.pages = pages;

            // Sort pages
            // We use unstable here since _I suppose_ pages are already in arbitrary order
            // coming from the async tasks.
            index.pages.sort_unstable_by(|p1, p2| {
                match index.metadata.frontmatter.sort_by {
                    SortOrder::Title => p1
                        .metadata
                        .frontmatter
                        .title
                        .cmp(&p2.metadata.frontmatter.title),
                    SortOrder::Date => {
                        // Sort pages based on their date descending.
                        p2.metadata
                            .frontmatter
                            .date
                            .cmp(&p1.metadata.frontmatter.date)
                    }
                    SortOrder::Weight => p1
                        .metadata
                        .frontmatter
                        .weight
                        .cmp(&p2.metadata.frontmatter.weight),
                }
            });

            indices.push(index);
        }
    }

    Ok(indices)
}

/// Populate breadcrumbs for all indices and their pages after content is fully loaded.
///
/// Root is never included as an ancestor; the root index itself gets an empty chain.
fn populate_breadcrumbs(indices: &mut [Index]) {
    let by_dir: HashMap<PathBuf, Breadcrumb> = indices
        .iter()
        .map(|idx| {
            let dir = idx
                .metadata
                .filepath
                .parent()
                .expect("Each filepath has parent")
                .to_path_buf();
            let breadcrumb = Breadcrumb {
                name: idx.metadata.frontmatter.title.clone(),
                url: idx.metadata.canonical_url.clone(),
            };
            (dir, breadcrumb)
        })
        .collect();

    for index in indices.iter_mut() {
        let dir = index
            .metadata
            .filepath
            .parent()
            .expect("Each file has a parent directory");

        if dir != Path::new("") {
            let current = Breadcrumb {
                name: index.metadata.frontmatter.title.clone(),
                url: index.metadata.canonical_url.clone(),
            };
            index.metadata.breadcrumbs = build_breadcrumb_chain(dir, false, current, &by_dir);
        }

        for page in index.pages.iter_mut() {
            let parent = page
                .metadata
                .filepath
                .parent()
                .expect("Each file has a parent directory");
            let current = Breadcrumb {
                name: page.metadata.frontmatter.title.clone(),
                url: page.metadata.canonical_url.clone(),
            };
            page.metadata.breadcrumbs = build_breadcrumb_chain(parent, true, current, &by_dir);
        }
    }
}

/// Builds a breadcrumb chain.
///
/// Root (`""`) is never included as an ancestor.
/// `include_dir` = true for pages (include the immediate parent index in the chain),
///               = false for indices (only ancestors strictly above `dir`).
fn build_breadcrumb_chain(
    dir: &Path,
    include_dir: bool,
    current: Breadcrumb,
    by_dir: &std::collections::HashMap<PathBuf, Breadcrumb>,
) -> Vec<Breadcrumb> {
    let mut prefixes: Vec<PathBuf> = vec![];
    let mut cur = PathBuf::new();
    for component in dir.components() {
        cur.push(component);
        prefixes.push(cur.clone());
    }
    if !include_dir {
        prefixes.pop();
    }

    let mut chain: Vec<Breadcrumb> = prefixes
        .iter()
        .filter_map(|p| by_dir.get(p).cloned())
        .collect();

    chain.push(current);
    chain
}

/// Write all indices to disk.
async fn export_indices_to_html(
    config: Arc<Config>,
    opts: &Cli,
    mut ctx: Context,
    indices: Vec<Index>,
) -> Result<()> {
    for index in indices {
        debug!("Building index {:?}", index);

        // Create filepath to store the index.html
        let dir = config.output_path.join(
            index
                .metadata
                .filepath
                .parent()
                .expect("index always has a parent"),
        );
        let file = dir.join("index.html");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| Error::CreateDirectory(dir, e))?;

        // Build index context
        ctx.insert("title", index.metadata.frontmatter.title.to_string());
        ctx.insert("content", index.html.to_string());
        ctx.insert("canonical_url", index.metadata.canonical_url.to_string());
        ctx.insert("og_type", OgType::Website.to_string());
        ctx.insert(
            "description",
            index
                .metadata
                .frontmatter
                .description
                .unwrap_or(config.site_info.description.to_string()),
        );
        ctx.insert(
            "schema_jsonld",
            json_ld::generate(index.metadata.frontmatter.schema, &ctx)?,
        );
        ctx.insert(
            "breadcrumb_jsonld",
            json_ld::generate_breadcrumbs(&index.metadata.breadcrumbs),
        );

        // Apply templating
        let templates_dir = config.content_path.join("templates");
        let template_path = templates_dir.join(&index.metadata.frontmatter.template);
        let template = tokio::fs::read_to_string(&template_path)
            .await
            .map_err(|e| Error::ReadInput(template_path, e))?;
        let html = template::template(&config, &ctx, template).await?;

        // Write index.html
        tokio::fs::write(&file, html)
            .await
            .map_err(|e| Error::WriteFile(file, e))?;

        // Export pages
        let mut handles = Vec::new();
        let pages = index
            .pages
            .into_iter()
            .filter(|page| !page.metadata.frontmatter.draft || opts.drafts);
        for page in pages {
            let config = Arc::clone(&config);
            let mut ctx = ctx.clone();
            let templates_dir = templates_dir.clone();

            handles.push(tokio::spawn(async move {
                debug!("Building page '{:?}'", &page.metadata);

                // Build page context
                ctx.insert("content", page.html.to_string());
                ctx.insert("title", page.metadata.frontmatter.title.to_string());
                ctx.insert("canonical_url", page.metadata.canonical_url.clone());
                ctx.insert("og_type", page.metadata.og_type.to_string());
                ctx.insert(
                    "description",
                    page.metadata
                        .frontmatter
                        .description
                        .as_deref()
                        .or(page.metadata.frontmatter.excerpt.as_deref())
                        .unwrap_or(&config.site_info.description)
                        .to_string(),
                );
                if let Some(excerpt) = page.metadata.frontmatter.excerpt {
                    ctx.insert("excerpt", excerpt);
                }
                if let Some(author) = page.metadata.frontmatter.author {
                    ctx.insert("author", author);
                }
                if let Some(date) = page.metadata.frontmatter.date {
                    ctx.insert("date_iso8601", format_date_iso8601(&date));
                    ctx.insert("date", format_date_utc(&date));
                }
                ctx.insert(
                    "schema_jsonld",
                    json_ld::generate(page.metadata.frontmatter.schema, &ctx)?,
                );
                ctx.insert(
                    "breadcrumb_jsonld",
                    json_ld::generate_breadcrumbs(&page.metadata.breadcrumbs),
                );

                // Apply templating
                let template_path = templates_dir.join(&page.metadata.frontmatter.template);
                let template = tokio::fs::read_to_string(&template_path)
                    .await
                    .map_err(|e| Error::ReadInput(template_path, e))?;
                let html = template::template(&config, &ctx, template).await?;

                // Write page HTML to file
                let dir = config
                    .output_path
                    .join(page.metadata.filepath.parent().unwrap())
                    .join(page.metadata.frontmatter.id);
                tokio::fs::create_dir_all(dir.clone())
                    .await
                    .map_err(|e| Error::CreateDirectory(dir.clone(), e))?;
                let path = dir.join("index.html");
                tokio::fs::write(&path, html)
                    .await
                    .map_err(|e| Error::WriteFile(path, e))?;

                Result::Ok(())
            }))
        }

        for handle in handles {
            handle.await.map_err(Error::Join)??;
        }
    }
    Ok(())
}

/// Create the HTML for the navigation based on the indices and pages.
fn build_navigation(indices: &[Index]) -> String {
    let mut navs = Vec::new();

    indices
        .iter()
        .flat_map(|index| {
            index
                .metadata
                .frontmatter
                .display_in_nav
                .map(|i| (i, index))
        })
        .for_each(|(i, index)| {
            let path = PathBuf::from("/")
                .join(index.metadata.filepath.parent().unwrap())
                .display()
                .to_string();
            if path.len() > 1 {
                navs.push((
                    i,
                    format!(
                        "<a href=\"{}/\">{}</a>\n",
                        path, index.metadata.frontmatter.title
                    ),
                ));
            } else {
                navs.push((
                    i,
                    format!("<a href=\"/\">{}</a>\n", index.metadata.frontmatter.title),
                ));
            }
            index
                .pages
                .iter()
                .flat_map(|page| page.metadata.frontmatter.display_in_nav.map(|i| (i, page)))
                .for_each(|(i, page)| {
                    let path = PathBuf::from("/")
                        .join(index.metadata.filepath.parent().unwrap())
                        .join(&page.metadata.frontmatter.id);
                    navs.push((
                        i,
                        format!(
                            "<a href=\"{}/\">{}</a>\n",
                            path.display(),
                            page.metadata.frontmatter.title
                        ),
                    ));
                });
        });

    navs.sort_by_key(|(i, _nav)| *i);
    navs.into_iter().map(|(_i, nav)| nav).collect()
}

/// Build an HTML list of articles.
fn build_article_list(indices: &[Index], opts: &Cli) -> String {
    indices
        .iter()
        .flat_map(|index| &index.pages)
        .filter(|page| {
            page.metadata.frontmatter.date.is_some()
                && page.metadata.frontmatter.excerpt.is_some()
                && (!page.metadata.frontmatter.draft || opts.drafts)
        })
        .fold(String::new(), |mut output, page| {
            // Append current metadata as HTML to post TOC
            let path = PathBuf::from("/")
                .join(page.metadata.filepath.parent().unwrap())
                .join(&page.metadata.frontmatter.id);
            let _ = write!(
                output,
                "<hgroup>\n<h3><a href=\"{path}/\">{title}</a></h3>\n<p><small><time \
                 datetime=\"{date_iso}\">{date_utc}</time></small></p>\n</hgroup>\n<p>{excerpt}</\
                 p>\n",
                path = path.display(),
                title = page.metadata.frontmatter.title,
                date_iso = format_date_iso8601(&page.metadata.frontmatter.date.unwrap()),
                date_utc = format_date_utc(&page.metadata.frontmatter.date.unwrap()),
                excerpt = page.metadata.frontmatter.excerpt.as_ref().unwrap(),
            );
            output
        })
}

fn format_date_iso8601(date: &OffsetDateTime) -> String {
    date.format(&Iso8601::<DATE_ISO_CONFIG>)
        .expect("date already validated")
}

fn format_date_utc(date: &OffsetDateTime) -> String {
    date.to_offset(time::macros::offset!(UTC))
        .format(&DATE_FORMAT)
        .expect("date already validated")
}

/// Mirror the assets fully.
async fn mirror_assets(from: PathBuf, to: PathBuf) -> Result<()> {
    // Ensure that the output base directory exists.
    tokio::fs::create_dir_all(&to)
        .await
        .map_err(|e| Error::CreateDirectory(to.clone(), e))?;

    // Stack storing the directories which remain to be processed
    let mut stack = vec![(from, to)];

    while let Some((from, to)) = stack.pop() {
        // Iterate over the current directory entries
        let mut entries = tokio::fs::read_dir(&from)
            .await
            .map_err(|e| Error::ReadDirectory(from.clone(), e))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| Error::ReadDirectory(from.clone(), e))?
        {
            let new_from = entry.path();
            let new_to = to.join(entry.file_name());
            if new_from.is_dir() {
                // Replicate the found directory
                tokio::fs::create_dir_all(&new_to)
                    .await
                    .map_err(|e| Error::CreateDirectory(new_to.clone(), e))?;
                // Add the directory to the stack to iterate later
                stack.push((new_from, new_to));
            } else if new_from.is_file() {
                // Copy the found file
                tokio::fs::copy(&new_from, &new_to)
                    .await
                    .map_err(|e| Error::Copy(new_from, new_to, e))?;
            }
        }
    }

    Ok(())
}

/// Extract frontmatter and markdown from a input file.
fn parse_file(input: &str, filepath: impl AsRef<Path>) -> Result<(&str, &str)> {
    let mut split = input.splitn(3, "+++");
    // Empty before frontmatter
    split.next();
    let err = || Error::MalformedContent(filepath.as_ref().into());
    let frontmatter = split.next().ok_or_else(err)?;
    let markdown = split.next().ok_or_else(err)?.trim();
    Ok((frontmatter, markdown))
}

fn convert_markdown(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = pulldown_cmark::Parser::new_ext(markdown, options);

    // Write to String buffer.
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);

    html
}

async fn try_main() -> Result<()> {
    let it = std::time::Instant::now();

    let cli = Cli::parse();
    let config = Config::from_file(&cli.config_path).await?;

    info!("Config read at {:?}", it.elapsed());

    // Build website.
    Website::new(config).build(&cli).await?;

    info!("Website built at {:?}", it.elapsed());

    Ok(())
}

#[tokio::main]
async fn main() {
    env_logger::init();

    if let Err(e) = try_main().await {
        error!("{}", e);
        std::process::exit(1);
    }
}
