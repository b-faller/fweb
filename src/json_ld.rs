use serde::Deserialize;

use crate::{
    error::{Error, Result},
    template::Context,
};

#[derive(Debug, Clone)]
pub(crate) struct Breadcrumb {
    pub(crate) name: String,
    pub(crate) url: String,
}

/// Schema.org type declared in page or index frontmatter.
#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(crate) enum SchemaType {
    WebSite,
    #[default]
    WebPage,
    Blog,
    BlogPosting,
}

/// Generate a JSON-LD `<script>` tag from the given schema type and template context.
///
/// Reads values already present in the template context. Returns an error if
/// a required field for the declared schema type is absent from the context.
pub(crate) fn generate(schema: SchemaType, ctx: &Context) -> Result<String> {
    let get_scalar = |key| ctx.get(key).and_then(|v| v.as_scalar());

    let json = match schema {
        SchemaType::WebSite => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "WebSite",
            "name": get_scalar("site_title"),
            "description": get_scalar("site_description"),
            "url": get_scalar("canonical_url"),
        }),
        SchemaType::Blog => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "Blog",
            "name": get_scalar("title"),
            "description": get_scalar("description"),
            "url": get_scalar("canonical_url"),
        }),
        SchemaType::WebPage => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "WebPage",
            "name": get_scalar("title"),
            "description": get_scalar("description"),
            "url": get_scalar("canonical_url"),
        }),
        SchemaType::BlogPosting => {
            let date = get_scalar("date_iso8601")
                .ok_or(Error::MissingSchemaField("date", "BlogPosting"))?;
            let author =
                get_scalar("author").ok_or(Error::MissingSchemaField("author", "BlogPosting"))?;
            serde_json::json!({
                "@context": "https://schema.org",
                "@type": "BlogPosting",
                "headline": get_scalar("title"),
                "description": get_scalar("description"),
                "url": get_scalar("canonical_url"),
                "datePublished": date,
                "author": {"@type": "Person", "name": author},
            })
        }
    };

    Ok(format!(
        "<script type=\"application/ld+json\">{json}</script>"
    ))
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
