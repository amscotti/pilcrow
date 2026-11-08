//! Standalone HTML from the reading copy.
//!
//! Export is deliberately plain: one file, system fonts, no template engine,
//! no assets, and no print pipeline. The source Markdown remains the document;
//! this is a shareable rendering of it.

use crate::blocks::Kind;
use crate::display;

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
    let mut out = String::new();
    let mut list: Option<(bool, usize)> = None;
    for block in display::blocks(text) {
        let list_item = block.kind == Kind::ListItem;
        if list.is_some() && !list_item {
            out.push_str(if list.take().is_some_and(|(ordered, _)| ordered) {
                "</ol>\n"
            } else {
                "</ul>\n"
            });
        }
        match block.kind {
            Kind::Heading => {
                let level = block.level.clamp(1, 6);
                out.push_str(&format!("<h{level}>{}</h{level}>\n", escape(&block.text)));
            }
            Kind::Paragraph => {
                out.push_str(&format!("<p>{}</p>\n", escape(&block.text)));
            }
            Kind::Quote => {
                out.push_str(&format!(
                    "<blockquote>{}</blockquote>\n",
                    escape(&block.text)
                ));
            }
            Kind::Code => {
                out.push_str(&format!(
                    "<pre><code>{}</code></pre>\n",
                    escape(&block.text)
                ));
            }
            Kind::ListItem => {
                if list.is_none_or(|(ordered, _)| ordered != block.ordered) {
                    if let Some((ordered, _)) = list.take() {
                        out.push_str(if ordered { "</ol>\n" } else { "</ul>\n" });
                    }
                    if block.ordered {
                        if block.number > 1 {
                            out.push_str(&format!("<ol start=\"{}\">\n", block.number));
                        } else {
                            out.push_str("<ol>\n");
                        }
                    } else {
                        out.push_str("<ul>\n");
                    }
                    list = Some((block.ordered, block.number));
                }
                let task = block
                    .task
                    .map(|checked| {
                        format!(
                            "<span class=\"task\">{}</span> ",
                            if checked { "☑" } else { "☐" }
                        )
                    })
                    .unwrap_or_default();
                let depth = if block.depth > 0 {
                    format!(" style=\"margin-left:{}em\"", block.depth)
                } else {
                    String::new()
                };
                out.push_str(&format!(
                    "<li{depth}>{task}{}</li>\n",
                    escape(list_body(&block.text))
                ));
            }
            Kind::Rule => out.push_str("<hr>\n"),
            Kind::Blank | Kind::Frontmatter => {}
        }
    }
    if let Some((ordered, _)) = list {
        out.push_str(if ordered { "</ol>\n" } else { "</ul>\n" });
    }
    out
}

fn list_body(text: &str) -> &str {
    let text = text.trim_start();
    let text = text.strip_prefix("• ").unwrap_or(text);
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let text = if digits > 0 {
        let rest = &text[digits..];
        rest.strip_prefix(". ")
            .or_else(|| rest.strip_prefix(") "))
            .unwrap_or(text)
    } else {
        text
    };
    text.strip_prefix("☐ ")
        .or_else(|| text.strip_prefix("☑ "))
        .unwrap_or(text)
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
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
blockquote { margin: 1.25rem 0; padding-left: 1rem; border-left: 2px solid #c4c4c0; \
color: #5c5c5c; }
pre { overflow-x: auto; padding: 1rem; background: #eaeae6; border-radius: 4px; }
code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.92em; }
ul, ol { padding-left: 1.25rem; }
.task { color: #6d6d6d; }
hr { border: 0; border-top: 1px solid #c4c4c0; margin: 2rem 0; }
@media (prefers-color-scheme: dark) {
  body { background: #101011; color: #e6e6e3; }
  blockquote { border-color: #3c3c3f; color: #a0a0a0; }
  pre { background: #1e1e21; }
  hr { border-color: #3c3c3f; }
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
        assert!(html.contains("<p>A small note.</p>"));
        assert!(html.contains("<ul>\n<li>one</li>\n<li>two</li>\n</ul>"));
        assert!(html.contains("<blockquote>quote</blockquote>"));
        assert!(html.contains("<pre><code>let x = 1;\nlet y = 2;</code></pre>"));
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
