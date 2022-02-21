use pulldown_cmark::{html, Options, Parser};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::OffsetDateTime;

mod config;

use crate::config::Config;

const DATE_FORMAT: &'static [FormatItem<'static>] =
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

/// Extract frontmatter and markdown from a input file.
fn parse_file(input: &str) -> (&str, &str) {
    let mut split = input.splitn(3, "+++");
    // Empty before frontmatter
    split.next();
    let frontmatter = split.next().unwrap();
    let markdown = split.next().unwrap().trim();
    (frontmatter, markdown)
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

async fn parse_page(in_page: impl AsRef<Path>) -> io::Result<Page> {
    let input = tokio::fs::read_to_string(&in_page).await?;
    let (frontmatter, markdown) = parse_file(&input);
    let mut metadata: PageMetadata = toml::from_str(frontmatter)?;
    metadata.is_index = if in_page.as_ref().file_name().expect("file must exist") == "_index.md" {
        true
    } else {
        false
    };

    let html = convert_markdown(markdown).await;
    let page = Page {
        metadata,
        content: html.trim_end().to_string(),
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

async fn parse_post(in_post: impl AsRef<Path>) -> io::Result<Post> {
    let input = tokio::fs::read_to_string(&in_post).await?;
    let mut split = input.splitn(3, "+++");

    // Empty before frontmatter
    split.next();
    let frontmatter = split.next().unwrap();
    let markdown = split.next().unwrap().trim();

    let metadata: PostMetadata = toml::from_str(frontmatter)?;

    let mut options = Options::empty();
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(markdown, options);

    // Write to String buffer.
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    let post = Post {
        metadata,
        content: html_output.trim_end().to_string(),
    };

    Ok(post)
}

async fn handle_posts(posts_dir: impl AsRef<Path>) -> io::Result<Vec<Post>> {
    let mut handles = vec![];
    let mut posts = vec![];

    let mut entries = tokio::fs::read_dir(posts_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_file() {
            handles.push(tokio::spawn(async move { parse_post(&path).await }));
        }
    }

    for handle in handles {
        let page = handle.await??;

        posts.push(page);
    }

    Ok(posts)
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
            if p.metadata.is_index {
                format!("<a href=\"/\">{}</a>\n", p.metadata.title)
            } else if !p.metadata.hide {
                format!("<a href=\"/{}/\">{}</a>\n", p.metadata.id, p.metadata.title)
            } else {
                "".to_string()
            }
        })
        .collect::<String>();
    let html = template_input.replace("%%% nav %%%", &nav_html);
    let html = html.replace("%%% site_title %%%", &cfg.site.title);
    let html = html.replace("%%% site_description %%%", &cfg.site.description);

    let posts = handle_posts("posts").await?;
    let mut html_articles = "".to_string();
    for post in &posts {
        let html = html.replace("%%% content %%%", &post.content);
        let html_output = html.replace("%%% title %%%", &post.metadata.title);
        html_articles += &format!(
            "<h3><a href=\"/posts/{id}\">{title}</a></h3>\n<p>{excerpt}</p>\n<p><small>{date}</small></p>\n",
            id=post.metadata.id, title=post.metadata.title, excerpt=post.metadata.excerpt, date=post.metadata.date.format(&DATE_FORMAT).expect("valid date")
        );

        let dir_path = cfg.output_path.join("posts/").join(&post.metadata.id);
        tokio::fs::create_dir_all(&dir_path).await?;
        let index_file_path = dir_path.join("index.html");
        tokio::fs::write(index_file_path, html_output).await?;
    }

    for page in &pages {
        let html = html.replace("%%% content %%%", &page.content);
        let html = html.replace("%%% articles %%%", &html_articles);
        let html_output = html.replace("%%% title %%%", &page.metadata.title);

        if page.metadata.is_index {
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
