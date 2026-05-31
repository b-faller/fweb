use serde::Deserialize;

use crate::config::SiteInfo;

#[derive(Debug, Clone)]
pub(crate) struct Breadcrumb {
    pub(crate) name: String,
    pub(crate) url: String,
}

/// Schema.org type declared in page or index frontmatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(crate) enum SchemaType {
    WebSite,
    #[default]
    WebPage,
    Blog,
    BlogPosting,
}

impl std::fmt::Display for SchemaType {
    /// Renders the schema.org type name as it appears in JSON-LD `@type`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            SchemaType::WebSite => "WebSite",
            SchemaType::WebPage => "WebPage",
            SchemaType::Blog => "Blog",
            SchemaType::BlogPosting => "BlogPosting",
        };
        f.write_str(name)
    }
}

/// Generate a JSON-LD `<script>` tag from the given schema type and typed fields.
///
/// The caller is responsible for ensuring that `date` and `author` is present when a [SchemaType::BlogPosting] is chosen.
pub(crate) fn generate(
    schema: SchemaType,
    site: &SiteInfo,
    title: &str,
    description: &str,
    url: &str,
    date_iso8601: Option<&str>,
    author: Option<&str>,
) -> String {
    let json = match schema {
        SchemaType::WebSite => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "WebSite",
            "name": site.title,
            "description": site.description,
            "url": url,
        }),
        SchemaType::Blog => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "Blog",
            "name": title,
            "description": description,
            "url": url,
        }),
        SchemaType::WebPage => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "WebPage",
            "name": title,
            "description": description,
            "url": url,
        }),
        SchemaType::BlogPosting => {
            let date = date_iso8601.expect("A BlogPosting is dated");
            let author = author.expect("A BlogPosting has an author");
            serde_json::json!({
                "@context": "https://schema.org",
                "@type": "BlogPosting",
                "headline": title,
                "description": description,
                "url": url,
                "datePublished": date,
                "author": {"@type": "Person", "name": author},
            })
        }
    };

    format!("<script type=\"application/ld+json\">{json}</script>")
}

/// Generate a BreadcrumbList JSON-LD `<script>` tag.
///
/// Returns an empty string when `items` is empty.
pub(crate) fn generate_breadcrumbs(items: &[Breadcrumb]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let list: Vec<_> = items
        .iter()
        .enumerate()
        .map(|(i, bc)| {
            serde_json::json!({
                "@type": "ListItem",
                "position": i + 1,
                "name": bc.name,
                "item": bc.url,
            })
        })
        .collect();
    let json = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "BreadcrumbList",
        "itemListElement": list,
    });
    format!("<script type=\"application/ld+json\">{json}</script>")
}
