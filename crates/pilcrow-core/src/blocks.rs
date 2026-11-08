//! One source line is one block. That is the decision that keeps this editor
//! honest: what is in the file is what is on screen, the caret's line is the
//! file's line, and nothing is reflowed into a shape the bytes do not have.
//!
//! The scanner is a single pass and allocation-light, because it re-runs on
//! every edit. Markdown runs merge: consecutive paragraph lines, consecutive
//! quote lines, and a line indented under a list item. Fenced code and
//! frontmatter are the only blocks that span lines by their own syntax.
//!
//! Only `\n` separates lines. A `\r` before it is content at this layer; the
//! loader normalises CRLF before the document reaches the editor.

/// What a block looks like to the layout and the reading copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Blank,
    Paragraph,
    Heading,
    Code,
    Quote,
    ListItem,
    Rule,
    Frontmatter,
}

/// One source line, or one merged markdown run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: Kind,
    /// Heading level, 1 to 6; 0 for everything else.
    pub level: u8,
    /// First byte of the line, indentation included.
    pub start: usize,
    /// One past the last content byte, before the newline.
    pub end: usize,
    /// Bytes of syntax at the front (`"## "`, `"> "`, `"- "`, "```"), which
    /// are drawn dimmer than the words. Never includes the words themselves.
    pub marker: usize,
}

fn is_space(ch: u8) -> bool {
    ch == b' ' || ch == b'\t'
}

/// Indentation up to three spaces: the most a heading, quote or fence may
/// carry and still be itself. A tab counts as four columns, so it is never
/// consumed here.
fn count_indent(line: &[u8]) -> usize {
    let mut i = 0;
    while i < line.len() && i < 3 && line[i] == b' ' {
        i += 1;
    }
    i
}

/// All leading whitespace, uncapped: a list may be nested under any amount of
/// indentation, and deeper-indented items are items of their own rather than
/// run-on text of the parent.
fn count_indent_full(line: &[u8]) -> usize {
    let mut i = 0;
    while i < line.len() && is_space(line[i]) {
        i += 1;
    }
    i
}

/// A line's indentation measured in columns, where a tab is four: the width
/// the editor's Tab key writes. A tabbed continuation line is a continuation
/// (one tab is four columns under the item), not a new paragraph one column
/// in.
fn indent_cols(line: &[u8]) -> usize {
    let mut cols = 0;
    for &ch in line {
        match ch {
            b' ' => cols += 1,
            b'\t' => cols += 4,
            _ => break,
        }
    }
    cols
}

/// The length of a fence run at the start of `rest`, or `None`.
fn fence_run(rest: &[u8]) -> Option<usize> {
    let first = *rest.first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let mut n = 0;
    while n < rest.len() && rest[n] == first {
        n += 1;
    }
    if n >= 3 { Some(n) } else { None }
}

/// A closing fence matches the opener's character and is at least as long.
/// An info string on the line makes it content, not a closer.
fn closes_fence(rest: &[u8], ch: u8, len: usize) -> bool {
    if rest.len() < len || rest[0] != ch {
        return false;
    }
    let mut n = 0;
    while n < rest.len() && rest[n] == ch {
        n += 1;
    }
    if n < len {
        return false;
    }
    rest[n..].iter().all(|&c| is_space(c))
}

fn heading_level(rest: &[u8]) -> u8 {
    let mut n = 0;
    while n < rest.len() && rest[n] == b'#' {
        n += 1;
    }
    if n == 0 || n > 6 {
        return 0;
    }
    // A bare run of hashes has no heading text: `#` alone is a paragraph.
    // `# ` is a heading with an empty line under it, exactly as the file says.
    if n == rest.len() || !is_space(rest[n]) {
        return 0;
    }
    n as u8
}

fn is_rule(rest: &[u8]) -> bool {
    let Some(&ch) = rest.first() else {
        return false;
    };
    if ch != b'-' && ch != b'*' && ch != b'_' {
        return false;
    }
    let mut n = 0;
    for &c in rest {
        if c == ch {
            n += 1;
        } else if !is_space(c) {
            return false;
        }
    }
    n >= 3
}

/// The list marker's byte length at the head of `rest`: the bullet or number
/// with its punctuation and the space that ends it. A task box is content to
/// the block scanner; the layout adds its width to the list hang separately.
fn list_marker(rest: &[u8]) -> Option<usize> {
    let marker = if rest.len() >= 2 && matches!(rest[0], b'-' | b'*' | b'+') && is_space(rest[1]) {
        if rest.len() >= 3 && is_space(rest[2]) {
            3
        } else {
            2
        }
    } else {
        // "1. " or "1) "
        let mut i = 0;
        while i < rest.len() && rest[i].is_ascii_digit() {
            i += 1;
        }
        if i == 0 || i >= rest.len() {
            return None;
        }
        if rest[i] != b'.' && rest[i] != b')' {
            return None;
        }
        if i + 1 >= rest.len() || !is_space(rest[i + 1]) {
            return None;
        }
        // The second space is the marker's, as with a bullet: the item's
        // words start after it.
        if i + 3 <= rest.len() && is_space(rest[i + 2]) {
            i + 3
        } else {
            i + 2
        }
    };

    Some(marker)
}

/// The byte length of a task box at the start of a list item's content:
/// `[ ] `, `[x] ` or `[X] ` (three bytes plus a trailing space when present).
/// This is layout information, not a block marker; the checkbox stays content.
pub fn task_box_len(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    if bytes.len() < 3
        || bytes[0] != b'['
        || !matches!(bytes[1], b' ' | b'x' | b'X')
        || bytes[2] != b']'
    {
        return None;
    }
    if bytes.len() > 3 && !is_space(bytes[3]) {
        return None;
    }
    Some(if bytes.len() > 3 { 4 } else { 3 })
}

/// A list marker anywhere in the leading whitespace: the byte length from the
/// line's start to the end of the marker, or `None` when the content is not a
/// list.
fn list_marker_len(line: &[u8]) -> Option<usize> {
    let indent = count_indent_full(line);
    let len = list_marker(&line[indent..])?;
    Some(indent + len)
}

