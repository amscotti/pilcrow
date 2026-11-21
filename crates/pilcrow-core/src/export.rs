//! Standalone HTML from the Markdown block and inline model.
//!
//! Export is deliberately plain: one file, system fonts, no template engine,
//! no assets, and no print pipeline. The source Markdown remains the document;
//! this is a shareable rendering of it.

use std::ops::Range;

use crate::blocks::{self, Kind};
use crate::inline::{self, Kind as InlineKind, Link};
use crate::lists;

/// A complete HTML document with a quiet reading style and dark-mode support.
pub fn document(text: &str, title: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<title>{}</title>\n<style>{}</style>\n</head>\n<body>\n<main>\n{}\n</main>\n</body>\n</html>\n",
        escape(title),
        STYLE,
        fragment(text)
    )
}

/// The body markup for one note.
pub fn fragment(text: &str) -> String {
    let mut scanned = Vec::new();
    blocks::scan(text, &mut scanned);

    let mut out = String::new();
    let mut list: Option<bool> = None;
    let mut code_run: Option<String> = None;

    for block in scanned {
        let source = &text[block.start..block.end];
        let continues_code = block.kind == Kind::Code && block.marker == 0;
        if !continues_code && let Some(code) = code_run.take() {
            out.push_str(&format!("<pre><code>{}</code></pre>\n", escape(&code)));
        }

        let list_item = block.kind == Kind::ListItem;
        if list.is_some() && !list_item {
            out.push_str(if list.take().is_some_and(|ordered| ordered) {
                "</ol>\n"
            } else {
                "</ul>\n"
            });
        }

        match block.kind {
            Kind::Blank | Kind::Frontmatter => {}
            Kind::Rule => out.push_str("<hr>\n"),
            Kind::Code => {
                if block.marker == 0 {
                    if let Some(code) = code_run.as_mut() {
                        code.push('\n');
                        code.push_str(source);
                    } else {
                        code_run = Some(source.to_owned());
                    }
                }
            }
            Kind::Heading => {
                let body = source.get(block.marker..).unwrap_or_default();
                let html = render_inline_html(body);
                if !html.is_empty() {
                    let level = block.level.clamp(1, 6);
                    out.push_str(&format!("<h{level}>{html}</h{level}>\n"));
                }
            }
            Kind::Paragraph => {
                let html = render_inline_html(source);
                if !html.is_empty() {
                    out.push_str(&format!("<p>{html}</p>\n"));
                }
            }
            Kind::Quote => {
                let body = source
                    .lines()
                    .map(strip_quote_line)
                    .collect::<Vec<_>>()
                    .join("\n");
                let html = render_inline_html(&body);
                if !html.is_empty() {
                    out.push_str(&format!("<blockquote>{html}</blockquote>\n"));
                }
            }
            Kind::ListItem => {
                let Some(item) = lists::parse(source.as_bytes()) else {
                    continue;
                };
                if list.is_none_or(|ordered| ordered != item.ordered) {
                    if let Some(ordered) = list.take() {
                        out.push_str(if ordered { "</ol>\n" } else { "</ul>\n" });
                    }
                    if item.ordered {
                        if item.number > 1 {
                            out.push_str(&format!("<ol start=\"{}\">\n", item.number));
                        } else {
                            out.push_str("<ol>\n");
                        }
                    } else {
                        out.push_str("<ul>\n");
                    }
                    list = Some(item.ordered);
                }
                let task = if item.task {
                    format!(
                        "<span class=\"task\">{}</span> ",
                        if item.checked { "☑" } else { "☐" }
                    )
                } else {
                    String::new()
                };
                let depth = item.indent / 2;
                let depth_attr = if depth > 0 {
                    format!(" style=\"margin-left:{depth}em\"")
                } else {
                    String::new()
                };
                let body = source.get(item.marker_end..).unwrap_or_default();
                let body_html = render_inline_html(body);
                out.push_str(&format!("<li{depth_attr}>{task}{body_html}</li>\n"));
            }
        }
    }

    if let Some(code) = code_run.take() {
        out.push_str(&format!("<pre><code>{}</code></pre>\n", escape(&code)));
    }
    if let Some(ordered) = list {
        out.push_str(if ordered { "</ol>\n" } else { "</ul>\n" });
    }
    out
}

fn strip_quote_line(line: &str) -> &str {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let Some(rest) = trimmed.strip_prefix('>') else {
        return line;
    };
    rest.strip_prefix(' ').unwrap_or(rest)
}

