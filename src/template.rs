//! This module is responsible for replacing shortcodes from input files with
//! the appropriate data.

use std::{collections::HashMap, path::PathBuf, str::FromStr};

use log::debug;

use crate::{
    config::Config,
    error::{self, Error, Result},
};

/// Start delimiter of a shortcode.
///
/// This is used to detect a beginning shortcode as all shortcodes start with
/// this delimiter.
const SHORTCODE_START: char = '{';

/// Start delimiter of a command.
const COMMAND_START: &str = "{%";

/// End delimiter of a command.
const COMMAND_END: &str = "%}";

/// Start delimiter of a tag.
const TAG_START: &str = "{{";

/// End delimiter of a tag.
const TAG_END: &str = "}}";

/// Variable context for tags.
pub type Context = HashMap<&'static str, String>;

/// A information holder about a parsed shortcode.
#[derive(Debug, PartialEq, Eq)]
enum Shortcode {
    /// A shortcode with an include directive.
    ///
    /// If applied, it will include the contents given in the path.
    Include(PathBuf),

    /// A shortcode to insert with the given variable.
    Tag(String),
}

impl Shortcode {
    /// Applies the shortcode and converts it to HTML.
    async fn to_html(&self, config: &Config, ctx: &Context) -> Result<String> {
        match self {
            Shortcode::Include(path) => {
                let full_path = config.content_path.join("templates").join(path);
                debug!("Including file '{}'", path.display());
                tokio::fs::read_to_string(full_path)
                    .await
                    .map_err(|e| Error::IncludeShortcode(path.to_owned(), e))
            }
            Shortcode::Tag(var) => {
                debug!("Replacing tag '{}'", var);
                ctx.get(var.as_str())
                    .cloned()
                    .ok_or_else(|| Error::TagNotFound(var.to_string()))
            }
        }
    }
}

impl FromStr for Shortcode {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        let extract_command = |input: &str| -> Option<Self> {
            // {% include "stuff/head.html" %} -> include "stuff/head.html"
            let inner = input
                .strip_prefix(COMMAND_START)?
                .strip_suffix(COMMAND_END)?
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
        let extract_tag = |input: &str| -> Option<Self> {
            let inner = input.strip_prefix(TAG_START)?.strip_suffix(TAG_END)?.trim();
            Some(Self::Tag(inner.to_string()))
        };

        extract_tag(input)
            .or_else(|| extract_command(input))
            .ok_or_else(|| error::Error::ParseShortcode(input.to_string()))
    }
}

/// Find a shortcode within the given input.
///
/// This returns the start and end indices including the delimiters.
/// Essentially this is the range which gives the shortcut itself back from the
/// input:
///
/// ```rust
/// let (start, end) = find_shortcode(input);
/// let shortcode = &input[start..end];
/// ```
fn find_shortcode(input: &str) -> Option<(usize, usize)> {
    let mut search_start_idx = 0;

    // Find the first '{' char
    // This is a perf optimization as all shortcodes start with '{'
    while let Some(start) = input[search_start_idx..].find(SHORTCODE_START) {
        // Make start an absolute index
        let start_abs = search_start_idx + start;

        // Check the next char to determine type and find the end if it exists
        let end_abs = match &input[start_abs..] {
            s if s.starts_with(TAG_START) => s[TAG_START.len()..]
                .find(TAG_END)
                .map(|i| start_abs + i + TAG_START.len() + TAG_END.len()),
            s if s.starts_with(COMMAND_START) => s[COMMAND_START.len()..]
                .find(COMMAND_END)
                .map(|i| start_abs + i + COMMAND_START.len() + COMMAND_END.len()),
            _ => None,
        };

        // Check if we found a valid end
        match end_abs {
            Some(end_abs) => return Some((start_abs, end_abs)),
            None => search_start_idx = start_abs + 1,
        }
    }

    None
}

/// Apply shortcodes to the input template file.
pub async fn template(config: &Config, ctx: &Context, mut input: String) -> error::Result<String> {
    let mut html = String::new();

    while let Some((start, end)) = find_shortcode(&input) {
        // Parse shortcode
        let shortcode_str = &input[start..end];
        let shortcode: Shortcode = shortcode_str.parse()?;
        // Push all content before the found shortcode to the output HTML
        html.push_str(&input[..start]);
        // Push handled shortcode and remaining input to as todo to the new input since
        // there can be recursively nested shortcodes.
        input = shortcode.to_html(config, ctx).await? + &input[end..];
    }

    // Append the last part without a shortcode
    html.push_str(&input);

    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    fn dummy_config() -> Config {
        Config {
            site_info: config::SiteInfo {
                title: "".to_string(),
                description: "".to_string(),
            },
            content_path: "".into(),
            output_path: "".into(),
        }
    }

    #[test]
    fn test_find_shortcode_command() {
        let input = "{%%}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((0, 4), (start, end));
        assert_eq!(input, &input[start..end]);
    }

    #[test]
    fn test_find_shortcode_tag() {
        let input = "{{}}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((0, 4), (start, end));
        assert_eq!(input, &input[start..end]);
    }

    #[test]
    fn test_always_find_first_shortcode() {
        let input = "{{}}{%%}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((0, 4), (start, end));

        let input = "{%%}{{}}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((0, 4), (start, end));
    }

    #[test]
    fn test_shortcode_surrounded() {
        let input = "abcd{{ 1234 }}asdf";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((4, 14), (start, end));
    }

    #[test]
    fn test_shortcode_after_curly_braces() {
        let input = "{}{%%}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((2, 6), (start, end));

        let input = "{}hel{lo{% include \"test.html\" %}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((8, 33), (start, end));
    }

    #[test]
    fn test_shortcode_last() {
        let input = "test{";
        assert!(find_shortcode(input).is_none());
    }

    #[test]
    fn test_parse_include_shortcode() {
        let input = "{% include \"folder/head.html\" %}";
        let shortcode: Shortcode = input.parse().unwrap();
        assert_eq!(Shortcode::Include("folder/head.html".into()), shortcode);
    }

    #[tokio::test]
    async fn test_existing_tag() {
        let input = "{{ test }}";
        let shortcode: Shortcode = input.parse().unwrap();
        let ctx = Context::from_iter([("test", "value".to_string())]);
        assert_eq!(
            "value",
            shortcode.to_html(&dummy_config(), &ctx).await.unwrap()
        );
    }

    #[tokio::test]
    async fn test_nonexistant_tag() {
        let input = "{{ test }}";
        let shortcode: Shortcode = input.parse().unwrap();
        assert!(
            shortcode
                .to_html(&dummy_config(), &Context::new())
                .await
                .is_err()
        );
    }
}
