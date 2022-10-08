//! This module is responsible for replacing shortcodes from input files with the appropriate data.

use std::{path::PathBuf, str::FromStr};

use log::debug;

use crate::{
    config::Config,
    error::{self, Error, Result},
};

/// Start delimiter of a shortcode
const SHORTCODE_START: &str = "{%";

/// End delimiter of a shortcode
const SHORTCODE_END: &str = "%}";

/// A information holder about a parsed shortcode.
#[derive(Debug, PartialEq, Eq)]
enum Shortcode {
    /// A shortcode with an include directive.
    ///
    /// If applied, it will include the contents given in the path.
    Include(PathBuf),
}

impl Shortcode {
    /// Applies the shortcode and converts it to HTML.
    async fn to_html(&self, config: &Config) -> Result<String> {
        match self {
            Shortcode::Include(path) => {
                let full_path = config.content_path.join("templates").join(path);
                debug!("Including file '{}'", path.display());
                tokio::fs::read_to_string(full_path)
                    .await
                    .map_err(|e| Error::IncludeShortcode(path.to_owned(), e))
            }
        }
    }
}

impl FromStr for Shortcode {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        let parse_inner = |input: &str| {
            // {% include "stuff/head.html" %} -> include "stuff/head.html"
            let inner = input
                .strip_prefix(SHORTCODE_START)?
                .strip_suffix(SHORTCODE_END)?
                .trim();
            // include "stuff/head.html" -> "stuff/head.html"
            let quoted_path = inner.strip_prefix("include")?.trim_start();
            // stuff/head.html
            let path: PathBuf = quoted_path
                .strip_prefix('"')?
                .strip_suffix('"')?
                .parse()
                .ok()?;
            Some(Self::Include(path))
        };
        parse_inner(input).ok_or_else(|| error::Error::ParseShortcode(input.to_string()))
    }
}

/// Find a shortcode within the given input.
///
/// This returns the start and end indices including [SHORTCODE_START] and [SHORTCODE_END].
/// Essentially this is the range which gives the shortcut itself back from the input:
///
/// ```rust
/// let (start, end) = find_shortcode(input);
/// let shortcode = &input[start..end];
/// ```
fn find_shortcode(input: &str) -> Option<(usize, usize)> {
    let start = input.find(SHORTCODE_START)?;
    let end = input[start..].find(SHORTCODE_END)?;
    Some((start, start + end + SHORTCODE_END.len()))
}

/// Apply shortcodes to the input template file.
pub async fn template(config: &Config, mut input: String) -> error::Result<String> {
    let mut html = String::new();

    while let Some((start, end)) = find_shortcode(&input) {
        // Parse shortcode
        let shortcode_str = &input[start..end];
        let shortcode: Shortcode = shortcode_str.parse()?;
        // Push all content before the found shortcode to the output HTML
        html.push_str(&input[..start]);
        // Push handled shortcode and remaining input to as todo to the new input since
        // there can be recursively nested shortcodes.
        input = shortcode.to_html(config).await? + &input[end..];
    }

    // Append the last part without a shortcode
    html.push_str(&input);

    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_shortcode() {
        let input = "{%%}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((0, 4), (start, end));
        assert_eq!(input, &input[start..end]);
    }

    #[test]
    fn test_parse_include_shortcode() {
        let input = "{% include \"folder/head.html\" %}";
        let shortcode: Shortcode = input.parse().unwrap();
        assert_eq!(Shortcode::Include("folder/head.html".into()), shortcode);
    }
}