fn render_inline_html(source: &str) -> String {
    let source = source.trim();
    if source.is_empty() {
        return String::new();
    }
    let mut kinds = vec![InlineKind::Plain; source.len()];
    inline::kinds(source, &mut kinds);
    let links: Vec<Link> = if source.as_bytes().contains(&b'[') {
        inline::links(source)
            .into_iter()
            .filter(|link| {
                link.next <= source.len()
                    && link.text_start < link.text_end
                    && kinds.get(link.text_start.saturating_sub(1)) == Some(&InlineKind::Marker)
            })
            .collect()
    } else {
        Vec::new()
    };

    let soft = soft_break_map(source);
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    for link in links {
        if link.text_start - 1 < cursor {
            continue;
        }
        render_styled_range(source, &kinds, &soft, cursor..link.text_start - 1, &mut out);
        let raw_url = source[link.url_start..link.url_end].trim();
        if is_safe_url(raw_url) {
            out.push_str("<a href=\"");
            out.push_str(&escape(raw_url));
            out.push_str("\">");
            render_styled_range(
                source,
                &kinds,
                &soft,
                link.text_start..link.text_end,
                &mut out,
            );
            out.push_str("</a>");
        } else {
            render_styled_range(
                source,
                &kinds,
                &soft,
                link.text_start..link.text_end,
                &mut out,
            );
        }
        cursor = link.next;
    }
    render_styled_range(source, &kinds, &soft, cursor..source.len(), &mut out);
    out
}

/// Marks whitespace around newlines inside a trimmed block:
/// `0` = normal byte, `1` = skipped space/CR/extra newline, `2` = break emitting `' '`.
pub(crate) fn soft_break_map(source: &str) -> Vec<u8> {
    if !source.as_bytes().contains(&b'\n') {
        return Vec::new();
    }
    let bytes = source.as_bytes();
    let mut soft = vec![0u8; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let mut back = i;
            while back > 0 && matches!(bytes[back - 1], b' ' | b'\t' | b'\r') {
                back -= 1;
                soft[back] = 1;
            }
            soft[i] = 2;
            i += 1;
            while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
                soft[i] = 1;
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    soft
}

fn render_styled_range(
    source: &str,
    kinds: &[InlineKind],
    soft: &[u8],
    range: Range<usize>,
    out: &mut String,
) {
    let mut i = range.start;
    while i < range.end {
        let kind = kinds[i];
        if kind == InlineKind::Marker {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < range.end && kinds[i] == kind {
            i += 1;
        }
        let inner = render_text_run(source, soft, run_start..i, kind != InlineKind::Code);
        if inner.is_empty() {
            continue;
        }
        match kind {
            InlineKind::Plain | InlineKind::Marker => out.push_str(&inner),
            InlineKind::Bold => {
                out.push_str("<strong>");
                out.push_str(&inner);
                out.push_str("</strong>");
            }
            InlineKind::Italic => {
                out.push_str("<em>");
                out.push_str(&inner);
                out.push_str("</em>");
            }
            InlineKind::Strike => {
                out.push_str("<del>");
                out.push_str(&inner);
                out.push_str("</del>");
            }
            InlineKind::Code => {
                out.push_str("<code>");
                out.push_str(&inner);
                out.push_str("</code>");
            }
        }
    }
}

fn render_text_run(source: &str, soft: &[u8], range: Range<usize>, unescape: bool) -> String {
    let slice = &source[range.clone()];
    let mut out = String::with_capacity(slice.len());
    let mut escaped = false;
    for (rel, ch) in slice.char_indices() {
        let abs = range.start + rel;
        let mark = soft.get(abs).copied().unwrap_or(0);
        if mark == 1 {
            escaped = false;
            continue;
        }
        if mark == 2 {
            escaped = false;
            out.push(' ');
            continue;
        }
        if escaped {
            push_escaped_char(&mut out, ch);
            escaped = false;
            continue;
        }
        if unescape
            && ch == '\\'
            && source[abs + 1..]
                .chars()
                .next()
                .is_some_and(|next| next.is_ascii_punctuation())
        {
            escaped = true;
            continue;
        }
        push_escaped_char(&mut out, ch);
    }
    out
}

/// True when a link URL is safe to include in exported HTML (`http`, `https`,
/// `mailto`, or a relative reference without a dangerous scheme).
pub fn is_safe_url(url: &str) -> bool {
    if url.is_empty() || url.chars().any(char::is_control) {
        return false;
    }
    if let Some(colon) = url.find([':', '/', '?', '#'])
        && url.as_bytes()[colon] == b':'
    {
        let scheme = &url[..colon];
        return scheme.eq_ignore_ascii_case("http")
            || scheme.eq_ignore_ascii_case("https")
            || scheme.eq_ignore_ascii_case("mailto");
    }
    true
}

/// True when a link URL can be handed directly to the host OS launcher.
pub fn is_external_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return false;
    }
    let Some(colon) = url.find(':') else {
        return false;
    };
    let scheme = &url[..colon];
    scheme.eq_ignore_ascii_case("http")
        || scheme.eq_ignore_ascii_case("https")
        || scheme.eq_ignore_ascii_case("mailto")
}

