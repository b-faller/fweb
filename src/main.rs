use pulldown_cmark::{html, Options, Parser};
use std::io;
use std::path::PathBuf;
use std::{fs, path::Path};

async fn parse_markdown(path: &Path) {
    let markdown_input = fs::read_to_string(path).unwrap();

    let mut options = Options::empty();
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(&markdown_input, options);

    // Write to String buffer.
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    let html_output = html_output.trim_end();
    println!("{}", html_output);
}

async fn read_pages_template(pages_dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(pages_dir)? {
        let path = entry?.path();
        if path.is_file() {
            parse_markdown(&path).await;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pages_dir = PathBuf::from("pages");
    read_pages_template(&pages_dir).await?;
    Ok(())
}
