use std::{
    cmp::Ordering,
    collections::{hash_map, HashMap},
    os::unix::prelude::OsStrExt,
    path::{Path, PathBuf},
};

use log::{debug, error, info};
use pulldown_cmark::{html, Options, Parser};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PostMetadata {
    /// ID used for URLs.
    id: String,

    /// The path to the markdown input file.
    #[serde(skip_deserializing)]
    filepath: PathBuf,

    /// Template file to use.
    #[serde(default = "default_post_template")]
    template: PathBuf,

    /// Post title.
    title: String,

    /// Excerpt of the post content.
    excerpt: String,
    #[serde(with = "time::serde::rfc3339")]
    date: OffsetDateTime,
}

fn default_post_template() -> PathBuf {
    "post.html".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Post {
    metadata: PostMetadata,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PageMetadata {
    /// ID used for URLs.
    id: String,

    /// The path to the markdown input file.
    #[serde(skip_deserializing)]
    filepath: PathBuf,

    /// Template file to use.
    #[serde(default = "default_page_template")]
    template: PathBuf,

    /// Page title.
    title: String,

    /// Whether the page is the base `index.html`.
    #[serde(skip_deserializing)]
    is_index: bool,

    /// If the page should be shown in the navigation.
    #[serde(default)]
    hide: bool,
}

fn default_page_template() -> PathBuf {
    "page.html".into()
}

#[derive(Debug, Clone)]
struct Page {
    metadata: PageMetadata,
    content: String,
}

#[derive(Debug)]
struct Website {
    /// Configuration for this website.
    config: Config,

    /// Cache for all templates.
    ///
    /// This is to only load a template once from disk and store them in Memory
    template_cache: HashMap<PathBuf, String>,
}

impl Website {
    /// Create a new website.
    fn new(config: Config) -> Self {
        let template_cache = HashMap::new();
        Website {
            config,
            template_cache,
        }
    }

    /// Build the website to HTML content.
    async fn build(mut self) -> Result<()> {
        // Copy all assets
        let from = self.config.content_path.join("assets");
        let to = self.config.output_path.clone();
        let mirror_assets_handle = tokio::spawn(async move { mirror_assets(from, to).await });

        // Read and parse pages and posts
        let posts_dir = self.config.content_path.join("posts");
        let pages_dir = self.config.content_path.join("pages");
        let (posts, pages) = tokio::try_join!(
            tokio::spawn(async move { load_and_parse_posts(posts_dir).await }),
            tokio::spawn(async move { load_and_parse_pages(pages_dir).await }),
        )
        .map_err(Error::Join)?;
        let (posts, pages) = (posts?, pages?);

        // Store templates in a cache
        let templates_dir = self.config.content_path.join("templates");
        let templates_iter = posts
            .iter()
            .map(|post| post.metadata.template.clone())
            .chain(pages.iter().map(|page| page.metadata.template.clone()));
        for path in templates_iter {
            if let hash_map::Entry::Vacant(e) = self.template_cache.entry(path.clone()) {
                let full_path = templates_dir.join(&path);
                let template = tokio::fs::read_to_string(&full_path)
                    .await
                    .map_err(|e| Error::ReadInput(path, e))?;
                e.insert(template);
            }
        }

        // Fill templating context
        let mut ctx = template::Context::new();
        ctx.insert(
            "nav",
            pages
                .iter()
                .map(|p| {
                    if p.metadata.is_index {
                        format!("<a href=\"/\">{}</a>\n", p.metadata.title)
                    } else if !p.metadata.hide {
                        format!("<a href=\"/{}/\">{}</a>\n", p.metadata.id, p.metadata.title)
                    } else {
                        "".to_string()
                    }
                })
                .collect::<String>(),
        );
        ctx.insert(
            "articles",
            posts
                .iter()
                .map(|post| {
                    // Append current metadata as HTML to post TOC
                    format!(
                        "<hgroup>\n<h3><a href=\"/posts/{id}\">{title}</a></h3>\n<p><small><time \
                         datetime=\"{iso_date}\">{date}</time></small></p>\n</hgroup>\\
                         n<p>{excerpt}</p>\n",
                        id = post.metadata.id,
                        title = post.metadata.title,
                        excerpt = post.metadata.excerpt,
                        iso_date = format_date_iso8601(&post.metadata.date),
                        date = format_date_utc(&post.metadata.date),
                    )
                })
                .collect(),
        );
        ctx.insert("site_title", self.config.site_info.title.to_string());
        ctx.insert(
            "site_description",
            self.config.site_info.description.to_string(),
        );

        export_posts_to_html(&self.config, &mut ctx, &self.template_cache, posts).await?;
        export_pages_to_html(&self.config, &mut ctx, &self.template_cache, pages).await?;

        mirror_assets_handle.await.map_err(Error::Join)??;

        Ok(())
    }
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

async fn export_html_file(
    path: &Path,
    template: String,
    is_index: bool,
    cfg: &Config,
    ctx: &Context,
) -> Result<()> {
    // Apply templating
    let html = template::template(cfg, ctx, template).await?;

    if is_index {
        let path = cfg.output_path.join("index.html");
        tokio::fs::write(&path, html)
            .await
            .map_err(|e| Error::WriteFile(path, e))?;
    } else {
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|e| Error::CreateDirectory(path.to_path_buf(), e))?;
        let index_file_path = path.join("index.html");
        tokio::fs::write(&index_file_path, html)
            .await
            .map_err(|e| Error::WriteFile(index_file_path, e))?;
    }

    Ok(())
}

async fn export_pages_to_html(
    cfg: &Config,
    ctx: &mut Context,
    template_cache: &HashMap<PathBuf, String>,
    pages: Vec<Page>,
) -> Result<()> {
    let mut handles = Vec::new();

    for page in pages {
        debug!("Building page '{:?}'", &page.metadata);

        let cfg = cfg.clone();
        let mut ctx = ctx.clone();
        let template = template_cache
            .get(&page.metadata.template)
            .expect("templates are loaded")
            .clone();

        handles.push(tokio::spawn(async move {
            debug!("Building page '{:?}'", &page.metadata);

            // Insert page metadata into context for templating
            ctx.insert("title", page.metadata.title.to_string());
            ctx.insert("content", page.content.to_string());

            let path = cfg.output_path.join(&page.metadata.id);
            export_html_file(&path, template, page.metadata.is_index, &cfg, &ctx).await
        }));
    }

    // Wait for all processing to complete
    for task in handles {
        task.await.map_err(Error::Join)??;
    }

    Ok(())
}

async fn export_posts_to_html(
    cfg: &Config,
    ctx: &mut Context,
    template_cache: &HashMap<PathBuf, String>,
    posts: Vec<Post>,
) -> Result<()> {
    let mut handles = Vec::new();

    for post in posts {
        debug!("Building post '{:?}'", &post.metadata);

        let cfg = cfg.clone();
        let mut ctx = ctx.clone();
        let template = template_cache
            .get(&post.metadata.template)
            .expect("templates are loaded")
            .clone();

        handles.push(tokio::spawn(async move {
            // Insert metadata as current context for templating
            ctx.insert("content", post.content.to_string());
            ctx.insert("title", post.metadata.title.to_string());
            ctx.insert("excerpt", post.metadata.excerpt.to_string());
            ctx.insert("date_iso8601", format_date_iso8601(&post.metadata.date));
            ctx.insert("date", format_date_iso8601(&post.metadata.date));

            let path = cfg.output_path.join("posts").join(&post.metadata.id);
            export_html_file(&path, template, false, &cfg, &ctx).await
        }));
    }

    // Wait for all processing to complete
    for task in handles {
        task.await.map_err(Error::Join)??;
    }

    Ok(())
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

async fn convert_markdown(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(markdown, options);

    // Write to String buffer.
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    html_output
}

async fn parse_page(in_page: impl AsRef<Path>) -> Result<Page> {
    let in_page = in_page.as_ref();
    let input = tokio::fs::read_to_string(&in_page)
        .await
        .map_err(|e| Error::ReadInput(in_page.into(), e))?;

    let (frontmatter, markdown) = parse_file(&input, &in_page)?;
    let mut metadata: PageMetadata =
        toml::from_str(frontmatter).map_err(|e| Error::ParseMetadata(in_page.into(), e))?;
    metadata.is_index = in_page.file_name().expect("file must exist") == "_index.md";
    metadata.filepath = in_page.to_path_buf();

    let html = convert_markdown(markdown).await;
    let page = Page {
        metadata,
        content: html,
    };

    Ok(page)
}

async fn load_and_parse_pages(pages_dir: impl AsRef<Path>) -> Result<Vec<Page>> {
    let mut handles = vec![];
    let mut pages = vec![];

    let mut entries = tokio::fs::read_dir(&pages_dir)
        .await
        .map_err(|e| Error::ReadDirectory(pages_dir.as_ref().into(), e))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| Error::ReadDirectory(pages_dir.as_ref().into(), e))?
    {
        let path = entry.path();
        if path.is_file() {
            handles.push(tokio::spawn(async move { parse_page(&path).await }));
        }
    }

    // Wait for all pages to be done with processing
    for handle in handles {
        let page = handle.await.map_err(Error::Join)??;
        pages.push(page);
    }

    // Sort posts based on their date descending.
    pages.sort_unstable_by(|p1, p2| {
        let f1 = p1.metadata.filepath.file_name().expect("page has filename");
        let f2 = p2.metadata.filepath.file_name().expect("page has filename");
        // Sort '_' first
        match (f1.as_bytes(), f2.as_bytes()) {
            ([b'_', ..], _) => Ordering::Less,
            (_, [b'_', ..]) => Ordering::Greater,
            (f1, f2) => f1.cmp(f2),
        }
    });

    Ok(pages)
}

async fn parse_post(in_post: impl AsRef<Path>) -> Result<Post> {
    let in_post = in_post.as_ref();
    let input = tokio::fs::read_to_string(&in_post)
        .await
        .map_err(|e| Error::ReadInput(in_post.into(), e))?;

    let (frontmatter, markdown) = parse_file(&input, &in_post)?;
    let mut metadata: PostMetadata =
        toml::from_str(frontmatter).map_err(|e| Error::ParseMetadata(in_post.into(), e))?;
    metadata.filepath = in_post.to_path_buf();

    let html = convert_markdown(markdown).await;
    let post = Post {
        metadata,
        content: html,
    };

    Ok(post)
}

async fn load_and_parse_posts(posts_dir: impl AsRef<Path>) -> Result<Vec<Post>> {
    let mut handles = vec![];
    let mut posts = vec![];

    let mut entries = tokio::fs::read_dir(&posts_dir)
        .await
        .map_err(|e| Error::ReadDirectory(posts_dir.as_ref().into(), e))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| Error::ReadDirectory(posts_dir.as_ref().into(), e))?
    {
        let path = entry.path();
        if path.is_file() {
            handles.push(tokio::spawn(async move { parse_post(&path).await }));
        }
    }

    // all posts to be done with processing
    for handle in handles {
        let page = handle.await.map_err(Error::Join)??;
        posts.push(page);
    }

    // Sort posts based on their date descending.
    posts.sort_unstable_by(|p1, p2| p2.metadata.date.cmp(&p1.metadata.date));

    Ok(posts)
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
