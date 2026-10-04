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

/// The page data used to build the JSON-LD.
#[derive(Debug, Default)]
pub(crate) struct PageFields<'a> {
    pub(crate) title: &'a str,
    pub(crate) description: &'a str,
    pub(crate) url: &'a str,
    pub(crate) date_iso8601: Option<&'a str>,
    pub(crate) updated_iso8601: Option<&'a str>,
    pub(crate) author: Option<&'a str>,
}

/// Generate a JSON-LD `<script>` tag from the given schema type and typed fields.
///
/// The caller is responsible for ensuring that `date` and `author` is present when a [SchemaType::BlogPosting] is chosen.
pub(crate) fn generate(schema: SchemaType, site: &SiteInfo, page: &PageFields) -> String {
    let json = match schema {
        SchemaType::WebSite => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "WebSite",
            "name": site.title,
            "description": site.description,
            "url": page.url,
        }),
        SchemaType::Blog => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "Blog",
            "name": page.title,
            "description": page.description,
            "url": page.url,
        }),
        SchemaType::WebPage => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "WebPage",
            "name": page.title,
            "description": page.description,
            "url": page.url,
        }),
        SchemaType::BlogPosting => {
            let date = page.date_iso8601.expect("A BlogPosting is dated");
            let author = page.author.expect("A BlogPosting has an author");
            let mut json = serde_json::json!({
                "@context": "https://schema.org",
                "@type": "BlogPosting",
                "headline": page.title,
                "description": page.description,
                "url": page.url,
                "datePublished": date,
                "author": {"@type": "Person", "name": author},
            });
            if let Some(updated) = page.updated_iso8601 {
                json["dateModified"] = updated.into();
            }
            json
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
