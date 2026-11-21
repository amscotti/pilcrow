//! A syntax-free reading copy of the document.
//!
//! Reading view never edits the buffer. It asks this module for one display
//! block per source block and renders those with the same type scale. Inline
//! delimiters, link URLs, heading hashes, quote marks, list markers and task
//! boxes are presentation here; none of them are removed from the file.

use std::ops::Range;

use crate::blocks::{self, Kind};
use crate::inline::{self, Kind as InlineKind, Link};
use crate::lists;

/// One styled range in [`DisplayBlock::text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplaySpan {
    pub range: Range<usize>,
    pub kind: InlineKind,
}

/// One parsed `[text](url)` link range in [`DisplayBlock::text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayLink {
    pub range: Range<usize>,
    pub url: String,
}

/// One block of the reading copy. `kind` keeps the page's block styling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayBlock {
    pub kind: Kind,
    pub level: u8,
    pub text: String,
    /// Byte offset of this block in the source document, for scroll sync.
    pub source_start: usize,
    pub spans: Vec<DisplaySpan>,
    pub links: Vec<DisplayLink>,
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
                source_start: block.start,
                spans: Vec::new(),
                links: Vec::new(),
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
                            source_start: block.start,
                            spans: Vec::new(),
                            links: Vec::new(),
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
                let (text, spans, links) = extract_inline(body, 0);
                if !text.is_empty() {
                    out.push(DisplayBlock {
                        kind: Kind::Heading,
                        level: block.level,
                        text,
                        source_start: block.start,
                        spans,
                        links,
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
                let (text, spans, links) = extract_inline(&body, 0);
                if !text.is_empty() {
                    out.push(DisplayBlock {
                        kind: Kind::Quote,
                        level: 0,
                        text,
                        source_start: block.start,
                        spans,
                        links,
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
                    let prefix = format!("{indent}{number}{marker}");
                    let (body_text, spans, links) = extract_inline(body, prefix.len());
                    out.push(DisplayBlock {
                        kind: Kind::ListItem,
                        level: 0,
                        text: format!("{prefix}{body_text}"),
                        source_start: block.start,
                        spans,
                        links,
                        ordered: item.ordered,
                        number: item.number,
                        depth: item.indent / 2,
                        task: item.task.then_some(item.checked),
                    });
                }
            }
            Kind::Paragraph => {
                let (text, spans, links) = extract_inline(source, 0);
                if !text.is_empty() {
                    out.push(DisplayBlock {
                        kind: Kind::Paragraph,
                        level: 0,
                        text,
                        source_start: block.start,
                        spans,
                        links,
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

/// Remove inline syntax while preserving styled byte spans and link metadata.
fn extract_inline(raw: &str, base_offset: usize) -> (String, Vec<DisplaySpan>, Vec<DisplayLink>) {
    let source = raw.trim();
    if source.is_empty() {
        return (String::new(), Vec::new(), Vec::new());
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
    let soft = crate::export::soft_break_map(source);

    let mut out = String::with_capacity(source.len());
    let mut spans: Vec<DisplaySpan> = Vec::new();
    let mut out_links: Vec<DisplayLink> = Vec::new();
    let mut active_link: Option<(usize, Range<usize>, String)> = None;
    let mut link_idx = 0usize;
    let mut escaped = false;

    for (index, ch) in source.char_indices() {
        let mark = soft.get(index).copied().unwrap_or(0);
        if mark == 1 {
            escaped = false;
            continue;
        }
        while link_idx < links.len() && links[link_idx].next <= index {
            link_idx += 1;
        }
        let in_link = links
            .get(link_idx)
            .filter(|link| (link.text_start..link.text_end).contains(&index))
            .map(|link| (link_idx, source[link.url_start..link.url_end].trim()));

        if mark == 2 {
            escaped = false;
            let start = base_offset + out.len();
            out.push(' ');
            let end = base_offset + out.len();
            if let Some((idx, url)) = in_link {
                extend_link(&mut active_link, &mut out_links, idx, start..end, url);
            } else if let Some((_, range, url)) = active_link.take() {
                out_links.push(DisplayLink { range, url });
            }
            continue;
        }

        let kind = kinds[index];
        if escaped {
            escaped = false;
            let start = base_offset + out.len();
            out.push(ch);
            let end = base_offset + out.len();
            push_span(&mut spans, kind, start..end);
            if let Some((idx, url)) = in_link {
                extend_link(&mut active_link, &mut out_links, idx, start..end, url);
            } else if let Some((_, range, url)) = active_link.take() {
                out_links.push(DisplayLink { range, url });
            }
            continue;
        }
        if kind != InlineKind::Code
            && ch == '\\'
            && source[index + 1..]
                .chars()
                .next()
                .is_some_and(|next| next.is_ascii_punctuation())
        {
            escaped = true;
            continue;
        }
        if kind == InlineKind::Marker {
            if let Some((_, range, url)) = active_link.take() {
                out_links.push(DisplayLink { range, url });
            }
            continue;
        }

        let start = base_offset + out.len();
        out.push(ch);
        let end = base_offset + out.len();
        push_span(&mut spans, kind, start..end);
        if let Some((idx, url)) = in_link {
            extend_link(&mut active_link, &mut out_links, idx, start..end, url);
        } else if let Some((_, range, url)) = active_link.take() {
            out_links.push(DisplayLink { range, url });
        }
    }
    if let Some((_, range, url)) = active_link {
        out_links.push(DisplayLink { range, url });
    }
    (out, spans, out_links)
}

fn push_span(spans: &mut Vec<DisplaySpan>, kind: InlineKind, range: Range<usize>) {
    if !matches!(
        kind,
        InlineKind::Bold | InlineKind::Italic | InlineKind::Strike | InlineKind::Code
    ) {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.kind == kind
        && last.range.end == range.start
    {
        last.range.end = range.end;
    } else {
        spans.push(DisplaySpan { range, kind });
    }
}

fn extend_link(
    active: &mut Option<(usize, Range<usize>, String)>,
    out: &mut Vec<DisplayLink>,
    idx: usize,
    range: Range<usize>,
    url: &str,
) {
    if let Some((cur_idx, cur_range, _)) = active.as_mut()
        && *cur_idx == idx
        && cur_range.end == range.start
    {
        cur_range.end = range.end;
        return;
    }
    if let Some((_, prev_range, prev_url)) = active.take() {
        out.push(DisplayLink {
            range: prev_range,
            url: prev_url,
        });
    }
    *active = Some((idx, range, url.to_owned()));
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

    #[test]
    fn spans_and_links_record_exact_offsets_in_reading_copy() {
        let text =
            "- [ ] Buy **milk** and `oat` from [Store](https://shop.test)\n\nPara ~~old~~ *new*.";
        let out = blocks(text);
        assert_eq!(out.len(), 2);

        let item = &out[0];
        assert_eq!(item.text, "☐ Buy milk and oat from Store");
        assert_eq!(item.source_start, 0);
        assert_eq!(
            item.spans
                .iter()
                .map(|s| (s.kind, &item.text[s.range.clone()]))
                .collect::<Vec<_>>(),
            vec![(InlineKind::Bold, "milk"), (InlineKind::Code, "oat")]
        );
        assert_eq!(item.links.len(), 1);
        assert_eq!(&item.text[item.links[0].range.clone()], "Store");
        assert_eq!(item.links[0].url, "https://shop.test");

        let para = &out[1];
        assert_eq!(para.text, "Para old new.");
        assert_eq!(
            para.spans
                .iter()
                .map(|s| (s.kind, &para.text[s.range.clone()]))
                .collect::<Vec<_>>(),
            vec![(InlineKind::Strike, "old"), (InlineKind::Italic, "new")]
        );
    }
}
