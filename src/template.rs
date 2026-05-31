//! This module is responsible for replacing shortcodes from input files with
//! the appropriate data.

use std::{collections::HashMap, future::Future, path::Path, pin::Pin};

use log::debug;

use crate::{
    config::Config,
    error::{self, Error},
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

/// Block keyword: if condition.
const KW_IF: &str = "if";

/// Block keyword: else branch.
const KW_ELSE: &str = "else";

/// Block keyword: end of if block.
const KW_ENDIF: &str = "endif";

/// Block keyword: for loop.
const KW_FOR: &str = "for";

/// Block keyword: end of for block.
const KW_ENDFOR: &str = "endfor";

/// Block keyword: logical negation in if conditions.
const KW_NOT: &str = "not";

/// Separator between loop variable and iterable in for tags.
const KW_IN: &str = " in ";

/// Command keyword for file inclusion.
const KW_INCLUDE: &str = "include";

/// A value held in the template context.
#[derive(Debug, Clone)]
pub enum TemplateValue {
    Scalar(String),
    Object(Context),
    List(Vec<Context>),
}

impl From<&'static str> for TemplateValue {
    fn from(s: &'static str) -> Self {
        TemplateValue::Scalar(s.to_string())
    }
}

impl From<String> for TemplateValue {
    fn from(s: String) -> Self {
        TemplateValue::Scalar(s)
    }
}

impl TemplateValue {
    pub fn is_truthy(&self) -> bool {
        match self {
            TemplateValue::Scalar(s) => !s.is_empty(),
            TemplateValue::Object(_) => true,
            TemplateValue::List(v) => !v.is_empty(),
        }
    }

    pub fn as_scalar(&self) -> Option<&str> {
        match self {
            TemplateValue::Scalar(s) => Some(s),
            _ => None,
        }
    }
}

pub type Context = HashMap<String, TemplateValue>;

/// Recursively looks up a (possibly dotted) key in a flat Context.
fn get_in_ctx<'a>(ctx: &'a Context, key: &str) -> Option<&'a TemplateValue> {
    if let Some((prefix, suffix)) = key.split_once('.') {
        ctx.get(prefix).and_then(|v| match v {
            TemplateValue::Object(inner) => get_in_ctx(inner, suffix),
            _ => None,
        })
    } else {
        ctx.get(key)
    }
}

/// Variable context for template rendering.
#[derive(Debug, Clone, Default)]
struct Stack {
    /// This holds the stack frames.
    frames: Vec<Context>,
}

impl Stack {
    pub fn new(ctx: Context) -> Self {
        Self { frames: vec![ctx] }
    }

    pub fn insert(&mut self, key: String, value: TemplateValue) -> Option<TemplateValue> {
        self.frames
            .last_mut()
            .expect("Always one stack frame is present")
            .insert(key, value)
    }

    pub fn get(&self, key: &str) -> Option<&TemplateValue> {
        if let Some((prefix, suffix)) = key.split_once('.') {
            self.frames.iter().rev().find_map(|m| {
                m.get(prefix).and_then(|v| match v {
                    TemplateValue::Object(ctx) => get_in_ctx(ctx, suffix),
                    _ => None,
                })
            })
        } else {
            self.frames.iter().rev().find_map(|m| m.get(key))
        }
    }

    fn push_frame(&mut self) {
        self.frames.push(HashMap::default());
    }

    fn pop_frame(&mut self) -> Option<Context> {
        if self.frames.len() == 1 {
            return None;
        }
        self.frames.pop()
    }
    /// Returns true if `TemplateValue` evaluates to true
    pub fn is_truthy(&self, key: &str) -> bool {
        self.get(key).is_some_and(|v| v.is_truthy())
    }
}

impl FromIterator<(&'static str, TemplateValue)> for Stack {
    fn from_iter<T: IntoIterator<Item = (&'static str, TemplateValue)>>(iter: T) -> Self {
        Self {
            frames: vec![iter.into_iter().map(|(k, v)| (k.to_string(), v)).collect()],
        }
    }
}

