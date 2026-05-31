use std::{
    collections::HashMap,
    ffi::OsStr,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::Parser;
use log::{debug, error, info};
use pulldown_cmark::Options;
use serde::Deserialize;
use time::{
    format_description::{
        well_known::{
            iso8601::{self, EncodedConfig, TimePrecision},
            Iso8601, Rfc2822, Rfc3339,
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
    template::{Context, TemplateValue},
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

impl PageFrontmatter {
    /// Validate schema-specific invariants the rest of the pipeline relies on.
    fn validate(&self) -> Result<()> {
        if self.schema == SchemaType::BlogPosting {
            if self.date.is_none() {
                return Err(Error::MissingSchemaField("date", "BlogPosting"));
            }
            if self.author.is_none() {
                return Err(Error::MissingSchemaField("author", "BlogPosting"));
            }
        }
        Ok(())
    }
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
        frontmatter.validate()?;

        let filepath = relpath.as_ref().to_path_buf();

        Ok(Self {
            metadata: PageMetadata::new(frontmatter, filepath, &config.site_info.base_url),
            html: convert_markdown(markdown),
        })
    }

    /// Extend the base context `ctx` with this page's fields for rendering.
    ///
    /// Consumes the page so that the HTML is moved.
    fn into_context(self, mut ctx: Context, config: &Config) -> Result<Context> {
        let meta = self.metadata;
        let fm = meta.frontmatter;

        // Derived presentation fields, computed once and reused below.
        let description = fm
            .description
            .as_deref()
            .or(fm.excerpt.as_deref())
            .unwrap_or(&config.site_info.description)
            .to_string();
        let date_iso8601 = fm.date.map(|d| format_date_iso8601(&d));
        let date_utc = fm.date.map(|d| format_date_utc(&d));
        let schema_jsonld = json_ld::generate(
            fm.schema,
            &config.site_info,
            &fm.title,
            &description,
            &meta.canonical_url,
            date_iso8601.as_deref(),
            fm.author.as_deref(),
        );
        let breadcrumb_jsonld = json_ld::generate_breadcrumbs(&meta.breadcrumbs);

        ctx.insert("content".to_string(), self.html.into());
        ctx.insert("title".to_string(), fm.title.into());
        ctx.insert("canonical_url".to_string(), meta.canonical_url.into());
        ctx.insert("og_type".to_string(), meta.og_type.to_string().into());
        ctx.insert("description".to_string(), description.into());
        if let Some(excerpt) = fm.excerpt {
            ctx.insert("excerpt".to_string(), excerpt.into());
        }
        if let Some(author) = fm.author {
            ctx.insert("author".to_string(), author.into());
        }
        if let (Some(iso), Some(utc)) = (date_iso8601, date_utc) {
            ctx.insert("date_iso8601".to_string(), iso.into());
            ctx.insert("date".to_string(), utc.into());
        }
        ctx.insert("schema_jsonld".to_string(), schema_jsonld.into());
        ctx.insert("breadcrumb_jsonld".to_string(), breadcrumb_jsonld.into());
        Ok(ctx)
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

impl IndexFrontmatter {
    /// Validate schema-specific invariants the rest of the pipeline relies on.
    fn validate(&self) -> Result<()> {
        if self.schema == SchemaType::BlogPosting {
            return Err(Error::InvalidSchemaType(self.schema, "an index page"));
        }
        Ok(())
    }
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
        frontmatter.validate()?;

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

    /// Extend the base context `ctx` with this index's fields for rendering.
    fn to_context(&self, mut ctx: Context, config: &Config, opts: &Cli) -> Result<Context> {
        let meta = &self.metadata;
        let fm = &meta.frontmatter;

        let description = fm
            .description
            .clone()
            .unwrap_or_else(|| config.site_info.description.clone());
        let schema_jsonld = json_ld::generate(
            fm.schema,
            &config.site_info,
            &fm.title,
            &description,
            &meta.canonical_url,
            None,
            None,
        );
        let breadcrumb_jsonld = json_ld::generate_breadcrumbs(&meta.breadcrumbs);

        ctx.insert("pages".to_string(), build_pages_list(&self.pages, opts));
        ctx.insert("title".to_string(), fm.title.clone().into());
        ctx.insert("content".to_string(), self.html.clone().into());
        ctx.insert(
            "canonical_url".to_string(),
            meta.canonical_url.clone().into(),
        );
        ctx.insert("og_type".to_string(), OgType::Website.to_string().into());
        ctx.insert("description".to_string(), description.into());
        ctx.insert("schema_jsonld".to_string(), schema_jsonld.into());
        ctx.insert("breadcrumb_jsonld".to_string(), breadcrumb_jsonld.into());
        Ok(ctx)
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
        self.clean_output_dir().await?;

        let mirror_handle = self.spawn_asset_mirror();
        let indices = self.load_indices().await?;

        let base_ctx = self.base_context(&indices);
        let robots_handle = self.spawn_render_to_output(&base_ctx, "robots.txt");
        let sitemap_handle = self.spawn_render_to_output(&base_ctx, "sitemap.xml");
        let feed_ctx = build_feed_context(&base_ctx, &indices, &self.config, opts);
        let rss_handle = self.spawn_render_to_output(&feed_ctx, "feed.xml");
        let atom_handle = self.spawn_render_to_output(&feed_ctx, "atom.xml");
        render_and_write_html(Arc::clone(&self.config), opts, base_ctx, indices).await?;

        mirror_handle.await.map_err(Error::Join)??;
        robots_handle.await.map_err(Error::Join)??;
        sitemap_handle.await.map_err(Error::Join)??;
        rss_handle.await.map_err(Error::Join)??;
        atom_handle.await.map_err(Error::Join)??;

        Ok(())
    }

    /// Remove the output directory.
    async fn clean_output_dir(&self) -> Result<()> {
        let to = self.config.output_path.clone();
        tokio::fs::remove_dir_all(&to)
            .await
            .or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(Error::OutputPathClean(to, e)),
            })
    }

    /// Mirror the `assets` directory into the output directory concurrently.
    fn spawn_asset_mirror(&self) -> tokio::task::JoinHandle<Result<()>> {
        let from = self.config.content_path.join("assets");
        let to = self.config.output_path.clone();
        tokio::spawn(async move { mirror_assets(from, to).await })
    }

    /// Load and parse the `content` directory.
    async fn load_indices(&self) -> Result<Vec<Index>> {
        let content_dir = self.config.content_path.join("content");
        let mut indices = load_and_parse_content(content_dir, Arc::clone(&self.config)).await?;
        populate_breadcrumbs(&mut indices);
        Ok(indices)
    }

    /// Build the base templating context shared by every rendered file.
    fn base_context(&self, indices: &[Index]) -> Context {
        let mut ctx = Context::new();
        ctx.insert(
            "site_title".to_string(),
            self.config.site_info.title.to_string().into(),
        );
        ctx.insert(
            "site_description".to_string(),
            self.config.site_info.description.to_string().into(),
        );
        ctx.insert(
            "site_base_url".to_string(),
            self.config.site_info.base_url.to_string().into(),
        );
        ctx.insert("indices".to_string(), build_indices_list(indices));
        ctx.insert("nav".to_string(), build_nav_list(indices));
        ctx
    }

    /// Render a top-level template (e.g. `robots.txt`, `sitemap.xml`) to the output
    /// directory concurrently.
    fn spawn_render_to_output(
        &self,
        base_ctx: &Context,
        name: &str,
    ) -> tokio::task::JoinHandle<Result<()>> {
        let config = Arc::clone(&self.config);
        let ctx = base_ctx.clone();
        let name = name.to_string();
        tokio::spawn(async move {
            debug!("Templating {name}");
            let template_path = config.content_path.join("templates").join(&name);
            let template = tokio::fs::read_to_string(&template_path)
                .await
                .map_err(|e| Error::ReadInput(template_path, e))?;
            let rendered = template::template(&config, ctx, template).await?;
            let out = config.output_path.join(&name);
            tokio::fs::write(&out, rendered)
                .await
                .map_err(|e| Error::WriteFile(out, e))?;
            Ok(())
        })
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
                .expect("Each file has a parent directory")
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
async fn render_and_write_html(
    config: Arc<Config>,
    opts: &Cli,
    base_ctx: Context,
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
        let ctx = index.to_context(base_ctx.clone(), &config, opts)?;

        // Apply templating
        let templates_dir = config.content_path.join("templates");
        let template_path = templates_dir.join(&index.metadata.frontmatter.template);
        debug!("Templating file {}", template_path.display());
        let template = tokio::fs::read_to_string(&template_path)
            .await
            .map_err(|e| Error::ReadInput(template_path, e))?;
        let html = template::template(&config, ctx, template).await?;

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
            let ctx = base_ctx.clone();
            let templates_dir = templates_dir.clone();

            handles.push(tokio::spawn(async move {
                debug!("Building page '{:?}'", &page.metadata);

                // Resolve output paths before the page is consumed into the context.
                let template_path = templates_dir.join(&page.metadata.frontmatter.template);
                let dir = config
                    .output_path
                    .join(
                        page.metadata
                            .filepath
                            .parent()
                            .expect("Each file has a parent directory"),
                    )
                    .join(&page.metadata.frontmatter.id);

                // Build page context
                let ctx = page.into_context(ctx, &config)?;

                // Apply templating
                let template = tokio::fs::read_to_string(&template_path)
                    .await
                    .map_err(|e| Error::ReadInput(template_path, e))?;
                let html = template::template(&config, ctx, template).await?;

                // Write page HTML to file
                tokio::fs::create_dir_all(&dir)
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

/// Build the sorted navigation list from indices and their nav-flagged pages.
fn build_nav_list(indices: &[Index]) -> TemplateValue {
    let mut navs: Vec<(usize, Context)> = Vec::new();

    for index in indices {
        if let Some(pos) = index.metadata.frontmatter.display_in_nav {
            let parent = index
                .metadata
                .filepath
                .parent()
                .expect("Each file has a parent directory");
            let url = if parent == Path::new("") {
                "/".to_string()
            } else {
                format!("/{}/", parent.display())
            };
            let mut item = Context::new();
            item.insert("url".to_string(), url.into());
            item.insert(
                "title".to_string(),
                index.metadata.frontmatter.title.clone().into(),
            );
            navs.push((pos, item));
        }
        for page in &index.pages {
            if let Some(pos) = page.metadata.frontmatter.display_in_nav {
                let parent = index.metadata.filepath.parent().unwrap();
                let url = if parent == Path::new("") {
                    format!("/{}/", page.metadata.frontmatter.id)
                } else {
                    format!("/{}/{}/", parent.display(), page.metadata.frontmatter.id)
                };
                let mut item = Context::new();
                item.insert("url".to_string(), url.into());
                item.insert(
                    "title".to_string(),
                    page.metadata.frontmatter.title.clone().into(),
                );
                navs.push((pos, item));
            }
        }
    }

    navs.sort_by_key(|(pos, _)| *pos);
    TemplateValue::List(navs.into_iter().map(|(_, item)| item).collect())
}

/// Build the pages list for a single index, suitable for template rendering.
///
/// Each item carries: `url`, `title`, and optionally `date`, `date_iso8601`, `excerpt`.
fn build_pages_list(pages: &[Page], opts: &Cli) -> TemplateValue {
    let items = pages
        .iter()
        .filter(|page| !page.metadata.frontmatter.draft || opts.drafts)
        .map(|page| {
            let parent = page
                .metadata
                .filepath
                .parent()
                .expect("Each file has a parent directory");
            let url = if parent == Path::new("") {
                format!("/{}/", page.metadata.frontmatter.id)
            } else {
                format!("/{}/{}/", parent.display(), page.metadata.frontmatter.id)
            };
            let mut item = Context::new();
            item.insert("url".to_string(), url.into());
            item.insert(
                "title".to_string(),
                page.metadata.frontmatter.title.clone().into(),
            );
            if let Some(ref excerpt) = page.metadata.frontmatter.excerpt {
                item.insert("excerpt".to_string(), excerpt.clone().into());
            }
            if let Some(date) = page.metadata.frontmatter.date {
                item.insert("date".to_string(), format_date_utc(&date).into());
                item.insert(
                    "date_iso8601".to_string(),
                    format_date_iso8601(&date).into(),
                );
            }
            item
        })
        .collect();
    TemplateValue::List(items)
}

/// Build the full indices list for sitemap rendering.
///
/// Each item carries: `url` and `pages` (a nested list with `url` per page).
fn build_indices_list(indices: &[Index]) -> TemplateValue {
    let items = indices
        .iter()
        .map(|index| {
            let pages = index
                .pages
                .iter()
                .map(|page| {
                    let mut item = Context::new();
                    item.insert(
                        "url".to_string(),
                        page.metadata.canonical_url.clone().into(),
                    );
                    item
                })
                .collect();
            let mut item = Context::new();
            item.insert(
                "url".to_string(),
                index.metadata.canonical_url.clone().into(),
            );
            item.insert("pages".to_string(), TemplateValue::List(pages));
            item
        })
        .collect();
    TemplateValue::List(items)
}

/// Build the templating context for the RSS and Atom feeds.
fn build_feed_context(
    base_ctx: &Context,
    indices: &[Index],
    config: &Config,
    opts: &Cli,
) -> Context {
    let mut articles: Vec<&Page> = indices
        .iter()
        .flat_map(|index| index.pages.iter())
        .filter(|page| page.metadata.frontmatter.schema == SchemaType::BlogPosting)
        .filter(|page| !page.metadata.frontmatter.draft || opts.drafts)
        .collect();
    articles.sort_by_key(|page| {
        std::cmp::Reverse(
            page.metadata
                .frontmatter
                .date
                .expect("A BlogPosting is dated"),
        )
    });

    let items = articles
        .iter()
        .map(|page| {
            let fm = &page.metadata.frontmatter;
            let mut item = Context::new();
            item.insert("title".to_string(), xml_escape(&fm.title).into());
            item.insert(
                "url".to_string(),
                xml_escape(&page.metadata.canonical_url).into(),
            );
            if let Some(summary) = fm.excerpt.as_deref().or(fm.description.as_deref()) {
                item.insert("description".to_string(), xml_escape(summary).into());
            }
            let content = absolutize_urls(&page.html, &config.site_info.base_url);
            item.insert("content".to_string(), xml_escape(&content).into());
            if let Some(author) = fm.author.as_deref() {
                item.insert("author".to_string(), xml_escape(author).into());
            }
            let date = fm.date.expect("A BlogPosting is dated");
            item.insert("date_rfc822".to_string(), format_date_rfc2822(&date).into());
            item.insert(
                "date_rfc3339".to_string(),
                format_date_rfc3339(&date).into(),
            );
            item
        })
        .collect();

    let mut ctx = base_ctx.clone();
    ctx.insert(
        "feed_title".to_string(),
        xml_escape(&config.site_info.title).into(),
    );
    ctx.insert(
        "feed_description".to_string(),
        xml_escape(&config.site_info.description).into(),
    );
    // Feed-level timestamp from the newest article (RFC 3339 for Atom, RFC 822 for RSS).
    if let Some(latest) = articles.first().map(|page| {
        page.metadata
            .frontmatter
            .date
            .expect("A BlogPosting is dated")
    }) {
        ctx.insert(
            "feed_updated".to_string(),
            format_date_rfc3339(&latest).into(),
        );
        ctx.insert(
            "feed_updated_rfc822".to_string(),
            format_date_rfc2822(&latest).into(),
        );
    }
    ctx.insert("feed_items".to_string(), TemplateValue::List(items));
    ctx
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

/// Format a date as RFC 822 / 2822, used for RSS `<pubDate>`/`<lastBuildDate>`.
fn format_date_rfc2822(date: &OffsetDateTime) -> String {
    date.format(&Rfc2822).expect("date already validated")
}

/// Format a date as RFC 3339, used for Atom `<updated>`/`<published>`.
fn format_date_rfc3339(date: &OffsetDateTime) -> String {
    date.format(&Rfc3339).expect("date already validated")
}

/// Escape to XML text or attribute context.
fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Rewrite root-relative `src`/`href` attributes in rendered HTML to absolute URLs
/// rooted at `base_url`.
fn absolutize_urls(html: &str, base_url: &str) -> String {
    html.replace("src=\"/", &format!("src=\"{base_url}/"))
        .replace("href=\"/", &format!("href=\"{base_url}/"))
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
