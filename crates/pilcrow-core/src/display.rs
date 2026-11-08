//! A syntax-free reading copy of the document.
//!
//! Reading view never edits the buffer. It asks this module for one display
//! block per source block and renders those with the same type scale. Inline
//! delimiters, link URLs, heading hashes, quote marks, list markers and task
//! boxes are presentation here; none of them are removed from the file.

use crate::blocks::{self, Kind};
use crate::inline::{self, Kind as InlineKind};
use crate::lists;

/// One block of the reading copy. `kind` keeps the page's block styling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayBlock {
    pub kind: Kind,
    pub level: u8,
    pub text: String,
    /// List presentation, kept so HTML can preserve ordered lists and task
    /// state instead of flattening everything into bullets.
    pub ordered: bool,
    pub number: usize,
    pub depth: usize,
    pub task: Option<bool>,
}

/// Build the reading copy. Blank blocks and frontmatter disappear; rules stay
/// as empty blocks so the reading page can draw a hairline.
pub fn blocks(text: &str) -> Vec<DisplayBlock> {
    let mut scanned = Vec::new();
    blocks::scan(text, &mut scanned);
    let mut out: Vec<DisplayBlock> = Vec::new();
    let mut in_code_run = false;
    for block in scanned {
        let source = &text[block.start..block.end];
        if block.kind != Kind::Code || block.marker > 0 {
            in_code_run = false;
        }
        match block.kind {
            Kind::Blank | Kind::Frontmatter => {}
            Kind::Rule => out.push(DisplayBlock {
                kind: Kind::Rule,
                level: 0,
                text: String::new(),
                ordered: false,
                number: 0,
                depth: 0,
                task: None,
            }),
            Kind::Code => {
                if block.marker == 0 {
                    if in_code_run
                        && let Some(last) = out.last_mut()
                        && last.kind == Kind::Code
                    {
                        last.text.push('\n');
                        last.text.push_str(source);
                    } else {
                        out.push(DisplayBlock {
                            kind: Kind::Code,
                            level: 0,
                            text: source.to_owned(),
                            ordered: false,
                            number: 0,
                            depth: 0,
                            task: None,
                        });
                        in_code_run = true;
                    }
                }
            }
            Kind::Heading => {
                let body = source.get(block.marker..).unwrap_or_default();
                let text = join_soft_lines(&strip_inline(body));
                if !text.is_empty() {
                    out.push(DisplayBlock {
                        kind: Kind::Heading,
                        level: block.level,
                        text,
                        ordered: false,
                        number: 0,
                        depth: 0,
                        task: None,
                    });
                }
            }
            Kind::Quote => {
                // A merged quote block carries `> ` on every source line, not
                // just the first.
                let body = source
                    .lines()
                    .map(strip_quote_line)
                    .collect::<Vec<_>>()
                    .join("\n");
                let text = join_soft_lines(&strip_inline(&body));
                if !text.is_empty() {
                    out.push(DisplayBlock {
                        kind: Kind::Quote,
                        level: 0,
                        text,
                        ordered: false,
                        number: 0,
                        depth: 0,
                        task: None,
                    });
                }
            }
            Kind::ListItem => {
                if let Some(item) = lists::parse(source.as_bytes()) {
                    let body = source.get(item.marker_end..).unwrap_or_default();
                    let body = join_soft_lines(&strip_inline(body));
                    let indent = " ".repeat(item.indent);
                    let marker = if item.task {
                        if item.checked { "☑ " } else { "☐ " }
                    } else if item.ordered {
                        ""
                    } else {
                        "• "
                    };
                    let number = if item.ordered {
                        format!("{}{} ", item.number, item.punct as char)
                    } else {
                        String::new()
                    };
                    out.push(DisplayBlock {
                        kind: Kind::ListItem,
                        level: 0,
                        text: format!("{indent}{number}{marker}{body}"),
                        ordered: item.ordered,
                        number: item.number,
                        depth: item.indent / 2,
                        task: item.task.then_some(item.checked),
                    });
                }
            }
            Kind::Paragraph => {
                let text = join_soft_lines(&strip_inline(source));
                if !text.is_empty() {
                    out.push(DisplayBlock {
                        kind: Kind::Paragraph,
                        level: 0,
                        text,
                        ordered: false,
                        number: 0,
                        depth: 0,
                        task: None,
                    });
                }
            }
        }
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

fn join_soft_lines(text: &str) -> String {
    text.split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Remove inline syntax while keeping every word and character of prose.
fn strip_inline(text: &str) -> String {
    let mut kinds = vec![InlineKind::Plain; text.len()];
    inline::kinds(text, &mut kinds);
    let mut out = String::with_capacity(text.len());
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\'
            && text[index + 1..]
                .chars()
                .next()
                .is_some_and(|next| next.is_ascii_punctuation())
        {
            escaped = true;
            continue;
        }
        if kinds[index] == InlineKind::Marker {
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(text: &str) -> Vec<(Kind, String)> {
        blocks(text)
            .into_iter()
            .map(|block| (block.kind, block.text))
            .collect()
    }

    #[test]
    fn strips_block_and_inline_syntax() {
        let text =
            "# A *small* heading\n\n> a **quoted** line\n\n- [x] task with `code`\n\n1. numbered\n";
        let out = rendered(text);
        assert_eq!(
            out,
            vec![
                (Kind::Heading, "A small heading".into()),
                (Kind::Quote, "a quoted line".into()),
                (Kind::ListItem, "☑ task with code".into()),
                (Kind::ListItem, "1. numbered".into()),
            ]
        );
    }

    #[test]
    fn multi_line_quotes_lose_every_marker() {
        let out = rendered("> one\n> two\n> three");
        assert_eq!(out, vec![(Kind::Quote, "one two three".into())]);
    }

    #[test]
    fn links_keep_their_words_and_drop_the_url() {
        let out = rendered("Read [the docs](https://example.com/a_(b)) now.");
        assert_eq!(out, vec![(Kind::Paragraph, "Read the docs now.".into())]);
    }

    #[test]
    fn code_fences_disappear_but_code_lines_stay() {
        let text = "```rust\nlet x = *y;\n~~~inside\nlet z = 2;\n```\n```\nsecond();\n```";
        let out = rendered(text);
        assert_eq!(
            out,
            vec![
                (Kind::Code, "let x = *y;\n~~~inside\nlet z = 2;".into()),
                (Kind::Code, "second();".into()),
            ]
        );
    }

    #[test]
    fn frontmatter_and_blanks_are_not_reading_copy() {
        let text = "---\ntitle: hidden\n---\n\nVisible words.\n";
        assert_eq!(
            rendered(text),
            vec![(Kind::Paragraph, "Visible words.".into())]
        );
    }

    #[test]
    fn soft_source_lines_join_with_spaces() {
        let out = rendered("one line\nstill the same paragraph");
        assert_eq!(
            out,
            vec![(Kind::Paragraph, "one line still the same paragraph".into())]
        );
    }

    #[test]
    fn escaped_punctuation_is_plain_prose() {
        let out = rendered(r"a \*literal\* star");
        assert_eq!(out, vec![(Kind::Paragraph, "a *literal* star".into())]);
    }
}