fn push_escaped_char(out: &mut String, ch: char) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        _ => out.push(ch),
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        push_escaped_char(&mut out, ch);
    }
    out
}

const STYLE: &str = "\
:root { color-scheme: light dark; }
body { margin: 0; background: #fbfbfa; color: #1a1a1a; }
main { max-width: 42rem; margin: 0 auto; padding: 3rem 1.5rem 6rem; \
font: 1.0625rem/1.7 ui-serif, Georgia, serif; }
h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin: 2rem 0 0.75rem; }
h1 { font-size: 2rem; } h2 { font-size: 1.5rem; } h3 { font-size: 1.2rem; }
p { margin: 0 0 1rem; }
a { color: #1a5ccc; }
blockquote { margin: 1.25rem 0; padding-left: 1rem; border-left: 2px solid #c4c4c0; \
color: #5c5c5c; }
pre { overflow-x: auto; padding: 1rem; background: #eaeae6; border-radius: 4px; }
code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.92em; }
p code, li code, blockquote code { padding: 0.1em 0.3em; background: #eaeae6; border-radius: 3px; }
ul, ol { padding-left: 1.25rem; }
.task { color: #6d6d6d; }
hr { border: 0; border-top: 1px solid #c4c4c0; margin: 2rem 0; }
@media (prefers-color-scheme: dark) {
  body { background: #101011; color: #e6e6e3; }
  a { color: #4c9aff; }
  blockquote { border-color: #3c3c3f; color: #a0a0a0; }
  pre { background: #1e1e21; }
  p code, li code, blockquote code { background: #1e1e21; }
  hr { border-color: #3c3c3f; }
}
@media print {
  body { background: #fff; color: #000; }
  main { max-width: none; padding: 0; }
  pre, blockquote { break-inside: avoid; }
  h1, h2, h3 { break-after: avoid; }
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_semantic_blocks_and_groups_lists() {
        let html = document(
            "# Title\n\nA *small* note.\n\n- one\n- two\n\n> quote\n\n```\nlet x = 1;\nlet y = 2;\n```",
            "Title",
        );
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<p>A <em>small</em> note.</p>"));
        assert!(html.contains("<ul>\n<li>one</li>\n<li>two</li>\n</ul>"));
        assert!(html.contains("<blockquote>quote</blockquote>"));
        assert!(html.contains("<pre><code>let x = 1;\nlet y = 2;</code></pre>"));
    }

    #[test]
    fn exports_inline_formatting_links_and_rejects_unsafe_schemes() {
        let html = fragment(
            "Use **bold**, _italic_, ~~old~~, `code <tag>`, and [safe](https://example.com/a?b=1&c=2).\n\n\
             Also [Unsafe](javascript:alert(1)), [Data](data:text/html,<x>), [Local](./other.md#top), \
             `[not](https://ignored.test)`, and \\*literal\\*.",
        );
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("<em>italic</em>"));
        assert!(html.contains("<del>old</del>"));
        assert!(html.contains("<code>code &lt;tag&gt;</code>"));
        assert!(html.contains("<a href=\"https://example.com/a?b=1&amp;c=2\">safe</a>"));
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("data:text/html"));
        assert!(html.contains("Also Unsafe, Data, <a href=\"./other.md#top\">Local</a>"));
        assert!(html.contains("<code>[not](https://ignored.test)</code>"));
        assert!(html.contains("*literal*."));
    }

    #[test]
    fn ordered_and_task_lists_keep_their_shape() {
        let html = fragment("1. first\n2. second\n\n- [x] done\n- [ ] todo");
        assert!(html.contains("<ol>\n<li>first</li>\n<li>second</li>\n</ol>"));
        assert!(html.contains("<ul>\n<li><span class=\"task\">☑</span> done</li>"));
        assert!(html.contains("<li><span class=\"task\">☐</span> todo</li>"));
        let ordered_task = fragment("1. [x] done");
        assert_eq!(
            ordered_task,
            "<ol>\n<li><span class=\"task\">☑</span> done</li>\n</ol>\n"
        );
        let switched = fragment("- bullet\n1. number");
        assert!(switched.contains("</ul>\n<ol>"));
    }

    #[test]
    fn escapes_text_and_title() {
        let html = document("5 < 6 & \"quoted\"", "<title>");
        assert!(html.contains("<title>&lt;title&gt;</title>"));
        assert!(html.contains("<p>5 &lt; 6 &amp; &quot;quoted&quot;</p>"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn frontmatter_is_not_exported() {
        let html = document("---\ntitle: secret\n---\n\nVisible.", "note");
        assert!(!html.contains("secret"));
        assert!(html.contains("<p>Visible.</p>"));
    }
}