/// How one line inside a frontmatter block reads. The scanner and the
/// pre-check that decides whether an opening `---` is metadata both step
/// through the frontmatter with this, so they cannot disagree about where it
/// ends: a `---` inside a fenced block is metadata's content, and an indented
/// `---` is a YAML literal block's content rather than the closing fence.
#[derive(Default)]
struct FrontScan {
    fence: u8,
    fence_len: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrontLine {
    FenceOpen,
    FenceClose,
    Close,
    Meta,
    Plain,
}

impl FrontScan {
    fn step(&mut self, line: &[u8]) -> FrontLine {
        let mut trimmed_end = line.len();
        while trimmed_end > 0 && is_space(line[trimmed_end - 1]) {
            trimmed_end -= 1;
        }
        let indent = count_indent(line).min(trimmed_end);
        let rest = &line[indent..];

        if self.fence != 0 {
            if closes_fence(rest, self.fence, self.fence_len) {
                self.fence = 0;
                return FrontLine::FenceClose;
            }
            return FrontLine::Plain;
        }

        if let Some(n) = fence_run(rest) {
            self.fence = rest[0];
            self.fence_len = n;
            return FrontLine::FenceOpen;
        }

        // The closing fence is at column zero; an indented one belongs to a
        // literal block above it.
        let head = &line[..trimmed_end];
        if indent == 0 && (head == b"---" || head == b"...") {
            return FrontLine::Close;
        }

        if line[indent..trimmed_end].contains(&b':') {
            return FrontLine::Meta;
        }
        FrontLine::Plain
    }
}

/// True when a frontmatter fence opened before `from` has a closing line
/// later in the document. A note that merely starts with a horizontal rule
/// must not be swallowed whole by the frontmatter state: closing the fence
/// and looking like metadata (or containing a fence) is what makes it
/// metadata.
fn frontmatter_closes(text: &[u8], from: usize) -> bool {
    let mut i = from;
    let mut lines = 0;
    let mut has_meta = false;
    let mut has_fence = false;
    let mut front = FrontScan::default();

    while i <= text.len() && lines < 15 {
        let nl = text[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(text.len(), |offset| i + offset);
        match front.step(&text[i..nl]) {
            FrontLine::FenceOpen => has_fence = true,
            FrontLine::Close => return has_meta || has_fence,
            FrontLine::Meta => has_meta = true,
            FrontLine::FenceClose | FrontLine::Plain => {}
        }
        if nl == text.len() {
            break;
        }
        i = nl + 1;
        lines += 1;
    }
    false
}

/// Scans `text` into `out`, replacing whatever was there.
///
/// Every source line is covered exactly once. A merged run spans the newlines
/// inside it; its `end` is still the end of its last line, before that line's
/// terminating newline. Empty text is one empty [`Kind::Blank`] block, and a
/// trailing newline leaves a final empty block, so the block table always
/// tiles the document.
pub fn scan(text: &str, out: &mut Vec<Block>) {
    out.clear();

    let bytes = text.as_bytes();
    let mut scanner = Scanner::new(true);
    let mut i = 0;
    loop {
        let nl = bytes[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |offset| i + offset);
        scanner.line(bytes, i, nl, out);
        if nl == bytes.len() {
            break;
        }
        i = nl + 1;
    }
}

/// The line-by-line state behind [`scan`]: whether a fence or frontmatter is
/// open, and whether the next line is the document's first. Kept as a steppable
/// unit so an incremental rescan can run the exact same classification from a
/// mid-document block boundary and stop once it resynchronises with the
/// previous block table.
struct Scanner {
    in_fence: bool,
    fence_char: u8,
    fence_len: usize,
    in_front: bool,
    front: FrontScan,
    first_line: bool,
}

impl Scanner {
    fn new(first_line: bool) -> Self {
        Self {
            in_fence: false,
            fence_char: 0,
            fence_len: 0,
            in_front: false,
            front: FrontScan::default(),
            first_line,
        }
    }

    /// No fence and no frontmatter is open, so the next line's classification
    /// depends only on its own bytes and the previous block.
    fn neutral(&self) -> bool {
        !self.in_fence && !self.in_front
    }

    /// Classify the line `bytes[i..nl]`, merging it into the last block in
    /// `out` or pushing a new one. Returns true when a new block was pushed.
    fn line(&mut self, bytes: &[u8], i: usize, nl: usize, out: &mut Vec<Block>) -> bool {
        let first_line = self.first_line;
        self.first_line = false;
        let line = &bytes[i..nl];
        let mut trimmed_end = line.len();
        while trimmed_end > 0 && is_space(line[trimmed_end - 1]) {
            trimmed_end -= 1;
        }
        // A line of nothing but spaces has count_indent == line.len(), past
        // trimmed_end. Slicing [indent..trimmed_end] then panicked on the
        // first space typed into an empty note.
        let indent = count_indent(line).min(trimmed_end);
        let rest = &line[indent..];
        let trimmed = &line[indent..trimmed_end];

        let mut block = Block {
            kind: Kind::Paragraph,
            level: 0,
            start: i,
            end: nl,
            marker: 0,
        };

        if first_line && trimmed == b"---" && frontmatter_closes(bytes, nl + 1) {
            self.in_front = true;
        }

        if self.in_front {
            block.kind = Kind::Frontmatter;
            // The same state machine the pre-check used, so both agree on
            // which line closes the metadata.
            if !first_line && self.front.step(line) == FrontLine::Close {
                self.in_front = false;
            }
        } else if self.in_fence {
            block.kind = Kind::Code;
            if closes_fence(rest, self.fence_char, self.fence_len) {
                // Mark the closing fence so the reading view can drop it
                // without dropping a code line that merely looks like one.
                block.marker = indent + self.fence_len;
                self.in_fence = false;
            }
        } else if let Some(n) = fence_run(rest) {
            block.kind = Kind::Code;
            block.marker = indent + n;
            self.fence_char = rest[0];
            self.fence_len = n;
            self.in_fence = true;
        } else if trimmed.is_empty() {
            block.kind = Kind::Blank;
        } else if is_rule(trimmed) {
            block.kind = Kind::Rule;
        } else if heading_level(rest) > 0 {
            let level = heading_level(rest);
            block.kind = Kind::Heading;
            block.level = level;
            let level_at = level as usize;
            block.marker =
                indent + level_at + usize::from(rest.len() > level_at && is_space(rest[level_at]));
        } else if rest[0] == b'>' {
            block.kind = Kind::Quote;
            block.marker = indent + 1 + usize::from(rest.len() > 1 && is_space(rest[1]));
        } else if let Some(marker) = list_marker_len(line) {
            // Lists nest: a marker under any amount of indentation is an
            // item of its own, never run-on text of the item above it.
            block.kind = Kind::ListItem;
            block.marker = marker;
        } else {
            block.kind = Kind::Paragraph;
        }

        // Markdown's paragraph is a run of lines until a blank one: without
        // this, a hard-wrapped note becomes one paragraph per line and every
        // wrap in the source gains a paragraph gap it never asked for.
        // A run of `>` lines is one blockquote, the way a run of plain lines
        // is one paragraph. Left alone, a quote hard-wrapped at eighty columns
        // renders as a stack of separate quotes with gaps between them.
        if block.kind == Kind::Quote
            && let Some(prev) = out.last_mut()
            && prev.kind == Kind::Quote
            && prev.end + 1 == block.start
        {
            prev.end = block.end;
            return false;
        }

        // A continuation line carries no marker of its own; it belongs to the
        // list item above it when it is indented under it (in columns, so a
        // tab counts as the four the Tab key writes), exactly as it belongs to
        // the paragraph above it when it is not.
        if block.kind == Kind::Paragraph
            && let Some(prev) = out.last_mut()
            && (prev.kind == Kind::Paragraph
                || (prev.kind == Kind::ListItem && indent_cols(line) >= 2)
                || prev.kind == Kind::Quote)
            && prev.end + 1 == block.start
            && block.marker == 0
        {
            prev.end = block.end;
            return false;
        }

        out.push(block);
        true
    }
}

/// Rescans `text` starting at block `from_block` of the previous scan `old`,
/// stopping as soon as the two tables resynchronise. The result in `out` is
/// exactly what [`scan`] would produce for `text`; the return value is the
/// byte offset scanning reached, which is `text.len()` when no resync point
/// was found.
///
/// The contract mirrors the editor's invalidation:
///
/// - `text` is the document `old` was scanned from, with one contiguous
///   replacement applied. `edit_end` is the byte offset in `text` just past
///   the inserted replacement.
/// - Bytes before `old[from_block].start` are identical in both documents,
///   and the scanner is at a neutral boundary there in the old scan: the
///   block is not inside a fence or frontmatter run (an opening fence line
///   is fine; the caller walks back to it).
///
/// Resync is deliberately conservative. Scanning stops only at a blank block
/// at or after `edit_end` — blank lines break every merge run and only occur
/// outside fences and frontmatter — when that blank and the block before it
/// both align with consecutive old blocks under the document's single byte
/// shift (`text.len()` minus the old length). Everything after such a blank
/// in the new text is the old suffix shifted by that amount, so the old
/// blocks are spliced in with their offsets adjusted. An edit whose effects
/// run to the end of the note (an unclosed fence, say) simply never resyncs
/// and costs a full suffix scan, exactly as before.
pub fn rescan_from(
    text: &str,
    old: &[Block],
    from_block: usize,
    edit_end: usize,
    out: &mut Vec<Block>,
) -> usize {
    out.clear();
    let from_block = from_block.min(old.len());
    out.extend_from_slice(&old[..from_block]);

    let bytes = text.as_bytes();
    let scan_byte = old
        .get(from_block)
        .map_or(0, |block| block.start)
        .min(bytes.len());
    let old_len = old.last().map_or(0, |block| block.end);
    let shift = bytes.len() as i128 - old_len as i128;

    let mut scanner = Scanner::new(from_block == 0);
    let mut i = scan_byte;
    loop {
        let nl = bytes[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |offset| i + offset);
        let pushed = scanner.line(bytes, i, nl, out);
        if pushed
            && scanner.neutral()
            && let Some(blank) = out.last()
            && blank.kind == Kind::Blank
            && blank.start >= edit_end
            && splice_at(old, out, shift, edit_end)
        {
            return if nl == bytes.len() { nl } else { nl + 1 };
        }
        if nl == bytes.len() {
            return bytes.len();
        }
        i = nl + 1;
    }
}

/// Tries to finish `out` (whose last block is a blank line) by splicing in the
/// old blocks that follow the blank's counterpart, shifted by `shift`.
/// Returns false and leaves `out` alone when the alignment does not hold for
/// both the blank and the block before it.
fn splice_at(old: &[Block], out: &mut Vec<Block>, shift: i128, edit_end: usize) -> bool {
    let blank = *out.last().expect("a blank block was just pushed");
    let Some(prev) = out.len().checked_sub(2).map(|index| out[index]) else {
        return false;
    };
    // Both aligned blocks must sit fully inside the shifted old suffix: a
    // block that straddles the end of the replacement may have been reshaped
    // by it, so its agreement with an old block proves nothing yet.
    if prev.start < edit_end {
        return false;
    }
    let want = blank.start as i128 - shift;
    if want < 0 {
        return false;
    }
    let want = want as usize;
    let index = old.partition_point(|block| block.start < want);
    let (Some(old_blank), Some(old_prev)) = (
        old.get(index).filter(|block| block.start == want),
        index.checked_sub(1).and_then(|prev| old.get(prev)),
    ) else {
        return false;
    };
    let aligned = |new: &Block, old: &Block| {
        new.kind == old.kind
            && new.level == old.level
            && new.marker == old.marker
            && new.start as i128 - old.start as i128 == shift
            && new.end as i128 - old.end as i128 == shift
    };
    if old_blank.kind != Kind::Blank || !aligned(&blank, old_blank) || !aligned(&prev, old_prev) {
        return false;
    }
    out.reserve(old.len() - index - 1);
    for block in &old[index + 1..] {
        out.push(Block {
            start: (block.start as i128 + shift) as usize,
            end: (block.end as i128 + shift) as usize,
            ..*block
        });
    }
    true
}

/// The first block index at which two scans of the same document differ in
/// shape: kind, heading level, marker length, or block length.
///
/// Byte positions are allowed to shift by a constant between aligned blocks,
/// so inserting a character in an early block does not make every later block
/// look changed. This is deliberately conservative about *content*: a
/// same-length word replacement leaves every block field identical, so it
/// returns `new.len()` and the caller must also pass the block containing the
/// edit. The reliable invalidation point is:
///
/// ```text
/// truncate_from(min(edit_block, first_changed(old, new)).saturating_sub(1))
/// ```
///
/// The extra block of run-up keeps a merge or split from starting after the
/// boundary that changed. A paragraph, quote, list continuation, heading,
/// fence or frontmatter change always shows up here because it changes one of
/// the compared fields. If structurally identical blocks move across an
/// insertion, the index is inherently ambiguous; the caller's `edit_block`
/// resolves that ambiguity through the `min`.
pub fn first_changed(old: &[Block], new: &[Block]) -> usize {
    let mut index = 0;
    while index < old.len() && index < new.len() {
        let old_block = old[index];
        let new_block = new[index];
        let shift = new_block.start as i128 - old_block.start as i128;
        let same = old_block.kind == new_block.kind
            && old_block.level == new_block.level
            && old_block.marker == new_block.marker
            && new_block.end as i128 - old_block.end as i128 == shift;
        if !same {
            return index;
        }
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::{Block, Kind, first_changed, rescan_from, scan, task_box_len};

    fn scan_text(text: &str) -> Vec<Block> {
        let mut out = Vec::new();
        scan(text, &mut out);
        out
    }

    fn kinds(text: &str) -> Vec<Kind> {
        scan_text(text)
            .into_iter()
            .map(|block| block.kind)
            .collect()
    }

    fn spans<'a>(text: &'a str, blocks: &[Block]) -> Vec<&'a str> {
        blocks
            .iter()
            .map(|block| &text[block.start..block.end])
            .collect()
    }

    #[test]
    fn empty_document_is_one_blank_block() {
        let blocks = scan_text("");
        assert_eq!(
            blocks,
            [Block {
                kind: Kind::Blank,
                level: 0,
                start: 0,
                end: 0,
                marker: 0,
            }]
        );
    }

    #[test]
    fn scan_reuses_and_clears_the_output() {
        let mut out = vec![Block {
            kind: Kind::Rule,
            level: 0,
            start: 9,
            end: 9,
            marker: 0,
        }];
        scan("word", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, Kind::Paragraph);
        assert_eq!((out[0].start, out[0].end), (0, 4));
    }

    #[test]
    fn whitespace_only_lines_are_blank_not_a_crash() {
        let text = "   \n\t\t";
        let blocks = scan_text(text);
        assert_eq!(kinds(text), [Kind::Blank, Kind::Blank]);
        assert_eq!((blocks[0].start, blocks[0].end), (0, 3));
        assert_eq!((blocks[1].start, blocks[1].end), (4, 6));
    }

    #[test]
    fn consecutive_lines_of_prose_are_one_paragraph_block() {
        let text = "one\ntwo\nthree";
        let blocks = scan_text(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, Kind::Paragraph);
        assert_eq!((blocks[0].start, blocks[0].end), (0, text.len()));
        assert_eq!(blocks[0].marker, 0);
    }

    #[test]
    fn a_blank_line_ends_a_paragraph() {
        let text = "one\ntwo\n\nthree";
        let blocks = scan_text(text);
        assert_eq!(kinds(text), [Kind::Paragraph, Kind::Blank, Kind::Paragraph]);
        assert_eq!(spans(text, &blocks), ["one\ntwo", "", "three"]);
    }

    #[test]
    fn a_trailing_newline_leaves_a_final_blank_block() {
        let text = "one\n";
        let blocks = scan_text(text);
        assert_eq!(kinds(text), [Kind::Paragraph, Kind::Blank]);
        assert_eq!(spans(text, &blocks), ["one", ""]);
        assert_eq!((blocks[1].start, blocks[1].end), (4, 4));
    }

    #[test]
    fn headings_carry_their_level_and_dim_only_the_marker() {
        let text = "## A title";
        let blocks = scan_text(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, Kind::Heading);
        assert_eq!(blocks[0].level, 2);
        assert_eq!(&text[blocks[0].start..blocks[0].marker], "## ");
        assert_eq!(&text[blocks[0].marker..blocks[0].end], "A title");
    }

    #[test]
    fn headings_cover_levels_one_through_six() {
        let text = "# one\n## two\n### three\n#### four\n##### five\n###### six";
        let blocks = scan_text(text);
        assert_eq!(blocks.len(), 6);
        for (index, block) in blocks.iter().enumerate() {
            let level = index + 1;
            assert_eq!(block.kind, Kind::Heading);
            assert_eq!(block.level as usize, level);
            assert_eq!(block.marker, level + 1);
        }
    }

    #[test]
    fn a_hash_without_a_space_is_not_a_heading() {
        for text in ["#nope", "####### seven", "    # code-like"] {
            let blocks = scan_text(text);
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].kind, Kind::Paragraph, "{text:?}");
            assert_eq!(blocks[0].level, 0);
            assert_eq!(blocks[0].marker, 0);
        }
    }

    #[test]
    fn a_bare_hash_is_not_a_heading_but_a_hash_and_space_is() {
        let bare = scan_text("#");
        assert_eq!(bare[0].kind, Kind::Paragraph);
        assert_eq!(bare[0].marker, 0);

        let spaced = scan_text("# ");
        assert_eq!(spaced[0].kind, Kind::Heading);
        assert_eq!(spaced[0].level, 1);
        assert_eq!(spaced[0].marker, 2);
    }

    #[test]
    fn indentation_up_to_three_spaces_stays_part_of_the_heading_marker() {
        let text = "  ## x";
        let blocks = scan_text(text);
        assert_eq!(blocks[0].kind, Kind::Heading);
        assert_eq!(blocks[0].level, 2);
        assert_eq!(blocks[0].marker, 5); // "  ## "
    }

    #[test]
    fn quotes_merge_and_keep_their_markers_on_their_own_lines() {
        let text = "> first line\n> second line\n\nafter";
        let blocks = scan_text(text);
        assert_eq!(kinds(text), [Kind::Quote, Kind::Blank, Kind::Paragraph]);
        assert_eq!(
            &text[blocks[0].start..blocks[0].end],
            "> first line\n> second line"
        );
        assert_eq!(blocks[0].marker, 2);
    }

    #[test]
    fn a_lazy_line_after_a_quote_stays_in_the_quote() {
        let text = "> a\nb";
        let blocks = scan_text(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, Kind::Quote);
        assert_eq!(spans(text, &blocks), ["> a\nb"]);
    }

    #[test]
    fn a_quoted_blank_line_keeps_the_blockquote_whole() {
        let text = "> a\n> \n> b";
        let blocks = scan_text(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, Kind::Quote);
        assert_eq!(spans(text, &blocks), ["> a\n> \n> b"]);
    }

    #[test]
    fn an_indented_line_after_a_quote_also_continues_it() {
        let text = "> a\n  b";
        let blocks = scan_text(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, Kind::Quote);
        assert_eq!(spans(text, &blocks), ["> a\n  b"]);
    }

    #[test]
    fn task_boxes_are_separate_layout_content() {
        assert_eq!(task_box_len("[ ] task"), Some(4));
        assert_eq!(task_box_len("[x] done"), Some(4));
        assert_eq!(task_box_len("[X] done"), Some(4));
        assert_eq!(task_box_len("[ ]"), Some(3));
        assert_eq!(task_box_len("[ ]x"), None);
        assert_eq!(task_box_len("plain"), None);
    }

    #[test]
    fn list_markers_cover_bullets_ordered_items_and_tasks() {
        let cases = [
            ("- item", 2),
            ("* item", 2),
            ("+ item", 2),
            ("1. first", 3),
            ("1) first", 3),
            ("10) item", 4),
            ("-  item", 3),
            ("1.  item", 4),
            (" - item", 3),
            ("\t- a", 3),
            ("- [ ] task", 2),
            ("- [x] done", 2),
            ("- [X] done", 2),
            ("- [ ]", 2),
            ("- [ ]x", 2),
        ];
        for (text, marker) in cases {
            let blocks = scan_text(text);
            assert_eq!(blocks.len(), 1, "{text:?}");
            assert_eq!(blocks[0].kind, Kind::ListItem, "{text:?}");
            assert_eq!(blocks[0].marker, marker, "{text:?}");
            assert_eq!(blocks[0].level, 0, "{text:?}");
        }
    }

    #[test]
    fn a_deeply_indented_list_line_is_its_own_item() {
        let text = "- parent\n    - nested\n- next";
        let blocks = scan_text(text);
        assert_eq!(blocks.len(), 3);
        assert_eq!(kinds(text), [Kind::ListItem; 3]);
        assert_eq!(spans(text, &blocks), ["- parent", "    - nested", "- next"]);
        assert_eq!(blocks[1].marker, 6);
    }

    #[test]
    fn list_continuations_merge_only_when_indented_under_the_item() {
        let text = "- a\n  b\nc";
        let blocks = scan_text(text);
        assert_eq!(kinds(text), [Kind::ListItem, Kind::Paragraph]);
        assert_eq!(spans(text, &blocks), ["- a\n  b", "c"]);
    }

    #[test]
    fn tabs_count_as_four_columns_for_continuation_and_nesting() {
        let continued = scan_text("- a\n\tb");
        assert_eq!(continued.len(), 1);
        assert_eq!(continued[0].kind, Kind::ListItem);
        assert_eq!(continued[0].marker, 2);

        let tabbed = scan_text("\t- a");
        assert_eq!(tabbed[0].kind, Kind::ListItem);
        assert_eq!(tabbed[0].marker, 3);

        assert_eq!(kinds("- a\n b"), [Kind::ListItem, Kind::Paragraph]);
    }

    #[test]
    fn a_short_dash_run_is_a_list_item_not_a_rule() {
        let text = "- - -\n***\n___\n- -";
        let blocks = scan_text(text);
        assert_eq!(
            kinds(text),
            [Kind::Rule, Kind::Rule, Kind::Rule, Kind::ListItem]
        );
        for block in &blocks[..3] {
            assert_eq!(block.marker, 0);
        }
        assert_eq!(blocks[3].marker, 2);
    }

    #[test]
    fn a_fence_makes_every_line_inside_it_code() {
        let text = "```rust\nconst x = 1;\n```\nafter";
        let blocks = scan_text(text);
        assert_eq!(
            kinds(text),
            [Kind::Code, Kind::Code, Kind::Code, Kind::Paragraph]
        );
        assert_eq!(blocks[0].marker, 3);
        assert_eq!(blocks[1].marker, 0);
        assert_eq!(blocks[2].marker, 3);
    }

    #[test]
    fn tilde_fences_work_and_unclosed_fences_run_to_eof() {
        let text = "~~~\nx\n~~~";
        let blocks = scan_text(text);
        assert_eq!(kinds(text), [Kind::Code; 3]);
        assert_eq!(blocks[0].marker, 3);
        assert_eq!(blocks[2].marker, 3);

        let open = "```\nx\nstill code";
        assert_eq!(kinds(open), [Kind::Code; 3]);
    }

    #[test]
    fn a_fence_closes_only_on_its_own_character_length_and_no_info_string() {
        let text = "````\n```js\nconst x = 1;\n````\nafter";
        let blocks = scan_text(text);
        assert_eq!(
            kinds(text),
            [
                Kind::Code,
                Kind::Code,
                Kind::Code,
                Kind::Code,
                Kind::Paragraph
            ]
        );
        assert_eq!(blocks[0].marker, 4);
        assert_eq!(blocks[3].marker, 4);

        let backtick = "~~~\na\n```\nb\n~~~";
        assert_eq!(kinds(backtick), [Kind::Code; 5]);
    }

    #[test]
    fn a_closing_fence_may_be_longer_than_its_opener() {
        let text = "```\nx\n`````";
        let blocks = scan_text(text);
        assert_eq!(kinds(text), [Kind::Code; 3]);
        // The marker covers the opener's length, as the scanner has always
        // measured it; the extra fence bytes are still part of the line.
        assert_eq!(blocks[2].marker, 3);
    }

    #[test]
    fn a_three_space_fence_is_code_but_a_four_space_one_is_paragraph() {
        let indented = "   ```\n   code\n   ```";
        let blocks = scan_text(indented);
        assert_eq!(kinds(indented), [Kind::Code; 3]);
        assert_eq!(blocks[0].marker, 6);
        assert_eq!(blocks[2].marker, 6);

        let too_deep = "    ```\n    not code\n    ```";
        assert_eq!(kinds(too_deep), [Kind::Paragraph]);
    }

    #[test]
    fn frontmatter_runs_to_its_closing_fence() {
        let text = "---\ntitle: x\n---\nbody";
        let blocks = scan_text(text);
        assert_eq!(
            kinds(text),
            [
                Kind::Frontmatter,
                Kind::Frontmatter,
                Kind::Frontmatter,
                Kind::Paragraph,
            ]
        );
        assert_eq!(spans(text, &blocks)[3], "body");
    }

    #[test]
    fn frontmatter_may_close_with_three_dots() {
        let text = "---\ntitle: x\n...\nbody";
        let blocks = scan_text(text);
        assert_eq!(blocks[2].kind, Kind::Frontmatter);
        assert_eq!(blocks[3].kind, Kind::Paragraph);
    }

    #[test]
    fn frontmatter_that_never_closes_is_an_ordinary_rule_and_words() {
        let text = "---\ntitle: x";
        let blocks = scan_text(text);
        assert_eq!(kinds(text), [Kind::Rule, Kind::Paragraph]);
        assert_eq!(spans(text, &blocks), ["---", "title: x"]);
    }

    #[test]
    fn a_dash_rule_pair_with_no_metadata_is_not_frontmatter() {
        let text = "---\n---\nbody";
        assert_eq!(kinds(text), [Kind::Rule, Kind::Rule, Kind::Paragraph]);
    }

    #[test]
    fn frontmatter_opens_only_on_the_first_line() {
        let text = "first\n---\ntitle: x\n---";
        assert_eq!(
            kinds(text),
            [Kind::Paragraph, Kind::Rule, Kind::Paragraph, Kind::Rule]
        );
    }

    #[test]
    fn a_dash_inside_a_fenced_frontmatter_block_is_content() {
        let text = "---\nmeta: |\n  ```\n  ---\n  ```\n---\nbody";
        let blocks = scan_text(text);
        for block in &blocks[..6] {
            assert_eq!(block.kind, Kind::Frontmatter);
        }
        assert_eq!(blocks[6].kind, Kind::Paragraph);
        assert_eq!(spans(text, &blocks)[6], "body");
    }

    #[test]
    fn an_indented_dash_is_literal_content_not_a_closing_fence() {
        let text = "---\ndesc: |\n  ---\n  kept\n---\nbody";
        let blocks = scan_text(text);
        for block in &blocks[..5] {
            assert_eq!(block.kind, Kind::Frontmatter);
        }
        assert_eq!(blocks[5].kind, Kind::Paragraph);
        assert_eq!(spans(text, &blocks)[5], "body");
    }

    #[test]
    fn a_fence_makes_the_frontmatter_metadata() {
        let text = "---\n```\nx\n```\n---\nafter";
        let blocks = scan_text(text);
        for block in &blocks[..5] {
            assert_eq!(block.kind, Kind::Frontmatter);
        }
        assert_eq!(blocks[5].kind, Kind::Paragraph);
    }

    #[test]
    fn a_leading_rule_is_not_frontmatter_just_because_a_later_rule_exists() {
        let text = "---\nFirst section\n\n---\nSecond section";
        assert_eq!(
            kinds(text),
            [
                Kind::Rule,
                Kind::Paragraph,
                Kind::Blank,
                Kind::Rule,
                Kind::Paragraph
            ]
        );
    }

    #[test]
    fn frontmatter_must_close_within_fifteen_lines() {
        let mut closes = String::from("---\n");
        for line in 0..13 {
            closes.push_str(&format!("key{line}: value\n"));
        }
        closes.push_str("---\nbody");
        let blocks = scan_text(&closes);
        assert_eq!(blocks[0].kind, Kind::Frontmatter);
        assert_eq!(blocks[14].kind, Kind::Frontmatter);
        assert_eq!(blocks[15].kind, Kind::Paragraph);

        let mut too_long = String::from("---\n");
        for line in 0..15 {
            too_long.push_str(&format!("key{line}: value\n"));
        }
        too_long.push_str("---\nbody");
        let blocks = scan_text(&too_long);
        assert_eq!(blocks[0].kind, Kind::Rule);
    }

    #[test]
    fn a_dash_inside_a_code_fence_is_code() {
        let text = "```\n---\n```";
        assert_eq!(kinds(text), [Kind::Code; 3]);
    }

    #[test]
    fn blocks_tile_the_document_in_order_with_no_gaps_or_overlaps() {
        let text = "---\ntitle: x\n---\n# H\n\npara one\npara two\n\n- item\n  more\n\n> q\n> r\n\n```\ncode\n```\n\n---";
        let blocks = scan_text(text);
        assert_eq!(
            kinds(text),
            [
                Kind::Frontmatter,
                Kind::Frontmatter,
                Kind::Frontmatter,
                Kind::Heading,
                Kind::Blank,
                Kind::Paragraph,
                Kind::Blank,
                Kind::ListItem,
                Kind::Blank,
                Kind::Quote,
                Kind::Blank,
                Kind::Code,
                Kind::Code,
                Kind::Code,
                Kind::Blank,
                Kind::Rule,
            ]
        );

        let bytes = text.as_bytes();
        let mut at = 0;
        for block in &blocks {
            assert_eq!(block.start, at);
            assert!(block.end >= block.start);
            assert!(block.end <= text.len());
            assert!(block.marker <= block.end - block.start);
            assert!(text.is_char_boundary(block.start + block.marker));
            assert_eq!(block.level == 0, block.kind != Kind::Heading);
            at = block.end;
            if block.end < text.len() {
                assert_eq!(bytes[block.end], b'\n');
                at += 1;
            }
        }
        assert_eq!(at, text.len());
    }

    #[test]
    fn crlf_leaves_the_carriage_return_in_the_content() {
        let text = "a\r\nb";
        let blocks = scan_text(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, Kind::Paragraph);
        assert_eq!((blocks[0].start, blocks[0].end), (0, text.len()));
        assert_eq!(spans(text, &blocks), ["a\r\nb"]);

        // A CRLF "blank" line is not blank at this layer: the `\r` is content.
        let blank = "a\r\n\r\nb";
        let blocks = scan_text(blank);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, Kind::Paragraph);
    }

    #[test]
    fn crlf_fence_lines_do_not_close_because_the_carriage_return_is_content() {
        let text = "```\r\nx\r\n```\r\n";
        let blocks = scan_text(text);
        assert!(blocks.iter().all(|block| block.kind == Kind::Code));
        assert_eq!(blocks[0].end, 4);
    }

    /// A tiny deterministic generator, so the property test does not need a
    /// crate.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, limit: usize) -> usize {
            (self.next() % limit as u64) as usize
        }
    }

    #[test]
    fn a_generated_document_always_tiles_and_respects_its_kinds() {
        let palette = [
            "",
            "plain words",
            "# heading",
            "## subheading",
            "> quoted words",
            "- item",
            "1. ordered",
            "- [ ] task",
            "  continuation",
            "\t- tabbed",
            "```",
            "```rust",
            "~~~",
            "---",
            "***",
            "key: value",
            "  ---",
            "text with **bold** and [link](url)",
        ];
        let mut rng = Lcg(0x5eed_1234_abcd_9876);
        let mut text = String::new();
        for line in 0..240 {
            text.push_str(palette[rng.below(palette.len())]);
            if line + 1 < 240 || rng.below(2) == 0 {
                text.push('\n');
            }
        }

        let blocks = scan_text(&text);
        assert!(!blocks.is_empty());

        let bytes = text.as_bytes();
        let mut at = 0;
        let mut seen_non_frontmatter = false;
        for block in &blocks {
            assert_eq!(block.start, at);
            assert!(block.end >= block.start);
            assert!(block.end <= text.len());
            assert!(block.marker <= block.end - block.start);
            assert!(text.is_char_boundary(block.start + block.marker));

            match block.kind {
                Kind::Heading => {
                    assert!((1..=6).contains(&block.level));
                    assert!(block.marker > 0);
                }
                Kind::Quote | Kind::ListItem => {
                    assert_eq!(block.level, 0);
                    assert!(block.marker > 0);
                }
                Kind::Rule | Kind::Blank | Kind::Paragraph | Kind::Frontmatter => {
                    assert_eq!(block.level, 0);
                    assert_eq!(block.marker, 0);
                }
                Kind::Code => {}
            }

            if block.kind == Kind::Frontmatter {
                assert!(!seen_non_frontmatter, "frontmatter is a first-line prefix");
            } else {
                seen_non_frontmatter = true;
            }

            at = block.end;
            if block.end < text.len() {
                assert_eq!(bytes[block.end], b'\n');
                at += 1;
            }
        }
        assert_eq!(at, text.len());
    }

    #[test]
    fn first_changed_finds_a_deleted_fence_character() {
        let old = scan_text("```\ncode\n```");
        let new = scan_text("```\ncode\n``");
        assert_eq!(first_changed(&old, &new), 2);
        assert_eq!(old[2].marker, 3);
        assert_eq!(new[2].marker, 0);

        // Deleting the opening fence character changes the document at 0.
        let old = scan_text("```\ncode\n```");
        let new = scan_text("``\ncode\n```");
        assert_eq!(first_changed(&old, &new), 0);
    }

    #[test]
    fn first_changed_finds_a_deleted_blank_line() {
        // Deleting the blank line merges the two paragraphs at block 0.
        let old = scan_text("a\n\nb");
        let new = scan_text("a\nb");
        assert_eq!(first_changed(&old, &new), 0);

        // Deleting the blank between b and c starts at b's block.
        let old = scan_text("a\n\nb\n\nc");
        let new = scan_text("a\n\nb\nc");
        assert_eq!(first_changed(&old, &new), 2);
    }

    #[test]
    fn first_changed_finds_structural_edits() {
        let heading = scan_text("# Title");
        let plain = scan_text("Title");
        assert_eq!(first_changed(&heading, &plain), 0);

        let h1 = scan_text("# Title");
        let h2 = scan_text("## Title");
        assert_eq!(first_changed(&h1, &h2), 0);

        let later_heading = scan_text("# A\n\n## B");
        let later_plain = scan_text("# A\n\nB");
        assert_eq!(first_changed(&later_heading, &later_plain), 2);

        let quoted = scan_text("> a\n\n> b");
        let merged_quote = scan_text("> a\n> b");
        assert_eq!(first_changed(&quoted, &merged_quote), 0);

        let listed = scan_text("- a\n\n  b");
        let continued = scan_text("- a\n  b");
        assert_eq!(first_changed(&listed, &continued), 0);

        let split = scan_text("a\nb");
        let paragraphs = scan_text("a\n\nb");
        assert_eq!(first_changed(&split, &paragraphs), 0);

        let front = scan_text("---\ntitle: x\n---\nbody");
        let not_front = scan_text("---\ntitle: x\n--\nbody");
        assert_eq!(first_changed(&front, &not_front), 0);
    }

    #[test]
    fn first_changed_cannot_see_a_same_length_content_replacement() {
        // Every block field is identical; only the bytes moved. The caller's
        // edit block is what invalidates this case.
        let old = scan_text("alpha\n\nbeta");
        let new = scan_text("alphA\n\nbetA");
        assert_eq!(old, new);
        assert_eq!(first_changed(&old, &new), new.len());
    }

    #[test]
    fn first_changed_ignores_a_constant_offset_shift() {
        let old = scan_text("a\n\nb\n\nc");
        let new = scan_text("aa\n\nb\n\nc");
        assert_eq!(first_changed(&old, &new), 0);
        // The later blocks are byte-shifted but unchanged in shape.
        assert_eq!(
            first_changed(&old[1..], &new[1..]),
            old[1..].len(),
            "a constant shift is not a content change"
        );
    }

    #[test]
    fn first_changed_returns_the_old_length_for_an_appended_block() {
        let old = scan_text("a");
        let new = scan_text("a\n\nb");
        assert_eq!(first_changed(&old, &new), 1);
        assert_eq!(old.len(), 1);
        assert_eq!(new.len(), 3);
    }

    /// Applies one replacement the way the editor does — rescan from the block
    /// before the edit, walked back to the start of any fence/frontmatter run
    /// — and returns the rescan result plus how far scanning actually went.
    fn rescan_edit(
        old_text: &str,
        range: std::ops::Range<usize>,
        replacement: &str,
    ) -> (Vec<Block>, usize, String) {
        let old = scan_text(old_text);
        let edit_block = old
            .iter()
            .position(|block| range.start <= block.end)
            .unwrap_or_else(|| old.len().saturating_sub(1));
        let mut from_block = edit_block.saturating_sub(1);
        while from_block > 0
            && matches!(old[from_block].kind, Kind::Code | Kind::Frontmatter)
            && old[from_block - 1].kind == old[from_block].kind
        {
            from_block -= 1;
        }
        let mut new_text = old_text[..range.start].to_owned();
        new_text.push_str(replacement);
        new_text.push_str(&old_text[range.end..]);
        let mut out = Vec::new();
        let scanned = rescan_from(
            &new_text,
            &old,
            from_block,
            range.start + replacement.len(),
            &mut out,
        );
        (out, scanned, new_text)
    }

    fn assert_rescan_matches_scan(
        old_text: &str,
        range: std::ops::Range<usize>,
        replacement: &str,
    ) -> usize {
        let (out, scanned, new_text) = rescan_edit(old_text, range, replacement);
        assert_eq!(
            out,
            scan_text(&new_text),
            "rescan diverged from a full scan for {new_text:?}"
        );
        scanned
    }

    #[test]
    fn rescan_matches_a_full_scan_and_stops_early_in_long_prose() {
        let mut old = String::new();
        for index in 0..50 {
            old.push_str(&format!("paragraph {index} has some words in it\n\n"));
        }
        let scanned = assert_rescan_matches_scan(&old, 5..5, "x");
        assert!(
            scanned < old.len() / 4,
            "an early edit rescanned {scanned} of {} bytes",
            old.len()
        );
        // Same-length replacement in the middle resyncs too (zero shift).
        let mid = old.find("paragraph 25").expect("middle paragraph");
        let scanned = assert_rescan_matches_scan(&old, mid..mid + 9, "paragraph");
        assert!(scanned < old.len(), "a middle edit should resync");
        // An edit in the last paragraph has nothing to resync with.
        let last = old.rfind("paragraph 49").expect("last paragraph");
        let scanned = assert_rescan_matches_scan(&old, last..last, "x");
        assert_eq!(scanned, old.len() + 1);
    }

    /// Deterministic prose at an exact word count: `words` per paragraph.
    fn words_text(paragraphs: usize, words: usize) -> String {
        let mut text = String::new();
        for paragraph in 0..paragraphs {
            text.push_str(&format!("para{paragraph}"));
            for _ in 1..words {
                text.push_str(" word");
            }
            text.push_str("\n\n");
        }
        text
    }

    #[test]
    fn rescan_of_a_million_word_note_stops_after_the_edited_paragraph() {
        // Perf fixture: work is counted in scanned bytes, never wall time.
        let old = words_text(10_000, 100);
        assert!(old.len() > 4_000_000, "fixture should be ~1M words");
        let scanned = assert_rescan_matches_scan(&old, 5..5, "x");
        assert!(
            scanned < 8_192,
            "an edit in the first paragraph scanned {scanned} bytes of {}",
            old.len()
        );
        let mid = old.find("para5000").expect("middle paragraph");
        let scanned = assert_rescan_matches_scan(&old, mid..mid, "x");
        assert!(
            scanned < mid + 8_192,
            "a middle edit scanned to {scanned}, past its paragraph at {mid}"
        );
    }

    #[test]
    fn rescan_matches_a_full_scan_across_structural_edits() {
        // An unclosed fence swallows the rest: no blank is ever neutral, so
        // the rescan runs to the end and still matches.
        let old = "para one\n\npara two\n\npara three";
        let scanned = assert_rescan_matches_scan(old, 0..0, "```\n");
        assert_eq!(scanned, old.len() + 4);
        // An edit inside a fenced block, rescanned from its opener.
        let old = "```\ncode line\nmore code\n```\n\nprose here\n\nlast para";
        let at = old.find("code line").expect("code");
        assert_rescan_matches_scan(old, at..at + 4, "code!");
        // Deleting a blank line merges two paragraphs.
        let old = "one para\n\ntwo para\n\nthree para\n\nfour para";
        let at = old.find("\n\n").expect("blank");
        assert_rescan_matches_scan(old, at..at + 2, "\n");
        // Removing a heading marker merges the heading into the intro run.
        let old = "intro text\n\n# Head\n\nbody para\n\nlast para";
        let at = old.find("# ").expect("marker");
        assert_rescan_matches_scan(old, at..at + 2, "");
        // A replacement spanning several blocks.
        let old = "alpha para\n\nbeta para\n\ngamma para\n\ndelta para";
        let start = old.find("beta").expect("start");
        let end = old.find("gamma").expect("end") + 5;
        assert_rescan_matches_scan(old, start..end, "merged text");
        // Frontmatter value edit, rescanned from the top.
        let old = "---\ntitle: draft\n---\n\nbody text\n\nmore body";
        let at = old.find("draft").expect("title");
        assert_rescan_matches_scan(old, at..at + 5, "final");
        // Append, empty-document edit, and a deleted first paragraph in a
        // repetitive document where blanks look identical.
        let old = "only para";
        assert_rescan_matches_scan(old, old.len()..old.len(), "\n\nnew para");
        assert_rescan_matches_scan("", 0..0, "hello\n\nworld");
        let old = "same words here\n\n".repeat(20);
        assert_rescan_matches_scan(&old, 0.."same words here\n\n".len(), "");
    }
}
