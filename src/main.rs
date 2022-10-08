use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use log::{debug, error, info};
use pulldown_cmark::{html, Options, Parser};
use serde::{Deserialize, Serialize};
use time::format_description::FormatItem;
use time::macros::format_description;
use time::OffsetDateTime;

mod config;
mod error;
mod template;

use crate::config::Config;
use crate::error::{Error, Result};

/// Date format used to display dates.
const DATE_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]Z");

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PostMetadata {
    id: String,
    title: String,
    excerpt: String,
    #[serde(with = "time::serde::rfc3339")]
    date: OffsetDateTime,
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
    /// Page title.
    title: String,
    /// Whether the page is the base `index.html`.
    #[serde(skip_deserializing)]
    is_index: bool,
    /// If the page should be shown in the navigation.
    #[serde(default)]
    hide: bool,
}

#[derive(Debug, Clone)]
struct Page {
    metadata: PageMetadata,
    content: String,
}

#[derive(Debug)]
pub struct Website {
    /// Configuration for this website.
    pub config: Config,
}

impl Website {
    pub async fn build(&self) -> Result<()> {
        // Read pages.
        let pages_dir = self.config.content_path.join("pages");
        let pages = handle_pages(pages_dir).await?;

        // Read posts.
        let posts_dir = self.config.content_path.join("posts/");
        let posts = handle_posts(&posts_dir).await?;

        // Copy all assets
        let mirror_assets_handle = mirror_assets(
            self.config.content_path.join("assets"),
            self.config.output_path.clone(),
        );

        // Read template
        let html_template = self.config.content_path.join("templates/index.html");
        let template_input = tokio::fs::read_to_string(&html_template)
            .await
            .map_err(|e| Error::ReadInput(html_template, e))?;

        // Fill templating context
        let mut ctx = template::Context::new();
        let nav = pages
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
            .collect::<String>();
        ctx.insert("nav", nav);
        ctx.insert("site_title", self.config.site_info.title.to_string());
        ctx.insert(
            "site_description",
            self.config.site_info.description.to_string(),
        );

        // Create articles
        let mut html_articles = String::new();
        for post in &posts {
            debug!("Building post '{}'", &post.metadata.title);
            ctx.insert("content", post.content.to_string());
            ctx.insert("title", post.metadata.title.to_string());

            html_articles += &format!(
                "<h3><a href=\"/posts/{id}\">{title}</a></h3>\n<p>{excerpt}</p>\n<p><small>{date}</small></p>\n",
                id=post.metadata.id, title=post.metadata.title, excerpt=post.metadata.excerpt, date=post.metadata.date.format(&DATE_FORMAT).expect("valid date")
            );

            // Apply shortcode templating
            let html = template::template(&self.config, &ctx, template_input.clone()).await?;

            let dir_path = self
                .config
                .output_path
                .join("posts/")
                .join(&post.metadata.id);
            tokio::fs::create_dir_all(&dir_path)
                .await
                .map_err(|e| Error::CreateDirectory(dir_path.clone(), e))?;
            let index_file_path = dir_path.join("index.html");
            tokio::fs::write(index_file_path, html)
                .await
                .map_err(|e| Error::WriteFile(dir_path, e))?;
        }
        ctx.insert("articles", html_articles);

        for page in &pages {
            debug!("Building page '{}'", &page.metadata.title);
            ctx.insert("title", page.metadata.title.to_string());
            ctx.insert("content", page.content.to_string());

            // Apply shortcode templating
            let html = template::template(&self.config, &ctx, template_input.clone()).await?;

            if page.metadata.is_index {
                let path = self.config.output_path.join("index.html");
                tokio::fs::write(&path, html)
                    .await
                    .map_err(|e| Error::WriteFile(path, e))?;
            } else {
                let dir_path = self.config.output_path.join(&page.metadata.id);
                tokio::fs::create_dir_all(&dir_path)
                    .await
                    .map_err(|e| Error::CreateDirectory(dir_path.clone(), e))?;
                let index_file_path = dir_path.join("index.html");
                tokio::fs::write(&index_file_path, html)
                    .await
                    .map_err(|e| Error::WriteFile(index_file_path, e))?;
            }
        }

        mirror_assets_handle.await?;

        Ok(())
    }
}

/// Mirror the assets fully.
fn mirror_assets(from: PathBuf, to: PathBuf) -> Pin<Box<dyn Future<Output = Result<()>>>> {
    Box::pin(async move {
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
                tokio::fs::create_dir_all(new_to.clone())
                    .await
                    .map_err(|e| Error::CreateDirectory(new_to.clone(), e))?;
                mirror_assets(new_from, new_to).await?;
            } else if new_from.is_file() {
                tokio::fs::copy(&new_from, &new_to)
                    .await
                    .map_err(|e| Error::Copy(new_from, new_to, e))?;
            }
        }
        Ok(())
    })
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

    let html = convert_markdown(markdown).await;
    let page = Page {
        metadata,
        content: html,
    };

    Ok(page)
}

async fn handle_pages(pages_dir: impl AsRef<Path>) -> Result<Vec<Page>> {
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

    for handle in handles {
        let page = handle.await.map_err(Error::PageJoin)??;
        pages.push(page);
    }

    Ok(pages)
}

async fn parse_post(in_post: impl AsRef<Path>) -> Result<Post> {
    let in_page = in_post.as_ref();
    let input = tokio::fs::read_to_string(&in_page)
        .await
        .map_err(|e| Error::ReadInput(in_page.into(), e))?;
    let (frontmatter, markdown) = parse_file(&input, &in_page)?;
    let metadata: PostMetadata =
        toml::from_str(frontmatter).map_err(|e| Error::ParseMetadata(in_page.into(), e))?;

    let html = convert_markdown(markdown).await;
    let post = Post {
        metadata,
        content: html,
    };

    Ok(post)
}

async fn handle_posts(posts_dir: impl AsRef<Path>) -> Result<Vec<Post>> {
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

    for handle in handles {
        let page = handle.await.map_err(Error::PageJoin)??;
        posts.push(page);
    }

    // Sort posts based on their date descending.
    posts.sort_by(|p1, p2| p2.metadata.date.cmp(&p1.metadata.date));

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
    // Get website config
    let config_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "config.toml".into()),
    );
    let config = read_site_config(&config_path)
        .await
        .map_err(|e| Error::ConfigRead(config_path, e))?;

    // Build website.
    let website = Website { config };
    website.build().await
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let it = std::time::Instant::now();

    if let Err(e) = try_main().await {
        error!("{}", e);
        std::process::exit(1);
    }

    info!("Command took {:?}", it.elapsed());
}
