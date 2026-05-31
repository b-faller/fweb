use crate::Index;
use std::fmt::Write;

pub(crate) fn generate(indices: &[Index]) -> String {
    let mut sitemap = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
        <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n"
        .to_string();

    let write_url = |sitemap: &mut String, url: &str| {
        let _ = writeln!(sitemap, "  <url>\n    <loc>{}</loc>\n  </url>", url);
    };

    for index in indices {
        write_url(&mut sitemap, &index.metadata.canonical_url);
        for page in &index.pages {
            write_url(&mut sitemap, &page.metadata.canonical_url);
        }
    }

    sitemap.push_str("</urlset>\n");
    sitemap
}
