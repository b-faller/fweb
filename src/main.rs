use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use log::{debug, error, info};
use pulldown_cmark::{Options, Parser};
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
mod template;

use crate::{
    config::Config,
    error::{Error, Result},
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

#[derive(Debug, Clone, Deserialize)]
struct PageMetadata {
    /// ID used for URLs.
    id: String,

    /// Post title.
    title: String,

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

    /// Excerpt of the post content.
    #[serde(default)]
    excerpt: Option<String>,

    /// Date when the page was written
    #[serde(default)]
    #[serde(deserialize_with = "optional_datetime")]
    date: Option<OffsetDateTime>,

    /// The path to the markdown input file.
    ///
    /// This path is relative to the `content/`
    #[serde(skip_deserializing)]
    filepath: PathBuf,

    /// Template file to use.
    ///
    /// This path is relative to `templates/`
    #[serde(default = "default_page_template")]
    template: PathBuf,
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

/// A page is an HTML file within a folder.
#[derive(Debug, Clone, Deserialize)]
struct Page {
    metadata: PageMetadata,
    html: String,
}

impl Page {
    async fn parse_md(content_dir: impl AsRef<Path>, relpath: impl AsRef<Path>) -> Result<Self> {
        let file = content_dir.as_ref().join(&relpath);
        let content = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| Error::ReadInput(relpath.as_ref().to_path_buf(), e))?;

        let (frontmatter, markdown) = parse_file(&content, file)?;
        let mut metadata: PageMetadata = toml::from_str(frontmatter)
            .map_err(|e| Error::ParseMetadata(relpath.as_ref().to_path_buf(), e))?;
        metadata.filepath = relpath.as_ref().to_path_buf();

        Ok(Self {
            metadata,
            html: convert_markdown(markdown),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct IndexMetadata {
    /// Page title.
    title: String,

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

    /// The path to the markdown input file.
    ///
    /// This path is relative to `content/`
    #[serde(skip_deserializing)]
    filepath: PathBuf,
}

fn default_index_template() -> PathBuf {
    "index.html".into()
}

/// An index is the `_index.html` within a folder in the content.
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
    async fn parse_md(content_dir: impl AsRef<Path>, relpath: impl AsRef<Path>) -> Result<Self> {
        let file = content_dir.as_ref().join(&relpath);
        let content = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| Error::ReadInput(relpath.as_ref().to_path_buf(), e))?;

        let (frontmatter, markdown) = parse_file(&content, file)?;
        let mut metadata: IndexMetadata = toml::from_str(frontmatter)
            .map_err(|e| Error::ParseMetadata(relpath.as_ref().to_path_buf(), e))?;
        metadata.filepath = relpath.as_ref().to_path_buf();

        Ok(Self {
            metadata,
            html: convert_markdown(markdown),
            pages: Vec::new(),
        })
    }
}

#[derive(Debug)]
struct Website {
    /// Configuration for this website.
    config: Config,
}

impl Website {
    /// Create a new website.
    fn new(config: Config) -> Self {
        Website { config }
    }

    /// Build the website to HTML content.
    async fn build(self) -> Result<()> {
        // Copy all assets
        let from = self.config.content_path.join("assets");
        let to = self.config.output_path.clone();
        let mirror_assets_handle = tokio::spawn(async move { mirror_assets(from, to).await });

        // Read and parse content
        let content_dir = self.config.content_path.join("content");
        let indices = load_and_parse_content(content_dir).await?;

        // Fill templating context
        let mut ctx = template::Context::new();
        ctx.insert("nav", build_navigation(&indices));
        ctx.insert("articles", build_article_list(&indices));
        ctx.insert("site_title", self.config.site_info.title.to_string());
        ctx.insert(
            "site_description",
            self.config.site_info.description.to_string(),
        );

        export_indices_to_html(&self.config, ctx, indices).await?;

        mirror_assets_handle.await.map_err(Error::Join)??;

        Ok(())
    }
}

/// Loads and parses all content in the `content_dir`.
///
/// Returns the base index which contains all further pages.
async fn load_and_parse_content(content_dir: PathBuf) -> Result<Vec<Index>> {
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
                    pages_handles.push(tokio::spawn(async move {
                        Page::parse_md(content_dir, relpath).await
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

            let mut index =
                tokio::spawn(async move { Index::parse_md(content_dir, relpath).await })
                    .await
                    .map_err(Error::Join)??;
            index.pages = pages;

            // Sort pages
            // We use unstable here since _I suppose_ pages are already in arbitrary order
            // coming from the async tasks.
            index.pages.sort_unstable_by(|p1, p2| {
                match index.metadata.sort_by {
                    SortOrder::Title => p1.metadata.title.cmp(&p2.metadata.title),
                    SortOrder::Date => {
                        // Sort pages based on their date descending.
                        p2.metadata.date.cmp(&p1.metadata.date)
                    }
                    SortOrder::Weight => p1.metadata.weight.cmp(&p2.metadata.weight),
                }
            });

            indices.push(index);
        }
    }

    Ok(indices)
}

/// Write all indices to disk.
async fn export_indices_to_html(
    config: &Config,
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
        ctx.insert("title", index.metadata.title.to_string());
        ctx.insert("content", index.html.to_string());

        // Apply templating
        let templates_dir = config.content_path.join("templates");
        let template_path = templates_dir.join(&index.metadata.template);
        let template = tokio::fs::read_to_string(&template_path)
            .await
            .map_err(|e| Error::ReadInput(template_path, e))?;
        let html = template::template(config, &ctx, template).await?;

        // Write index.html
        tokio::fs::write(&file, html)
            .await
            .map_err(|e| Error::WriteFile(file, e))?;

        // Export pages
        let mut handles = Vec::new();
        for page in index.pages {
            let config = config.clone();
            let mut ctx = ctx.clone();
            let templates_dir = templates_dir.clone();

            handles.push(tokio::spawn(async move {
                debug!("Building page '{:?}'", &page.metadata);

                // Build page context
                ctx.insert("content", page.html.to_string());
                ctx.insert("title", page.metadata.title.to_string());
                if let Some(excerpt) = page.metadata.excerpt {
                    ctx.insert("excerpt", excerpt);
                }
                if let Some(date) = page.metadata.date {
                    ctx.insert("date_iso8601", format_date_iso8601(&date));
                    ctx.insert("date", format_date_utc(&date));
                }

                // Apply templating
                let template_path = templates_dir.join(&page.metadata.template);
                let template = tokio::fs::read_to_string(&template_path)
                    .await
                    .map_err(|e| Error::ReadInput(template_path, e))?;
                let html = template::template(&config, &ctx, template).await?;

                // Write page HTML to file
                let dir = config
                    .output_path
                    .join(page.metadata.filepath.parent().unwrap())
                    .join(page.metadata.id);
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
        .flat_map(|index| index.metadata.display_in_nav.map(|i| (i, index)))
        .for_each(|(i, index)| {
            let path = PathBuf::from("/")
                .join(index.metadata.filepath.parent().unwrap())
                .display()
                .to_string();
            if path.len() > 1 {
                navs.push((
                    i,
                    format!("<a href=\"{}/\">{}</a>\n", path, index.metadata.title),
                ));
            } else {
                navs.push((i, format!("<a href=\"/\">{}</a>\n", index.metadata.title)));
            }
            index
                .pages
                .iter()
                .flat_map(|page| page.metadata.display_in_nav.map(|i| (i, page)))
                .for_each(|(i, page)| {
                    let path = PathBuf::from("/")
                        .join(index.metadata.filepath.parent().unwrap())
                        .join(&page.metadata.id);
                    navs.push((
                        i,
                        format!(
                            "<a href=\"{}/\">{}</a>\n",
                            path.display(),
                            page.metadata.title
                        ),
                    ));
                });
        });

    navs.sort_by_key(|(i, _nav)| *i);
    navs.into_iter().map(|(_i, nav)| nav).collect()
}

/// Build an HTML list of articles.
fn build_article_list(indices: &[Index]) -> String {
    indices
        .iter()
        .flat_map(|index| &index.pages)
        .filter(|page| page.metadata.date.is_some() && page.metadata.excerpt.is_some())
        .map(|page| {
            // Append current metadata as HTML to post TOC
            let path = PathBuf::from("/")
                .join(page.metadata.filepath.parent().unwrap())
                .join(&page.metadata.id);
            format!(
                "<hgroup>\n<h3><a href=\"{path}/\">{title}</a></h3>\n<p><small><time \
                 datetime=\"{date_iso}\">{date_utc}</time></small></p>\n</hgroup><p>{excerpt}</p>\\
                 \
                 n",
                path = path.display(),
                title = page.metadata.title,
                date_iso = format_date_iso8601(&page.metadata.date.unwrap()),
                date_utc = format_date_utc(&page.metadata.date.unwrap()),
                excerpt = page.metadata.excerpt.as_ref().unwrap(),
            )
        })
        .collect()
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
    let parser = Parser::new_ext(markdown, options);

    // Write to String buffer.
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);

    html
}

/// Read and parse site config
async fn read_site_config(path: impl AsRef<Path>) -> std::io::Result<Config> {
    // Read and parse config.
    let path = path.as_ref();
    let content = tokio::fs::read_to_string(path).await?;
    let mut config: Config = toml::from_str(&content)?;

    // Make config paths relative to the configuration file.
    let basedir = path
        .parent()
        .expect("file does exist and must have a parent");
    config.content_path = basedir.join(&config.content_path);
    config.output_path = basedir.join(&config.output_path);

    Ok(config)
}

async fn try_main() -> Result<()> {
    let it = std::time::Instant::now();

    // Get website config
    let config_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "config.toml".into()),
    );
    let config = read_site_config(&config_path)
        .await
        .map_err(|e| Error::ConfigRead(config_path, e))?;

    info!("Config read at {:?}", it.elapsed());

    // Build website.
    Website::new(config).build().await?;

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
