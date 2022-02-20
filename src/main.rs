use pulldown_cmark::{html, Options, Parser};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

mod config;

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PostMetadata {
    title: String,
    date: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Post {
    metadata: PostMetadata,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PageMetadata {
    id: String,
    title: String,
}

#[derive(Debug, Clone)]
struct Page {
    metadata: PageMetadata,
    content: String,
}

async fn parse_page(in_page: impl AsRef<Path>) -> io::Result<Page> {
    let input = tokio::fs::read_to_string(in_page).await?;
    let mut split = input.splitn(3, "+++");

    // Empty before frontmatter
    split.next();
    let frontmatter = split.next().unwrap();
    let markdown = split.next().unwrap().trim();

    let metadata = toml::from_str(frontmatter)?;

    let mut options = Options::empty();
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(markdown, options);

    // Write to String buffer.
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    let page = Page {
        metadata,
        content: html_output.trim_end().to_string(),
    };

    Ok(page)
}

async fn handle_pages(pages_dir: impl AsRef<Path>) -> io::Result<Vec<Page>> {
    let mut handles = vec![];
    let mut pages = vec![];

    let mut entries = tokio::fs::read_dir(pages_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_file() {
            handles.push(tokio::spawn(async move { parse_page(&path).await }));
        }
    }

    for handle in handles {
        let page = handle.await??;

        pages.push(page);
    }

    Ok(pages)
}

async fn read_pages_template(pages_dir: impl AsRef<Path>, cfg: &Config) -> io::Result<()> {
    let pages = handle_pages(pages_dir).await?;

    // FIXME: Error handling if directory fails not because it already exists
    tokio::fs::create_dir("_site").await.ok();
    tokio::fs::copy("templates/style.css", "_site/style.css").await?;
    let template_input = tokio::fs::read_to_string("templates/index.html").await?;

    // Templating
    let nav_html = pages
        .iter()
        .map(|p| {
            if p.metadata.id == "index" {
                format!("<a href=\"/\">{}</a>\n", p.metadata.title)
            } else {
                format!("<a href=\"/{}/\">{}</a>\n", p.metadata.id, p.metadata.title)
            }
        })
        .collect::<String>();
    let html = template_input.replace("%%% nav %%%", &nav_html);
    let html = html.replace("%%% site_title %%%", &cfg.site.title);
    let html = html.replace("%%% site_description %%%", &cfg.site.description);

    for page in &pages {
        let html_output = html.replace("%%% content %%%", &page.content);

        if page.metadata.id == "index" {
            tokio::fs::write("_site/index.html", html_output).await?;
        } else {
            let dir_path = cfg.output_path.join(&page.metadata.id);
            tokio::fs::create_dir_all(&dir_path).await?;
            let index_file_path = dir_path.join("index.html");
            tokio::fs::write(index_file_path, html_output).await?;
        }
    }

    Ok(())
}

async fn read_config(path: impl AsRef<Path>) -> io::Result<Config> {
    let content = tokio::fs::read_to_string(path).await?;
    let config = toml::from_str(&content)?;
    Ok(config)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let it = std::time::Instant::now();

    let config = read_config("config.toml").await?;
    read_pages_template("pages", &config).await?;

    print!("{:?}\n", it.elapsed());

    Ok(())
}
