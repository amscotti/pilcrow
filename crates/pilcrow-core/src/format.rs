//! Markdown formatting from the keyboard.
//!
//! Every command returns one replacement range, one replacement string, and
//! where the selection should land. The editor applies it as a single edit, so
//! undo is one step and the file remains the only document.
//!
//! Inline markers are toggles: pressing bold inside `**words**` removes the
//! pair rather than nesting another. Line markers are parsed with the same
//! rules as the block scanner and the Enter handler, so a command normalises
//! `* item` or `#nope` instead of stacking a second marker.

use std::ops::Range;

use crate::lists::parse as parse_list;

fn is_space(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

fn indent_len(line: &[u8]) -> usize {
    line.iter().take_while(|byte| is_space(**byte)).count()
}

/// The formatting commands the editor, menu and keymap share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Bold,
    Italic,
    Strike,
    Code,
    Link,
    Heading(u8),
    Bullet,
    Ordered,
    Task,
    Quote,
}

/// Where the caret and selection go after the replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Caret(usize),
    Selection { anchor: usize, caret: usize },
}

/// One atomic formatting edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatEdit {
    pub range: Range<usize>,
    pub replacement: String,
    pub after: Placement,
}

/// Build the edit for `command`, or `None` when the command would change
/// nothing or the selected shape is one Markdown cannot express.
pub fn command(
    text: &str,
    from: usize,
    to: usize,
    caret: usize,
    command: Command,
) -> Option<FormatEdit> {
    let len = text.len();
    let mut from = from.min(len);
    let mut to = to.min(len);
    if from > to {
        std::mem::swap(&mut from, &mut to);
    }
    let caret = caret.min(len);
    let reverse = from != to && caret == from;
    let mut edit = match command {
        Command::Bold => wrap(text, from, to, caret, "**", "**"),
        Command::Italic => italic(text, from, to, caret),
        Command::Strike => wrap(text, from, to, caret, "~~", "~~"),
        Command::Code => wrap(text, from, to, caret, "`", "`"),
        Command::Link => link(text, from, to),
        Command::Heading(level) => lines(text, from, to, caret, Op::Heading(level.clamp(1, 6))),
        Command::Bullet => lines(text, from, to, caret, Op::Bullet),
        Command::Ordered => lines(text, from, to, caret, Op::Ordered),
        Command::Task => lines(text, from, to, caret, Op::Task),
        Command::Quote => lines(text, from, to, caret, Op::Quote),
    }?;
    if reverse
        && command != Command::Link
        && let Placement::Selection { anchor, caret } = edit.after
    {
        edit.after = Placement::Selection {
            anchor: caret,
            caret: anchor,
        };
    }
    Some(edit)
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

fn is_edge(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// The selection with edge whitespace removed.
fn trimmed(text: &str, from: usize, to: usize) -> Range<usize> {
    let bytes = text.as_bytes();
    let mut start = from;
    let mut end = to;
    while start < end && is_edge(bytes[start]) {
        start += 1;
    }
    while end > start && is_edge(bytes[end - 1]) {
        end -= 1;
    }
    start..end
}

/// Wrap the selection (or caret) in `open`…`close`, or take the pair off when
/// it is already there.
fn wrap(
    text: &str,
    from: usize,
    to: usize,
    caret: usize,
    open: &str,
    close: &str,
) -> Option<FormatEdit> {
    if from == to {
        let at = caret.min(text.len());
        if let Some(edit) = strip_at(text, open, close, at) {
            return Some(edit);
        }
        return Some(FormatEdit {
            range: at..at,
            replacement: format!("{open}{close}"),
            after: Placement::Caret(at + open.len()),
        });
    }

    let range = trimmed(text, from, to);
    if range.is_empty() || has_blank_line(&text[range.clone()]) {
        return None;
    }
    if let Some(edit) = strip_pair(text, open, close, range.clone()) {
        return Some(edit);
    }
    let words = &text[range.clone()];
    Some(FormatEdit {
        range: range.clone(),
        replacement: format!("{open}{words}{close}"),
        after: Placement::Selection {
            anchor: range.start + open.len(),
            caret: range.start + open.len() + words.len(),
        },
    })
}

/// Whether the range contains a blank separator line, which is a paragraph
/// boundary. Inline emphasis cannot cross one without writing stray markers.
fn has_blank_line(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\n' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r') {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'\n' {
            return true;
        }
        i += 1;
    }
    false
}

fn is_blank_line(line: &[u8]) -> bool {
    line.iter().all(|b| matches!(*b, b' ' | b'\t' | b'\r'))
}

/// The contiguous non-blank paragraph containing `at` (or the blank line
/// itself when `at` sits on a blank line).
fn paragraph_around(text: &str, at: usize) -> Range<usize> {
    let bytes = text.as_bytes();
    let mut line_start = at;
    while line_start > 0 && bytes[line_start - 1] != b'\n' {
        line_start -= 1;
    }
    let mut line_end = at;
    while line_end < bytes.len() && bytes[line_end] != b'\n' {
        line_end += 1;
    }
    if is_blank_line(&bytes[line_start..line_end]) {
        return line_start..line_end;
    }
    let mut start = line_start;
    while start > 0 {
        let prev_end = start - 1;
        let mut prev_start = prev_end;
        while prev_start > 0 && bytes[prev_start - 1] != b'\n' {
            prev_start -= 1;
        }
        if is_blank_line(&bytes[prev_start..prev_end]) {
            break;
        }
        start = prev_start;
    }
    let mut end = line_end;
    while end < bytes.len() {
        let next_start = end + 1;
        let mut next_end = next_start;
        while next_end < bytes.len() && bytes[next_end] != b'\n' {
            next_end += 1;
        }
        if is_blank_line(&bytes[next_start..next_end]) {
            break;
        }
        end = next_end;
    }
    start..end
}

/// Remove a pair when the caret is inside it, so a second press inside bold
/// text takes the bold off instead of nesting another pair. Pairs are matched
/// in order within the current paragraph so a closing marker of an earlier
/// span is never paired with the opening marker of a later one.
fn strip_at(text: &str, open: &str, close: &str, at: usize) -> Option<FormatEdit> {
    let at = at.min(text.len());
    let para = paragraph_around(text, at);
    let slice = &text[para.clone()];
    let mut cursor = 0;
    while let Some(rel_open) = slice[cursor..].find(open) {
        let start = para.start + cursor + rel_open;
        let inner_start = start + open.len();
        let after_open = cursor + rel_open + open.len();
        let Some(rel_close) = slice[after_open..].find(close) else {
            break;
        };
        let close_at = para.start + after_open + rel_close;
        if at >= inner_start && at <= close_at {
            let inner = &text[inner_start..close_at];
            let caret = (at.saturating_sub(open.len())).max(start);
            return Some(FormatEdit {
                range: start..close_at + close.len(),
                replacement: inner.to_owned(),
                after: Placement::Caret(caret),
            });
        }
        cursor = after_open + rel_close + close.len();
    }
    None
}

/// Remove a pair already around the words: selected inside it, or sitting
/// just outside the selection. The words stay selected.
fn strip_pair(text: &str, open: &str, close: &str, range: Range<usize>) -> Option<FormatEdit> {
    let words = &text[range.clone()];
    let inside = words.len() >= open.len() + close.len()
        && words.starts_with(open)
        && words.ends_with(close);
    if inside {
        let inner = &words[open.len()..words.len() - close.len()];
        return Some(FormatEdit {
            range: range.clone(),
            replacement: inner.to_owned(),
            after: Placement::Selection {
                anchor: range.start,
                caret: range.start + inner.len(),
            },
        });
    }

    if range.start >= open.len()
        && range.end + close.len() <= text.len()
        && text.get(range.start - open.len()..range.start) == Some(open)
        && text.get(range.end..range.end + close.len()) == Some(close)
    {
        let inner = &text[range.clone()];
        let start = range.start - open.len();
        return Some(FormatEdit {
            range: start..range.end + close.len(),
            replacement: inner.to_owned(),
            after: Placement::Selection {
                anchor: start,
                caret: start + inner.len(),
            },
        });
    }
    None
}

/// Italic uses `*` inside a word and `_` at a word edge, because `_` does not
/// bind there. Either delimiter comes off on a second press.
fn italic(text: &str, from: usize, to: usize, caret: usize) -> Option<FormatEdit> {
    let bytes = text.as_bytes();
    if from == to {
        let at = caret.min(text.len());
        if let Some(edit) = strip_at(text, "_", "_", at) {
            return Some(edit);
        }
        if let Some(edit) = strip_at(text, "*", "*", at) {
            return Some(edit);
        }
        let intraword = (at > 0 && is_word_byte(bytes[at - 1]))
            || (at < bytes.len() && is_word_byte(bytes[at]));
        let delimiter = if intraword { "*" } else { "_" };
        return wrap(text, from, to, caret, delimiter, delimiter);
    }

    let range = trimmed(text, from, to);
    if range.is_empty() || has_blank_line(&text[range.clone()]) {
        return None;
    }
    if let Some(edit) = strip_pair(text, "_", "_", range.clone()) {
        return Some(edit);
    }
    if let Some(edit) = strip_pair(text, "*", "*", range.clone()) {
        return Some(edit);
    }
    let intraword = (range.start > 0 && is_word_byte(bytes[range.start - 1]))
        || (range.end < bytes.len() && is_word_byte(bytes[range.end]));
    let delimiter = if intraword { "*" } else { "_" };
    let words = &text[range.clone()];
    Some(FormatEdit {
        range: range.clone(),
        replacement: format!("{delimiter}{words}{delimiter}"),
        after: Placement::Selection {
            anchor: range.start + delimiter.len(),
            caret: range.start + delimiter.len() + words.len(),
        },
    })
}

fn link(text: &str, from: usize, to: usize) -> Option<FormatEdit> {
    let range = trimmed(text, from, to);
    let words = &text[range.clone()];
    if words.is_empty() || words.contains(['\n', '\r']) {
        return None;
    }
    // An odd run of backslashes before the selection escapes the `[` the
    // command would write. Repairing that would touch bytes outside the
    // selection, so leave the document alone.
    let mut lead = 0;
    while lead < range.start && text.as_bytes()[range.start - 1 - lead] == b'\\' {
        lead += 1;
    }
    if lead % 2 == 1 {
        return None;
    }

    let mut body = String::with_capacity(words.len());
    for ch in words.chars() {
        if ch == ']' {
            let slashes = body
                .as_bytes()
                .iter()
                .rev()
                .take_while(|b| **b == b'\\')
                .count();
            if slashes % 2 == 0 {
                body.push('\\');
            }
        }
        body.push(ch);
    }
    let slashes = body
        .as_bytes()
        .iter()
        .rev()
        .take_while(|b| **b == b'\\')
        .count();
    if slashes % 2 == 1 {
        body.push('\\');
    }

    let url_at = range.start + 1 + body.len() + 2;
    Some(FormatEdit {
        range,
        replacement: format!("[{body}](url)"),
        after: Placement::Selection {
            anchor: url_at,
            caret: url_at + 3,
        },
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Heading(u8),
    Bullet,
    Ordered,
    Task,
    Quote,
}

fn heading_len(line: &[u8]) -> usize {
    let indent = indent_len(line).min(3);
    let mut hashes = 0;
    while indent + hashes < line.len() && line[indent + hashes] == b'#' {
        hashes += 1;
    }
    if hashes == 0 || hashes > 6 {
        return 0;
    }
    let at = indent + hashes;
    if at < line.len() && line[at] == b' ' {
        at + 1
    } else {
        at
    }
}

fn quote_len(line: &[u8]) -> usize {
    let indent = indent_len(line).min(3);
    if indent >= line.len() || line[indent] != b'>' {
        return 0;
    }
    if indent + 1 < line.len() && line[indent + 1] == b' ' {
        indent + 2
    } else {
        indent + 1
    }
}

fn marker_len(op: Op, line: &[u8]) -> usize {
    match op {
        Op::Heading(_) => heading_len(line),
        Op::Bullet | Op::Ordered | Op::Task => parse_list(line).map_or(0, |item| item.marker_end),
        Op::Quote => quote_len(line),
    }
}

fn has_exact(op: Op, line: &[u8]) -> bool {
    let indent = indent_len(line);
    match op {
        Op::Heading(level) => {
            let at = heading_len(line);
            if at == 0 {
                return false;
            }
            let mut hashes = 0;
            while indent + hashes < line.len() && line[indent + hashes] == b'#' {
                hashes += 1;
            }
            hashes == level as usize
                && (indent + hashes == line.len() || line[indent + hashes] == b' ')
        }
        Op::Bullet | Op::Ordered | Op::Task => {
            let Some(item) = parse_list(line) else {
                return false;
            };
            match op {
                Op::Bullet => !item.ordered && !item.task && item.bullet == b'-',
                Op::Ordered => item.ordered && item.punct == b'.',
                Op::Task => item.task && !item.ordered && item.bullet == b'-',
                _ => false,
            }
        }
        Op::Quote => {
            let at = quote_len(line);
            at > 0 && (at == line.len() || at - indent == 2)
        }
    }
}

fn new_prefix(op: Op, index: usize) -> String {
    match op {
        Op::Heading(level) => format!("{} ", "#".repeat(level as usize)),
        Op::Bullet => "- ".to_owned(),
        Op::Ordered => format!("{}. ", index + 1),
        Op::Task => "- [ ] ".to_owned(),
        Op::Quote => "> ".to_owned(),
    }
}

fn beyond_rule(op: Op, line: &[u8]) -> bool {
    indent_len(line) >= 4 && matches!(op, Op::Heading(_) | Op::Quote)
}

fn lines(text: &str, from: usize, to: usize, caret: usize, op: Op) -> Option<FormatEdit> {
    let bytes = text.as_bytes();
    let mut start = from;
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    let mut end = to;
    if end > start && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    let had_selection = from != to;

    let mut line_list: Vec<&str> = Vec::new();
    let mut at = start;
    loop {
        let newline = text[at..]
            .find('\n')
            .map_or(text.len(), |offset| at + offset);
        line_list.push(&text[at..newline.min(end)]);
        if newline >= end || newline == text.len() {
            break;
        }
        at = newline + 1;
    }

    let mut off = true;
    let mut saw_line = false;
    for line in &line_list {
        if line.trim_matches([' ', '\t']).is_empty() {
            continue;
        }
        saw_line = true;
        if !has_exact(op, line.as_bytes()) {
            off = false;
            break;
        }
    }
    if !saw_line {
        off = false;
    }

    let mut any_marked = false;
    for line in &line_list {
        if line.trim_matches([' ', '\t']).is_empty() {
            continue;
        }
        if !beyond_rule(op, line.as_bytes()) {
            any_marked = true;
            break;
        }
    }

    let mut out = String::new();
    let mut caret_new = start;
    let mut caret_set = false;
    let mut line_start_in = start;
    let mut line_start_out = start;
    let mut item_index = 0usize;
    for (index, line) in line_list.iter().enumerate() {
        let line_bytes = line.as_bytes();
        if index > 0 {
            out.push('\n');
            line_start_out += 1;
        }
        let blank = line.trim_matches([' ', '\t']).is_empty();
        let indent = indent_len(line_bytes);
        let beyond = beyond_rule(op, line_bytes);
        let marked = !beyond && (!blank || !saw_line || (op == Op::Quote && any_marked));
        let old = marker_len(op, line_bytes);
        let head = old.max(indent).min(line_bytes.len());
        let content = &line[head..];
        let prefix = if off || !marked {
            String::new()
        } else {
            item_index += 1;
            new_prefix(op, item_index - 1)
        };
        out.push_str(&line[..indent]);
        out.push_str(&prefix);
        out.push_str(content);

        if !had_selection && !caret_set && caret <= line_start_in + line.len() {
            let cin = caret - line_start_in;
            if cin < indent {
                caret_new = line_start_out + cin;
            } else {
                let col = cin.saturating_sub(head);
                caret_new = line_start_out + indent + prefix.len() + col.min(content.len());
            }
            caret_set = true;
        }
        line_start_in += line.len() + 1;
        line_start_out = start + out.len();
    }

    if text[start..end] == out {
        return None;
    }
    let new_len = out.len();
    let new_doc_len = text
        .len()
        .saturating_sub(end - start)
        .saturating_add(new_len);
    Some(FormatEdit {
        range: start..end,
        replacement: out,
        after: if had_selection {
            Placement::Selection {
                anchor: start,
                caret: start + new_len,
            }
        } else {
            Placement::Caret(caret_new.min(new_doc_len))
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str, from: usize, to: usize, caret: usize, cmd: Command) -> FormatEdit {
        command(text, from, to, caret, cmd).expect("command should edit")
    }

    #[test]
    fn bold_inserts_a_pair_and_keeps_the_caret_inside() {
        let e = edit("hello", 5, 5, 5, Command::Bold);
        assert_eq!(e.range, 5..5);
        assert_eq!(e.replacement, "****");
        assert_eq!(e.after, Placement::Caret(7));
    }

    #[test]
    fn bold_wraps_and_strips_a_selection_inside_or_outside() {
        let e = edit("hello world", 6, 11, 11, Command::Bold);
        assert_eq!(e.range, 6..11);
        assert_eq!(e.replacement, "**world**");
        assert_eq!(
            e.after,
            Placement::Selection {
                anchor: 8,
                caret: 13
            }
        );

        let e = edit("**world**", 2, 7, 7, Command::Bold);
        assert_eq!(e.range, 0..9);
        assert_eq!(e.replacement, "world");
        assert_eq!(
            e.after,
            Placement::Selection {
                anchor: 0,
                caret: 5
            }
        );

        let e = edit("**world**", 0, 9, 9, Command::Bold);
        assert_eq!(e.range, 0..9);
        assert_eq!(e.replacement, "world");
    }

    #[test]
    fn italic_uses_the_delimiter_that_binds() {
        let e = edit("word", 0, 4, 4, Command::Italic);
        assert_eq!(e.replacement, "_word_");
        let e = edit("keyword", 3, 3, 3, Command::Italic);
        assert_eq!(e.replacement, "**");
        let e = edit("a word b", 2, 6, 6, Command::Italic);
        assert_eq!(e.replacement, "_word_");
        let e = edit("keyword", 0, 3, 3, Command::Italic);
        assert_eq!(e.replacement, "*key*");
    }

    #[test]
    fn link_selects_the_url_placeholder_and_escapes_brackets() {
        let e = edit("see docs", 4, 8, 8, Command::Link);
        assert_eq!(e.replacement, "[docs](url)");
        assert_eq!(
            e.after,
            Placement::Selection {
                anchor: 11,
                caret: 14
            }
        );

        let e = edit("a]b", 0, 3, 3, Command::Link);
        assert_eq!(e.replacement, "[a\\]b](url)");
        assert!(command("two\nlines", 0, 9, 9, Command::Link).is_none());
        assert!(command("\\word", 1, 5, 5, Command::Link).is_none());
    }

    #[test]
    fn headings_toggle_normalise_and_preserve_indent() {
        let e = edit("title", 0, 0, 0, Command::Heading(1));
        assert_eq!(e.replacement, "# title");
        let e = edit("# title", 0, 0, 0, Command::Heading(1));
        assert_eq!(e.replacement, "title");
        let e = edit("## title", 0, 0, 0, Command::Heading(3));
        assert_eq!(e.replacement, "### title");
        let e = edit("#nope", 0, 0, 0, Command::Heading(1));
        assert_eq!(e.replacement, "# nope");
        let e = edit("  # title", 0, 0, 0, Command::Heading(2));
        assert_eq!(e.replacement, "  ## title");
        assert!(command("    # deep", 0, 0, 0, Command::Heading(1)).is_none());
    }

    #[test]
    fn lists_toggle_and_normalise_markers() {
        let e = edit("one", 0, 0, 0, Command::Bullet);
        assert_eq!(e.replacement, "- one");
        let e = edit("- one", 0, 0, 0, Command::Bullet);
        assert_eq!(e.replacement, "one");
        let e = edit("* one", 0, 0, 0, Command::Bullet);
        assert_eq!(e.replacement, "- one");
        let e = edit("- [x] done", 0, 0, 0, Command::Task);
        assert_eq!(e.replacement, "done");
        let e = edit("one", 0, 0, 0, Command::Task);
        assert_eq!(e.replacement, "- [ ] one");
    }

    #[test]
    fn ordered_lists_number_each_marked_line() {
        let e = edit("one\ntwo\n\nthree", 0, 15, 15, Command::Ordered);
        assert_eq!(e.replacement, "1. one\n2. two\n\n3. three");
    }

    #[test]
    fn blank_lines_stay_blank_except_inside_a_quote() {
        let e = edit("one\n\ntwo", 0, 8, 8, Command::Bullet);
        assert_eq!(e.replacement, "- one\n\n- two");
        let e = edit("one\n\ntwo", 0, 8, 8, Command::Quote);
        assert_eq!(e.replacement, "> one\n> \n> two");
    }

    #[test]
    fn a_blank_selection_gains_a_marker() {
        let e = edit("", 0, 0, 0, Command::Bullet);
        assert_eq!(e.replacement, "- ");
    }

    #[test]
    fn selection_covers_the_rewritten_lines_and_caret_keeps_its_column() {
        let e = edit("one\ntwo", 0, 7, 7, Command::Bullet);
        assert_eq!(
            e.after,
            Placement::Selection {
                anchor: 0,
                caret: 11
            }
        );
        let e = edit("one", 1, 1, 1, Command::Bullet);
        assert_eq!(e.after, Placement::Caret(3));
    }

    #[test]
    fn unchanged_line_commands_are_no_ops() {
        assert!(command("", 0, 0, 0, Command::Quote).is_some());
        assert!(command("- one", 0, 0, 0, Command::Bullet).is_some());
        // A quote is already exact: the command removes it.
        assert!(command("> one", 0, 0, 0, Command::Quote).is_some());
    }

    #[test]
    fn task_box_length_is_used_by_the_marker_parser() {
        assert_eq!(crate::blocks::task_box_len("[ ] task"), Some(4));
        assert!(parse_list(b"- [ ] task").is_some_and(|item| item.task));
        assert!(parse_list(b"- [x] task").is_some_and(|item| item.task));
    }
    #[test]
    fn multibyte_neighbours_do_not_panic_and_multi_paragraph_inline_is_refused() {
        let e = edit("日本語", 3, 6, 6, Command::Bold);
        assert_eq!(e.range, 3..6);
        assert_eq!(e.replacement, "**本**");
        assert!(command("😀word", 4, 8, 8, Command::Bold).is_some());

        assert!(command("one\n\ntwo", 0, 8, 8, Command::Bold).is_none());
        assert!(command("one\n  \ntwo", 0, 10, 10, Command::Italic).is_none());
    }

    #[test]
    fn a_caret_inside_a_pair_toggles_it_off() {
        let e = edit("**word**", 4, 4, 4, Command::Bold);
        assert_eq!(e.range, 0..8);
        assert_eq!(e.replacement, "word");
        assert_eq!(e.after, Placement::Caret(2));

        let e = edit("_word_", 3, 3, 3, Command::Italic);
        assert_eq!(e.replacement, "word");
        assert_eq!(e.after, Placement::Caret(2));

        let e = edit("`code`", 3, 3, 3, Command::Code);
        assert_eq!(e.replacement, "code");
    }

    #[test]
    fn a_reverse_selection_keeps_its_direction() {
        let e = edit("hello world", 6, 11, 6, Command::Bold);
        assert_eq!(
            e.after,
            Placement::Selection {
                anchor: 13,
                caret: 8
            }
        );
    }

    #[test]
    fn seven_hashes_are_a_paragraph_not_a_heading() {
        let e = edit("####### x", 0, 0, 0, Command::Heading(1));
        assert_eq!(e.replacement, "# ####### x");
    }

    #[test]
    fn line_commands_at_document_end_place_the_caret_at_the_new_end() {
        let e = edit("hello", 5, 5, 5, Command::Heading(1));
        assert_eq!(e.replacement, "# hello");
        assert_eq!(e.after, Placement::Caret(7));

        let e = edit("task", 4, 4, 4, Command::Task);
        assert_eq!(e.replacement, "- [ ] task");
        assert_eq!(e.after, Placement::Caret(10));
    }

    #[test]
    fn collapsed_caret_between_formatted_spans_inserts_a_new_pair_instead_of_merging() {
        let e = edit("**foo** bar **baz**", 9, 9, 9, Command::Bold);
        assert_eq!(e.range, 9..9);
        assert_eq!(e.replacement, "****");
        assert_eq!(e.after, Placement::Caret(11));

        let e = edit("**foo** bar **baz**", 15, 15, 15, Command::Bold);
        assert_eq!(e.range, 12..19);
        assert_eq!(e.replacement, "baz");
        assert_eq!(e.after, Placement::Caret(13));

        let e = edit("**foo**\n\nbar\n\n**baz**", 10, 10, 10, Command::Bold);
        assert_eq!(e.range, 10..10);
        assert_eq!(e.replacement, "****");
        assert_eq!(e.after, Placement::Caret(12));
    }
}
