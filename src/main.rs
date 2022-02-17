use pulldown_cmark::{html, Options, Parser};
use std::io;
use std::path::PathBuf;
use std::{fs, path::Path};

#[derive(Debug, Clone)]
struct Page {
    id: String,
    title: String,
    content: String,
}

async fn parse_markdown(path: &Path) -> io::Result<Page> {
    let markdown_input = fs::read_to_string(path)?;

    let mut options = Options::empty();
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(&markdown_input, options);

    // Write to String buffer.
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    let page = Page {
        id: "test".to_string(),
        title: "Test Page".to_string(),
        content: html_output.trim_end().to_string(),
    };

    Ok(page)
}

async fn read_pages_template(pages_dir: &Path) -> io::Result<()> {
    let mut pages = Vec::new();
    for entry in fs::read_dir(pages_dir)? {
        let path = entry?.path();
        if path.is_file() {
            let html_output = parse_markdown(&path).await?;
            pages.push(html_output);
        }
    }

    // FIXME: Error handling if directory fails not because it already exists
    fs::create_dir("_site").ok();
    fs::copy("templates/style.css", "_site/style.css")?;
    let template_input = fs::read_to_string("templates/index.html")?;

    let mut sections_html = String::new();
    for page in &pages {
        sections_html += &format!("<section id=\"{}\">{}</section>\n", page.id, page.content);
    }
    let template_output = template_input.replace("%%% sections %%%", &sections_html);

    let nav_html = pages
        .iter()
        .map(|p| format!("<a href=\"#{}\">{}</a>\n", p.id, p.title))
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
