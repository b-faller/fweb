use serde::Deserialize;

use crate::{
    error::{Error, Result},
    template::Context,
};

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
    let json = match schema {
        SchemaType::WebSite => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "WebSite",
            "name": ctx["site_title"],
            "description": ctx["site_description"],
            "url": ctx["canonical_url"],
        }),
        SchemaType::Blog => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "Blog",
            "name": ctx["title"],
            "description": ctx["description"],
            "url": ctx["canonical_url"],
        }),
        SchemaType::WebPage => serde_json::json!({
            "@context": "https://schema.org",
            "@type": "WebPage",
            "name": ctx["title"],
            "description": ctx["description"],
            "url": ctx["canonical_url"],
        }),
        SchemaType::BlogPosting => {
            let date = ctx
                .get("date_iso8601")
                .ok_or(Error::MissingSchemaField("date", "BlogPosting"))?;
            let author = ctx
                .get("author")
                .ok_or(Error::MissingSchemaField("author", "BlogPosting"))?;
            serde_json::json!({
                "@context": "https://schema.org",
                "@type": "BlogPosting",
                "headline": ctx["title"],
                "description": ctx["description"],
                "url": ctx["canonical_url"],
                "datePublished": date,
                "author": {"@type": "Person", "name": author},
            })
        }
    };

    Ok(format!(
        "<script type=\"application/ld+json\">{json}</script>"
    ))
}