/// A template expression (AST node). All `&'a` fields are zero-copy slices of the parsed input.
#[derive(Debug, PartialEq)]
pub enum Expr<'a> {
    /// Literal text passed through unchanged.
    Text(&'a str),

    /// Variable substitution: `{{ var }}`.
    Tag(&'a str),

    /// File inclusion: `{% include "path" %}`.
    Include(&'a Path),

    /// Conditional block: `{% if [not] condition %}...{% else %}...{% endif %}`.
    If {
        negated: bool,
        condition: &'a str,
        then_body: Vec<Expr<'a>>,
        else_body: Vec<Expr<'a>>,
    },

    /// Loop block: `{% for var in list %}...{% endfor %}`.
    For {
        var: &'a str,
        list_var: &'a str,
        body: Vec<Expr<'a>>,
    },
}

impl<'a> Expr<'a> {
    /// Parse a template string into an expression tree.
    ///
    /// All slice fields in the returned nodes point into `input`.
    pub fn parse(input: &'a str) -> error::Result<Vec<Expr<'a>>> {
        let mut exprs = Vec::new();
        let mut rest = input;

        loop {
            let Some((start, tag_end)) = find_shortcode(rest) else {
                if !rest.is_empty() {
                    exprs.push(Expr::Text(rest));
                }
                return Ok(exprs);
            };

            if start > 0 {
                exprs.push(Expr::Text(&rest[..start]));
            }

            let tag = &rest[start..tag_end];

            // {{ var }}
            if let Some(inner) = tag
                .strip_prefix(TAG_START)
                .and_then(|s| s.strip_suffix(TAG_END))
            {
                exprs.push(Expr::Tag(inner.trim()));
                rest = &rest[tag_end..];
                continue;
            }

            // {% … %} — extract and trim the inner command string
            let inner = tag
                .strip_prefix(COMMAND_START)
                .and_then(|s| s.strip_suffix(COMMAND_END))
                .map(str::trim)
                .ok_or_else(|| Error::ParseShortcode(tag.to_string()))?;

            // Split on the first space to get the keyword and its arguments.
            let (kw, args) = inner
                .split_once(' ')
                .map_or((inner, ""), |(k, a)| (k, a.trim_start()));

            match kw {
                KW_INCLUDE => {
                    // include "path/to/file" -> "path/to/file"
                    let quoted = args;
                    // path/to/file
                    let path = quoted
                        .strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .ok_or_else(|| Error::ParseShortcode(tag.to_string()))?;
                    exprs.push(Expr::Include(Path::new(path)));
                    rest = &rest[tag_end..];
                }
                KW_IF => {
                    // if [not] condition
                    let (negated, condition) = if let Some(cond) =
                        args.strip_prefix(KW_NOT).and_then(|s| s.strip_prefix(' '))
                    {
                        (true, cond.trim())
                    } else {
                        (false, args)
                    };
                    let after_tag = &rest[tag_end..];
                    let (close_start, close_end) = Self::find_block_end(after_tag, KW_IF, KW_ENDIF)
                        .ok_or_else(|| Error::UnterminatedBlock(KW_IF.to_string()))?;
                    let body_str = &after_tag[..close_start];
                    let (then_str, else_str) = Self::split_else(body_str);
                    exprs.push(Expr::If {
                        negated,
                        condition,
                        then_body: Self::parse(then_str)?,
                        else_body: match else_str {
                            Some(s) => Self::parse(s)?,
                            None => vec![],
                        },
                    });
                    rest = &rest[tag_end + close_end..];
                }
                KW_FOR => {
                    // for var in list
                    let (var, list) = args
                        .split_once(KW_IN)
                        .ok_or_else(|| Error::ParseShortcode(tag.to_string()))?;
                    let after_tag = &rest[tag_end..];
                    let (close_start, close_end) =
                        Self::find_block_end(after_tag, KW_FOR, KW_ENDFOR)
                            .ok_or_else(|| Error::UnterminatedBlock(KW_FOR.to_string()))?;
                    exprs.push(Expr::For {
                        var: var.trim(),
                        list_var: list.trim(),
                        body: Self::parse(&after_tag[..close_start])?,
                    });
                    rest = &rest[tag_end + close_end..];
                }
                _ => return Err(Error::ParseShortcode(tag.to_string())),
            }
        }
    }

    /// Finds the closing `{% end<kw> %}` matching the open keyword, accounting for nesting.
    ///
    /// Returns `(close_start, close_end)` as byte offsets into `input`.
    fn find_block_end(input: &str, open_kw: &str, close_tag: &str) -> Option<(usize, usize)> {
        let mut depth: usize = 0;
        let mut pos = 0;

        while pos < input.len() {
            // Scan for next `{%`
            let Some(rel) = input[pos..].find(COMMAND_START) else {
                break;
            };
            let tag_start = pos + rel;

            let after_open = tag_start + COMMAND_START.len();
            let Some(end_rel) = input[after_open..].find(COMMAND_END) else {
                break;
            };
            let tag_end = after_open + end_rel + COMMAND_END.len();

            let inner = input[after_open..after_open + end_rel].trim();

            if starts_kw(inner, open_kw) {
                depth += 1;
            } else if inner == close_tag {
                if depth == 0 {
                    return Some((tag_start, tag_end));
                }
                depth -= 1;
            }

            pos = tag_end;
        }

        None
    }

    /// Splits a block body on a top-level `{% else %}`, returning `(then, else_opt)`.
    fn split_else(body: &str) -> (&str, Option<&str>) {
        let mut depth: usize = 0;
        let mut pos = 0;

        while pos < body.len() {
            let Some(rel) = body[pos..].find(COMMAND_START) else {
                break;
            };
            let tag_start = pos + rel;
            let after_open = tag_start + COMMAND_START.len();
            let Some(end_rel) = body[after_open..].find(COMMAND_END) else {
                break;
            };
            let tag_end = after_open + end_rel + COMMAND_END.len();
            let inner = body[after_open..after_open + end_rel].trim();

            // Track nesting: if/for open depth, endif/endfor close depth.
            if starts_kw(inner, KW_IF) || starts_kw(inner, KW_FOR) {
                depth += 1;
            } else if inner == KW_ENDIF || inner == KW_ENDFOR {
                depth = depth.saturating_sub(1);
            } else if inner == KW_ELSE && depth == 0 {
                return (&body[..tag_start], Some(&body[tag_end..]));
            }

            pos = tag_end;
        }

        (body, None)
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
///
/// Parses `input` into an expression tree, then evaluates it with `config` and `stack`.
pub async fn template(
    config: &Config,
    ctx: Context,
    input: impl AsRef<str>,
) -> error::Result<String> {
    let mut stack = Stack::new(ctx);
    eval(&Expr::parse(input.as_ref())?, config, &mut stack).await
}

/// Evaluates an expression tree to a string.
fn eval<'a>(
    exprs: &'a [Expr<'a>],
    config: &'a Config,
    stack: &'a mut Stack,
) -> Pin<Box<dyn Future<Output = error::Result<String>> + Send + 'a>> {
    Box::pin(async move {
        let mut out = String::new();
        for expr in exprs {
            match expr {
                Expr::Text(s) => out.push_str(s),
                Expr::Tag(key) => {
                    debug!("Replacing tag '{}'", key);
                    out.push_str(
                        stack
                            .get(key)
                            .and_then(TemplateValue::as_scalar)
                            .ok_or_else(|| Error::TagNotFound((*key).to_string()))?,
                    );
                }
                Expr::Include(path) => {
                    let full_path = config.content_path.join("templates").join(path);
                    debug!("Including file '{}'", path.display());
                    let content = tokio::fs::read_to_string(&full_path)
                        .await
                        .map_err(|e| Error::IncludeShortcode(path.to_path_buf(), e))?;
                    let sub = Expr::parse(&content)?;
                    out += &eval(&sub, config, stack).await?;
                }
                Expr::If {
                    negated,
                    condition,
                    then_body,
                    else_body,
                } => {
                    let truthy = stack.is_truthy(condition);
                    let branch = if truthy != *negated {
                        then_body
                    } else {
                        else_body
                    };
                    out += &eval(branch, config, stack).await?;
                }
                Expr::For {
                    var,
                    list_var: list,
                    body,
                } => {
                    if let Some(TemplateValue::List(items)) = stack.get(list) {
                        let items = items.clone();
                        for item in items {
                            stack.push_frame();
                            stack.insert((*var).to_string(), TemplateValue::Object(item));
                            out += &eval(body, config, stack).await?;
                            stack.pop_frame();
                        }
                    }
                }
            }
        }
        Ok(out)
    })
}

/// Returns true if `s` equals `kw` or starts with `kw` followed by a space.
fn starts_kw(s: &str, kw: &str) -> bool {
    s.strip_prefix(kw)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    fn make_ctx(pairs: impl IntoIterator<Item = (&'static str, TemplateValue)>) -> Context {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn dummy_config() -> Config {
        Config {
            site_info: config::SiteInfo {
                title: "".to_string(),
                description: "".to_string(),
                base_url: "".to_string(),
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
    fn test_find_shortcode_finds_first() {
        let input = "{{}}{%%}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((0, 4), (start, end));

        let input = "{%%}{{}}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((0, 4), (start, end));
    }

    #[test]
    fn test_find_shortcode_surrounded_by_text() {
        let input = "abcd{{ 1234 }}asdf";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((4, 14), (start, end));
    }

    #[test]
    fn test_find_shortcode_after_lone_brace() {
        let input = "{}{%%}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((2, 6), (start, end));

        let input = "{}hel{lo{% include \"test.html\" %}";
        let (start, end) = find_shortcode(input).unwrap();
        assert_eq!((8, 33), (start, end));
    }

    #[test]
    fn test_find_shortcode_none_on_unclosed() {
        assert!(find_shortcode("test{").is_none());
    }

    #[test]
    fn test_expr_parse_include() {
        let input = "{% include \"folder/head.html\" %}";
        let exprs = Expr::parse(input).unwrap();
        assert_eq!(exprs, vec![Expr::Include(Path::new("folder/head.html"))]);
    }

    #[tokio::test]
    async fn test_tag_existing() {
        let ctx = make_ctx([("test", "value".into())]);
        assert_eq!(
            "value",
            template(&dummy_config(), ctx,"{{ test }}")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn test_tag_missing() {
        assert!(template(&dummy_config(), Context::new(), "{{ test }}")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_if_truthy() {
        let ctx = make_ctx([("show", "yes".into())]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% if show %}visible{% endif %}",
        )
        .await
        .unwrap();
        assert_eq!("visible", out);
    }

    #[tokio::test]
    async fn test_if_falsy_missing_key() {
        let out = template(
            &dummy_config(),
            Context::new(),
            "{% if show %}visible{% endif %}",
        )
        .await
        .unwrap();
        assert_eq!("", out);
    }

    #[tokio::test]
    async fn test_if_falsy_empty_scalar() {
        let ctx = make_ctx([("show", "".into())]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% if show %}visible{% endif %}",
        )
        .await
        .unwrap();
        assert_eq!("", out);
    }

    #[tokio::test]
    async fn test_if_truthy_nonempty_list() {
        let ctx = make_ctx([("items", TemplateValue::List(vec![Context::new()]))]);
        let out = template(&dummy_config(), ctx,"{% if items %}yes{% endif %}")
            .await
            .unwrap();
        assert_eq!("yes", out);
    }

    #[tokio::test]
    async fn test_if_falsy_empty_list() {
        let ctx = make_ctx([("items", TemplateValue::List(vec![]))]);
        let out = template(&dummy_config(), ctx,"{% if items %}yes{% endif %}")
            .await
            .unwrap();
        assert_eq!("", out);
    }

    #[tokio::test]
    async fn test_if_not_missing_key() {
        let out = template(
            &dummy_config(),
            Context::new(),
            "{% if not missing %}shown{% endif %}",
        )
        .await
        .unwrap();
        assert_eq!("shown", out);
    }

    #[tokio::test]
    async fn test_if_not_present_key() {
        let ctx = make_ctx([("present", "1".into())]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% if not present %}shown{% endif %}",
        )
        .await
        .unwrap();
        assert_eq!("", out);
    }

    #[tokio::test]
    async fn test_if_else_takes_then_branch() {
        let ctx = make_ctx([("flag", "1".into())]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% if flag %}yes{% else %}no{% endif %}",
        )
        .await
        .unwrap();
        assert_eq!("yes", out);
    }

    #[tokio::test]
    async fn test_if_else_takes_else_branch() {
        let out = template(
            &dummy_config(),
            Context::new(),
            "{% if flag %}yes{% else %}no{% endif %}",
        )
        .await
        .unwrap();
        assert_eq!("no", out);
    }

    #[tokio::test]
    async fn test_if_unterminated_errors() {
        let result = template(&dummy_config(), Context::new(), "{% if show %}visible").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_for_empty_list() {
        let ctx = make_ctx([("items", TemplateValue::List(vec![]))]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% for item in items %}{{ item.name }}{% endfor %}",
        )
        .await
        .unwrap();
        assert_eq!("", out);
    }

    #[tokio::test]
    async fn test_for_single_item() {
        let item = make_ctx([("name", "Alice".into())]);
        let ctx = make_ctx([("items", TemplateValue::List(vec![item]))]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% for item in items %}{{ item.name }}{% endfor %}",
        )
        .await
        .unwrap();
        assert_eq!("Alice", out);
    }

    #[tokio::test]
    async fn test_for_multiple_items() {
        let items = vec![
            make_ctx([("name", "Alice".into())]),
            make_ctx([("name", "Bob".into())]),
        ];
        let ctx = make_ctx([("items", TemplateValue::List(items))]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% for item in items %}{{ item.name }},{% endfor %}",
        )
        .await
        .unwrap();
        assert_eq!("Alice,Bob,", out);
    }

    #[tokio::test]
    async fn test_for_surrounding_text_preserved() {
        let items = vec![
            make_ctx([("x", "1".into())]),
            make_ctx([("x", "2".into())]),
        ];
        let ctx = make_ctx([("items", TemplateValue::List(items))]);
        let out = template(
            &dummy_config(),
            ctx,
            "before{% for item in items %}{{ item.x }},{% endfor %}after",
        )
        .await
        .unwrap();
        assert_eq!("before1,2,after", out);
    }

    #[tokio::test]
    async fn test_for_unterminated_errors() {
        let ctx = make_ctx([("items", TemplateValue::List(vec![]))]);
        let result = template(&dummy_config(), ctx,"{% for item in items %}body").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_for_if_item_field_present() {
        let items = vec![
            make_ctx([("name", "Alice".into()), ("date", "2024-01-01".into())]),
            make_ctx([("name", "Bob".into())]),
        ];
        let ctx = make_ctx([("items", TemplateValue::List(items))]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% for item in items %}{% if item.date %}{{ item.name }}{% endif %}{% endfor %}",
        )
        .await
        .unwrap();
        assert_eq!("Alice", out);
    }

    #[tokio::test]
    async fn test_for_nested_dot_list() {
        let pages = vec![
            make_ctx([("url", "/p1/".into())]),
            make_ctx([("url", "/p2/".into())]),
        ];
        let index = make_ctx([
            ("title", "Blog".into()),
            ("pages", TemplateValue::List(pages)),
        ]);
        let ctx = make_ctx([("indices", TemplateValue::List(vec![index]))]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% for index in indices %}{% for page in index.pages %}{{ page.url }}{% endfor %}{% endfor %}",
        ).await.unwrap();
        assert_eq!("/p1//p2/", out);
    }

    #[tokio::test]
    async fn test_for_nested_outer_and_inner_fields() {
        let pages = vec![make_ctx([("url", "/p1/".into())])];
        let index = make_ctx([
            ("title", "Blog".into()),
            ("pages", TemplateValue::List(pages)),
        ]);
        let ctx = make_ctx([("indices", TemplateValue::List(vec![index]))]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% for index in indices %}{{ index.title }}:{% for page in index.pages %}{{ page.url }}{% endfor %}{% endfor %}",
        ).await.unwrap();
        assert_eq!("Blog:/p1/", out);
    }

    #[tokio::test]
    async fn test_for_global_stack_accessible_in_body() {
        let items = vec![make_ctx([("name", "Alice".into())])];
        let ctx = make_ctx([
            ("items", TemplateValue::List(items)),
            ("prefix", "Hello ".into()),
        ]);
        let out = template(
            &dummy_config(),
            ctx,
            "{% for item in items %}{{ prefix }}{{ item.name }}{% endfor %}",
        )
        .await
        .unwrap();
        assert_eq!("Hello Alice", out);
    }
}
