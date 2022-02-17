use pulldown_cmark::{html, Options, Parser};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use std::{fs, path::Path};

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

async fn parse_page(path: &Path) -> io::Result<Page> {
    let input = fs::read_to_string(path)?;
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

async fn read_pages_template(pages_dir: &Path) -> io::Result<()> {
    let mut pages = Vec::new();
    for entry in fs::read_dir(pages_dir)? {
        let path = entry?.path();
        if path.is_file() {
            let html_output = parse_page(&path).await?;
            pages.push(html_output);
        }
    }

    // FIXME: Error handling if directory fails not because it already exists
    fs::create_dir("_site").ok();
    fs::copy("templates/style.css", "_site/style.css")?;
    let template_input = fs::read_to_string("templates/index.html")?;

    let mut sections_html = String::new();
    for page in &pages {
        sections_html += &format!(
            "<section id=\"{}\">{}</section>\n",
            page.metadata.id, page.content
        );
    }
    let template_output = template_input.replace("%%% sections %%%", &sections_html);

    let nav_html = pages
        .iter()
        .map(|p| format!("<a href=\"#{}\">{}</a>\n", p.metadata.id, p.metadata.title))
        .collect::<String>();
    let template_output = template_output.replace("%%% nav %%%", &nav_html);
    fs::write("_site/index.html", template_output)?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pages_dir = PathBuf::from("pages");
    read_pages_template(&pages_dir).await?;
    Ok(())
}
