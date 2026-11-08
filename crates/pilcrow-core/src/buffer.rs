//! The document's bytes, and the line index the editor needs over them.
//!
//! The buffer is the file: nothing is normalised on the way in, because a
//! writing app that silently rewrites your text is a writing app you cannot
//! trust with it. In particular, CRLF is content; only `\n` separates lines.
//!
//! All offsets are UTF-8 byte offsets. Externally supplied offsets are clamped
//! to the document and snapped to a character boundary before use, so a caret
//! can never land inside a codepoint.

use core::ops::Range;

/// The document text, a revision counter, and the byte offset of every line
/// start.
///
/// Layout caches key on [`Buffer::revision`], so it changes only when the text
/// actually changes. A no-op edit leaves both the text and the revision alone.
#[derive(Clone, Debug)]
pub struct Buffer {
    text: String,
    revision: u64,
    line_starts: Vec<usize>,
    /// Order-independent sum of per-line hashes; see [`Buffer::content_hash`].
    hash: u64,
}

impl Buffer {
    /// Creates a buffer holding `text` exactly as given.
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            revision: 0,
            line_starts: line_starts_for(text),
            hash: content_hash(text),
        }
    }

    /// A hash of the current bytes, maintained incrementally on every edit:
    /// only the lines an edit touches are rehashed, never the whole document.
    ///
    /// The combine is a wrapping sum of per-line hashes, so it is deliberately
    /// weaker than equality: equal hashes do not prove equal text (reordered
    /// lines hash the same). Its contract is the other direction — different
    /// hashes prove different text — which lets a dirty check answer "changed"
    /// in O(1) and reserve the byte comparison for confirming "unchanged".
    pub fn content_hash(&self) -> u64 {
        self.hash
    }

    /// The document bytes.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Bumped once per edit that changes the text.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The document length in bytes.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Whether the document is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Inserts `text` at `at`, clamped to the document and snapped to a
    /// character boundary.
    ///
    /// Returns the caret after the inserted text. An empty insert is a no-op
    /// and does not bump the revision.
    pub fn insert(&mut self, at: usize, text: &str) -> usize {
        let at = boundary_at_or_before(&self.text, at);
        if !text.is_empty() {
            let old = self.hash_of_span(at, at);
            self.text.insert_str(at, text);
            let new = self.hash_of_span(at, at + text.len());
            self.hash = self.hash.wrapping_sub(old).wrapping_add(new);
            self.update_line_starts(at, at, text);
            self.revision = self.revision.wrapping_add(1);
        }
        at + text.len()
    }

    /// Replaces `range` with `text`.
    ///
    /// Both ends are clamped and snapped to character boundaries; if the
    /// resulting start is greater than the end, they are swapped. Returns the
    /// caret after the inserted text. Replacing a range with identical bytes
    /// is a no-op and does not bump the revision.
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> usize {
        let mut start = boundary_at_or_before(&self.text, range.start);
        let mut end = boundary_at_or_before(&self.text, range.end);
        if start > end {
            core::mem::swap(&mut start, &mut end);
        }
        if self.text.get(start..end) != Some(text) {
            let old = self.hash_of_span(start, end);
            self.text.replace_range(start..end, text);
            let new = self.hash_of_span(start, start + text.len());
            self.hash = self.hash.wrapping_sub(old).wrapping_add(new);
            self.update_line_starts(start, end, text);
            self.revision = self.revision.wrapping_add(1);
        }
        start + text.len()
    }

    /// The summed line hashes of the complete lines overlapping
    /// `[start, end)` in the current text. Pure insertions (`start == end`)
    /// still cover the line being inserted into, because its bytes change.
    fn hash_of_span(&self, start: usize, end: usize) -> u64 {
        let (from, to) = line_span(self.text.as_bytes(), start, end);
        hash_lines(&self.text.as_bytes()[from..to])
    }

    /// The number of source lines. Empty text has one (empty) line, and a
    /// trailing newline creates a final empty line.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// The line containing byte offset `at`, clamped to the document.
    ///
    /// A newline byte belongs to the line it terminates, so the caret at a
    /// newline is on the line before the next one.
    pub fn line_of(&self, at: usize) -> usize {
        let at = at.min(self.text.len());
        self.line_starts
            .partition_point(|&start| start <= at)
            .saturating_sub(1)
    }

    /// The byte offset where `line` starts. Out-of-range lines clamp to the
    /// last line.
    pub fn line_start(&self, line: usize) -> usize {
        let line = line.min(self.line_count() - 1);
        self.line_starts[line]
    }

    /// The byte offset just past the last byte of `line`, excluding the `\n`.
    ///
    /// A `\r` before the `\n` is content and is included. Out-of-range lines
    /// clamp to the last line.
    pub fn line_end(&self, line: usize) -> usize {
        let line = line.min(self.line_count() - 1);
        match self.line_starts.get(line + 1) {
            Some(&next) => next - 1,
            None => self.text.len(),
        }
    }

    /// The byte range of `line`, excluding its terminating `\n`.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        self.line_start(line)..self.line_end(line)
    }

    /// Updates the line index after replacing `[start, end)` with `inserted`.
    ///
    /// Line starts at or before `start` survive; starts strictly inside the
    /// replaced range disappear; later starts shift with the text. New starts
    /// come from `\n` bytes in `inserted`, in order.
    fn update_line_starts(&mut self, start: usize, end: usize, inserted: &str) {
        let keep = self.line_starts.partition_point(|&line| line <= start);
        let resume = self.line_starts.partition_point(|&line| line <= end);
        let removed = end - start;

        let mut tail = self.line_starts.split_off(keep);
        tail.drain(..resume - keep);
        for line in &mut tail {
            *line = *line - removed + inserted.len();
        }

        self.line_starts.extend(
            inserted
                .match_indices('\n')
                .map(|(offset, _)| start + offset + 1),
        );
        self.line_starts.extend(tail);
    }
}

/// The content hash of `text`: a wrapping sum of mixed per-line hashes,
/// matching [`Buffer::content_hash`] for a buffer holding `text`. Lines keep
/// their terminating `\n`, so `"a"` and `"a\n"` differ; line order does not
/// contribute, by design (see the method's contract).
pub fn content_hash(text: &str) -> u64 {
    hash_lines(text.as_bytes())
}

/// Sums the mixed hash of each line in `bytes`, splitting after every `\n`.
/// The caller passes whole lines only; a trailing partial line hashes as-is.
fn hash_lines(bytes: &[u8]) -> u64 {
    let mut total = 0u64;
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            total = total.wrapping_add(mix(line_hash(&bytes[start..=index])));
            start = index + 1;
        }
    }
    if start < bytes.len() {
        total = total.wrapping_add(mix(line_hash(&bytes[start..])));
    }
    total
}

fn line_hash(line: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in line {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// SplitMix64 finaliser: spreads the structured FNV values of similar lines
/// so their sum is not itself easily predictable.
fn mix(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// The byte range of the complete lines overlapping the replaced range
/// `[start, end)` in `bytes`: from the start of the line containing `start`
/// to just past the `\n` of the line containing the position `end`. The end
/// is treated as a position, not a last byte, so the line a replacement ends
/// in front of is included on both sides of an edit: its hash is subtracted
/// and re-added even when only its prefix changed (an insertion ending with
/// `\n` pushes the rest of the edited line into a new one). Inserting at the
/// end of a text that ends with `\n` affects no existing line.
fn line_span(bytes: &[u8], start: usize, end: usize) -> (usize, usize) {
    let from = bytes[..start.min(bytes.len())]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    if end >= bytes.len() {
        return (from, bytes.len());
    }
    let to = bytes[end..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| end + offset + 1);
    (from, to)
}

/// The byte offset of every line start in `text`. The first line always starts
/// at zero.
fn line_starts_for(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    starts.push(0);
    starts.extend(text.match_indices('\n').map(|(offset, _)| offset + 1));
    starts
}

/// The greatest character boundary at or before `at`.
///
/// Offsets at or past the end return the document length. An offset inside a
/// codepoint snaps back to the start of that codepoint.
pub fn boundary_at_or_before(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The greatest character boundary strictly before `at`, or zero at the start.
///
/// Offsets past the end of the document resolve to the end.
pub fn prev_boundary(text: &str, at: usize) -> usize {
    if at == 0 {
        return 0;
    }
    boundary_at_or_before(text, at - 1)
}

/// The next character boundary after `at`, or the document length at the end.
///
/// An offset inside a codepoint advances to the end of that codepoint.
pub fn next_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    if at >= text.len() {
        return text.len();
    }
    at += 1;
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::{Buffer, boundary_at_or_before, next_boundary, prev_boundary};

    /// A tiny deterministic generator, so property tests do not need a crate.
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

    fn snap(text: &str, at: usize) -> usize {
        let mut at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    fn expected_starts(text: &str) -> Vec<usize> {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(offset, _)| offset + 1));
        starts
    }

    #[test]
    fn empty_document_has_one_empty_line() {
        let buffer = Buffer::new("");
        assert_eq!(buffer.text(), "");
        assert_eq!(buffer.revision(), 0);
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.line_count(), 1);

        for at in [0, 1, usize::MAX] {
            assert_eq!(buffer.line_of(at), 0);
            assert_eq!(buffer.line_start(at), 0);
            assert_eq!(buffer.line_end(at), 0);
            assert_eq!(buffer.line_range(at), 0..0);
        }
    }

    #[test]
    fn single_line_indexing() {
        let buffer = Buffer::new("hello");
        assert_eq!(buffer.line_count(), 1);
        for at in 0..=5 {
            assert_eq!(buffer.line_of(at), 0);
        }
        assert_eq!(buffer.line_start(0), 0);
        assert_eq!(buffer.line_end(0), 5);
        assert_eq!(buffer.line_range(0), 0..5);
    }

    #[test]
    fn trailing_newline_creates_a_final_empty_line() {
        let buffer = Buffer::new("a\nb\n");
        assert_eq!(buffer.line_count(), 3);
        assert_eq!(buffer.line_starts.as_slice(), [0, 2, 4]);

        assert_eq!(buffer.line_of(1), 0);
        assert_eq!(buffer.line_of(2), 1);
        assert_eq!(buffer.line_of(3), 1);
        assert_eq!(buffer.line_of(4), 2);

        assert_eq!(buffer.line_range(0), 0..1);
        assert_eq!(buffer.line_range(1), 2..3);
        assert_eq!(buffer.line_range(2), 4..4);
    }

    #[test]
    fn crlf_is_content_except_for_the_newline() {
        let mut buffer = Buffer::new("a\r\nb");
        assert_eq!(buffer.text(), "a\r\nb");
        assert_eq!(buffer.line_count(), 2);
        assert_eq!(buffer.line_starts.as_slice(), [0, 3]);

        assert_eq!(buffer.line_range(0), 0..2);
        assert_eq!(&buffer.text()[buffer.line_range(0)], "a\r");
        assert_eq!(buffer.line_of(2), 0);
        assert_eq!(buffer.line_of(3), 1);
        assert_eq!(buffer.line_end(0), 2);
        assert_eq!(buffer.line_end(1), 4);

        assert_eq!(buffer.insert(1, "\r"), 2);
        assert_eq!(buffer.text(), "a\r\r\nb");
        assert_eq!(buffer.line_starts.as_slice(), [0, 4]);
        assert_eq!(buffer.line_range(0), 0..3);
    }

    #[test]
    fn multiline_line_lookup_and_ranges() {
        let buffer = Buffer::new("alpha\nbeta\n\ngamma");
        assert_eq!(buffer.line_count(), 4);
        assert_eq!(buffer.line_starts.as_slice(), [0, 6, 11, 12]);

        assert_eq!(buffer.line_of(0), 0);
        assert_eq!(buffer.line_of(4), 0);
        assert_eq!(buffer.line_of(5), 0);
        assert_eq!(buffer.line_of(6), 1);
        assert_eq!(buffer.line_of(10), 1);
        assert_eq!(buffer.line_of(11), 2);
        assert_eq!(buffer.line_of(12), 3);
        assert_eq!(buffer.line_of(17), 3);

        assert_eq!(buffer.line_range(0), 0..5);
        assert_eq!(buffer.line_range(1), 6..10);
        assert_eq!(buffer.line_range(2), 11..11);
        assert_eq!(buffer.line_range(3), 12..17);

        assert_eq!(buffer.line_start(99), 12);
        assert_eq!(buffer.line_end(99), 17);
        assert_eq!(buffer.line_range(99), 12..17);
    }

    #[test]
    fn insert_at_start_middle_end_tracks_revision() {
        let mut buffer = Buffer::new("bc");
        assert_eq!(buffer.revision(), 0);

        assert_eq!(buffer.insert(0, "a"), 1);
        assert_eq!(buffer.text(), "abc");
        assert_eq!(buffer.revision(), 1);

        assert_eq!(buffer.insert(2, "X"), 3);
        assert_eq!(buffer.text(), "abXc");
        assert_eq!(buffer.revision(), 2);

        assert_eq!(buffer.insert(buffer.len(), "!"), 5);
        assert_eq!(buffer.text(), "abXc!");
        assert_eq!(buffer.revision(), 3);
    }

    #[test]
    fn replace_at_start_middle_end_tracks_revision() {
        let mut buffer = Buffer::new("hello world");

        assert_eq!(buffer.replace(0..5, "HI"), 2);
        assert_eq!(buffer.text(), "HI world");
        assert_eq!(buffer.revision(), 1);

        assert_eq!(buffer.replace(3..8, "there"), 8);
        assert_eq!(buffer.text(), "HI there");
        assert_eq!(buffer.revision(), 2);

        assert_eq!(buffer.replace(buffer.len()..buffer.len(), "!"), 9);
        assert_eq!(buffer.text(), "HI there!");
        assert_eq!(buffer.revision(), 3);

        let (start, end) = (5, 2);
        assert_eq!(buffer.replace(start..end, "-"), 3);
        assert_eq!(buffer.text(), "HI-ere!");
        assert_eq!(buffer.revision(), 4);
    }

    #[test]
    fn out_of_range_edits_clamp() {
        let mut buffer = Buffer::new("abc");
        assert_eq!(buffer.insert(99, "d"), 4);
        assert_eq!(buffer.text(), "abcd");

        let mut buffer = Buffer::new("abc");
        assert_eq!(buffer.replace(1..99, "X"), 2);
        assert_eq!(buffer.text(), "aX");

        let mut buffer = Buffer::new("abc");
        assert_eq!(buffer.replace(99..200, "!"), 4);
        assert_eq!(buffer.text(), "abc!");
    }

    #[test]
    fn inserts_clamp_and_snap_to_char_boundaries() {
        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.insert(2, "X"), 2);
        assert_eq!(buffer.text(), "aXé中😀b");

        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.insert(4, "Y"), 4);
        assert_eq!(buffer.text(), "aéY中😀b");

        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.insert(8, "Z"), 7);
        assert_eq!(buffer.text(), "aé中Z😀b");

        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.insert(usize::MAX, "!"), 12);
        assert_eq!(buffer.text(), "aé中😀b!");
    }

    #[test]
    fn replaces_clamp_and_snap_both_ends() {
        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.replace(2..8, "X"), 2);
        assert_eq!(buffer.text(), "aX😀b");

        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.replace(0..2, "X"), 1);
        assert_eq!(buffer.text(), "Xé中😀b");

        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.replace(11..usize::MAX, "!"), 12);
        assert_eq!(buffer.text(), "aé中😀b!");

        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.replace(0..usize::MAX, "x"), 1);
        assert_eq!(buffer.text(), "x");
    }

    #[test]
    fn no_op_edits_do_not_bump_revision() {
        let mut buffer = Buffer::new("aé中😀b");
        let original = buffer.text().to_owned();
        let revision = buffer.revision();

        assert_eq!(buffer.insert(4, ""), 3);
        assert_eq!(buffer.revision(), revision);
        assert_eq!(buffer.text(), original);

        assert_eq!(buffer.replace(2..8, "é中"), 6);
        assert_eq!(buffer.revision(), revision);
        assert_eq!(buffer.text(), original);

        assert_eq!(buffer.replace(2..2, ""), 1);
        assert_eq!(buffer.revision(), revision);

        assert_eq!(buffer.replace(0..0, ""), 0);
        assert_eq!(buffer.revision(), revision);
        assert_eq!(buffer.text(), original);
    }

    #[test]
    fn boundary_helpers_on_ascii_and_empty() {
        let empty = "";
        assert_eq!(boundary_at_or_before(empty, 0), 0);
        assert_eq!(boundary_at_or_before(empty, 5), 0);
        assert_eq!(prev_boundary(empty, 0), 0);
        assert_eq!(prev_boundary(empty, 5), 0);
        assert_eq!(next_boundary(empty, 0), 0);
        assert_eq!(next_boundary(empty, 5), 0);

        let ascii = "abc";
        assert_eq!(boundary_at_or_before(ascii, 0), 0);
        assert_eq!(boundary_at_or_before(ascii, 2), 2);
        assert_eq!(boundary_at_or_before(ascii, 9), 3);

        assert_eq!(prev_boundary(ascii, 0), 0);
        assert_eq!(prev_boundary(ascii, 1), 0);
        assert_eq!(prev_boundary(ascii, 3), 2);
        assert_eq!(prev_boundary(ascii, 9), 3);

        assert_eq!(next_boundary(ascii, 0), 1);
        assert_eq!(next_boundary(ascii, 2), 3);
        assert_eq!(next_boundary(ascii, 3), 3);
        assert_eq!(next_boundary(ascii, 9), 3);
    }

    #[test]
    fn boundary_helpers_around_multibyte_codepoints() {
        let text = "aé中😀b";
        let boundaries = [0, 1, 3, 6, 10, 11];

        for at in 0..=13 {
            let before = boundary_at_or_before(text, at);
            assert!(text.is_char_boundary(before));
            assert!(before <= at.min(text.len()));
            assert!(boundaries.contains(&before));
        }
        assert_eq!(boundary_at_or_before(text, usize::MAX), 11);
        for at in boundaries {
            assert_eq!(boundary_at_or_before(text, at), at);
        }

        assert_eq!(prev_boundary(text, 0), 0);
        assert_eq!(prev_boundary(text, 1), 0);
        assert_eq!(prev_boundary(text, 2), 1);
        assert_eq!(prev_boundary(text, 3), 1);
        assert_eq!(prev_boundary(text, 4), 3);
        assert_eq!(prev_boundary(text, 10), 6);
        assert_eq!(prev_boundary(text, 11), 10);
        assert_eq!(prev_boundary(text, 12), 11);

        assert_eq!(next_boundary(text, 0), 1);
        assert_eq!(next_boundary(text, 1), 3);
        assert_eq!(next_boundary(text, 2), 3);
        assert_eq!(next_boundary(text, 3), 6);
        assert_eq!(next_boundary(text, 5), 6);
        assert_eq!(next_boundary(text, 6), 10);
        assert_eq!(next_boundary(text, 9), 10);
        assert_eq!(next_boundary(text, 10), 11);
        assert_eq!(next_boundary(text, 11), 11);
        assert_eq!(next_boundary(text, 12), 11);
    }

    #[test]
    fn line_index_tracks_inserts_and_replacements() {
        let mut buffer = Buffer::new("one\ntwo\nthree");
        assert_eq!(buffer.line_starts.as_slice(), [0, 4, 8]);

        buffer.insert(3, "\n");
        assert_eq!(buffer.text(), "one\n\ntwo\nthree");
        assert_eq!(buffer.line_starts.as_slice(), [0, 4, 5, 9]);

        buffer.insert(4, "X");
        assert_eq!(buffer.text(), "one\nX\ntwo\nthree");
        assert_eq!(buffer.line_starts.as_slice(), [0, 4, 6, 10]);

        buffer.replace(4..6, "T\n");
        assert_eq!(buffer.text(), "one\nT\ntwo\nthree");
        assert_eq!(buffer.line_starts.as_slice(), [0, 4, 6, 10]);

        buffer.replace(4..7, "U");
        assert_eq!(buffer.text(), "one\nUwo\nthree");
        assert_eq!(buffer.line_starts.as_slice(), [0, 4, 8]);
    }

    #[test]
    fn out_of_range_offsets_never_panic() {
        let mut buffer = Buffer::new("aé中😀b");
        let len = buffer.len();
        let last_line = buffer.line_count() - 1;

        assert_eq!(buffer.line_of(usize::MAX), last_line);
        assert_eq!(buffer.line_start(usize::MAX), buffer.line_start(last_line));
        assert_eq!(buffer.line_end(usize::MAX), len);
        assert_eq!(buffer.line_range(usize::MAX), buffer.line_range(last_line));

        assert_eq!(buffer.insert(usize::MAX, "!"), len + 1);
        assert_eq!(buffer.text(), "aé中😀b!");
        assert_eq!(buffer.revision(), 1);

        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.replace(usize::MAX..usize::MAX, "!"), len + 1);
        assert_eq!(buffer.text(), "aé中😀b!");
        assert_eq!(buffer.revision(), 1);

        let mut buffer = Buffer::new("aé中😀b");
        assert_eq!(buffer.replace(0..usize::MAX, "x"), 1);
        assert_eq!(buffer.text(), "x");
        assert_eq!(buffer.revision(), 1);
    }

    #[test]
    fn generated_text_line_index_matches_a_scan() {
        let mut rng = Lcg(0x00c0_ffee_1234_5678);

        for _ in 0..200 {
            let mut text = String::new();
            for _ in 0..rng.below(80) {
                match rng.below(10) {
                    0 => text.push('\n'),
                    1 => text.push('\r'),
                    2 => text.push('é'),
                    3 => text.push('中'),
                    4 => text.push('😀'),
                    _ => text.push(char::from(b'a' + rng.below(26) as u8)),
                }
            }

            let buffer = Buffer::new(&text);
            let starts = expected_starts(&text);
            assert_eq!(buffer.line_starts, starts);
            assert_eq!(buffer.line_count(), starts.len());

            for line in 0..starts.len() {
                let start = buffer.line_start(line);
                let end = buffer.line_end(line);
                let expected_end = starts.get(line + 1).map_or(text.len(), |&next| next - 1);
                assert_eq!((start, end), (starts[line], expected_end));
                assert!(text.is_char_boundary(start));
                assert!(text.is_char_boundary(end));
                assert!(start <= end);
            }

            for at in 0..=text.len() {
                let line = buffer.line_of(at);
                assert!(buffer.line_start(line) <= at);
                assert!(at <= buffer.line_end(line));
            }
        }
    }

    #[test]
    fn random_edits_keep_text_and_line_index_consistent() {
        const SNIPPETS: &[&str] = &[
            "",
            "a",
            "Z",
            " ",
            "\n",
            "\r\n",
            "é",
            "中",
            "😀",
            "hello",
            "line\n",
            "\nline",
            "two\nlines",
            "e\u{301}",
        ];
        const STEPS: usize = 2_000;

        let mut rng = Lcg(0x05ee_d123_4abc_d001);
        let mut buffer = Buffer::new("");
        let mut reference = String::new();
        let mut revision = 0u64;

        for step in 0..STEPS {
            let at = rng.below(reference.len() + 8);
            let snippet = SNIPPETS[rng.below(SNIPPETS.len())];

            if rng.below(3) == 0 {
                let expected_at = snap(&reference, at);
                let changed = !snippet.is_empty();

                let caret = buffer.insert(at, snippet);
                reference.insert_str(expected_at, snippet);
                if changed {
                    revision += 1;
                }

                assert_eq!(caret, expected_at + snippet.len());
            } else {
                let raw_end = rng.below(reference.len() + 8);
                let mut start = snap(&reference, at);
                let mut end = snap(&reference, raw_end);
                if start > end {
                    core::mem::swap(&mut start, &mut end);
                }
                let changed = reference.get(start..end) != Some(snippet);

                let caret = buffer.replace(at..raw_end, snippet);
                reference.replace_range(start..end, snippet);
                if changed {
                    revision += 1;
                }

                assert_eq!(caret, start + snippet.len());
            }

            assert_eq!(buffer.text(), reference.as_str());
            assert_eq!(buffer.len(), reference.len());
            assert_eq!(buffer.revision(), revision);
            assert_eq!(
                buffer.content_hash(),
                super::content_hash(&reference),
                "incremental hash drifted from a fresh hash at step {step}"
            );

            let starts = expected_starts(&reference);
            assert_eq!(buffer.line_starts, starts);
            assert_eq!(buffer.line_count(), starts.len());

            if step % 16 == 0 || step + 1 == STEPS {
                check_offsets(&buffer, &reference, &starts);
            }
        }
    }

    #[test]
    fn content_hash_tracks_edits_and_returns_with_the_bytes() {
        let mut buffer = Buffer::new("alpha\nbeta\ngamma");
        let original = buffer.content_hash();
        assert_eq!(original, super::content_hash("alpha\nbeta\ngamma"));
        assert_ne!(original, super::content_hash("alpha\nbeta\ngamma\n"));

        buffer.insert(5, "!");
        assert_ne!(buffer.content_hash(), original);
        assert_eq!(buffer.content_hash(), super::content_hash(buffer.text()));
        buffer.replace(5..6, "");
        assert_eq!(
            buffer.content_hash(),
            original,
            "undoing the bytes must restore the hash"
        );

        // An insertion ending with a newline pushes the rest of the edited
        // line into a new one; both pieces must be rehashed.
        let mut split = Buffer::new("abc");
        split.insert(1, "x\n");
        assert_eq!(split.text(), "ax\nbc");
        assert_eq!(split.content_hash(), super::content_hash("ax\nbc"));

        // Reordered lines keep the hash by design: equality of hashes is a
        // prompt to compare bytes, never a proof of equality.
        assert_eq!(
            super::content_hash("alpha\nbeta\n"),
            super::content_hash("beta\nalpha\n")
        );
        // But any byte change within a line moves it.
        assert_ne!(
            super::content_hash("alpha\nbeta\n"),
            super::content_hash("alpha\nbetx\n")
        );
    }

    #[test]
    fn million_word_buffer_edits_stay_consistent() {
        // Perf fixture: correctness at scale for the line index and the
        // incremental hash; work bounds are asserted structurally in the
        // scanner/layout fixtures, not by wall time here.
        let mut text = String::new();
        for paragraph in 0..10_000 {
            text.push_str(&format!("para{paragraph}"));
            for _ in 1..100 {
                text.push_str(" word");
            }
            text.push_str("\n\n");
        }
        let mut buffer = Buffer::new(&text);
        assert_eq!(buffer.line_count(), 20_001);
        assert_eq!(buffer.content_hash(), super::content_hash(&text));

        let end = buffer.len();
        buffer.insert(end, "tail");
        assert!(buffer.text().ends_with("tail"));
        assert_eq!(buffer.content_hash(), super::content_hash(buffer.text()));

        buffer.replace(0..4, "head");
        assert!(buffer.text().starts_with("head0 word"));
        assert_eq!(buffer.content_hash(), super::content_hash(buffer.text()));
        assert_eq!(buffer.line_start(20_000), buffer.len() - "tail".len());
    }

    fn check_offsets(buffer: &Buffer, text: &str, starts: &[usize]) {
        for at in 0..=text.len() + 2 {
            let clamped = at.min(text.len());
            let line = buffer.line_of(at);
            assert!(line < starts.len());
            assert!(buffer.line_start(line) <= clamped);
            assert!(clamped <= buffer.line_end(line));
            assert!(text.is_char_boundary(buffer.line_start(line)));
            assert!(text.is_char_boundary(buffer.line_end(line)));
            assert_eq!(
                buffer.line_range(line),
                buffer.line_start(line)..buffer.line_end(line)
            );

            let before = boundary_at_or_before(text, at);
            assert!(text.is_char_boundary(before));
            assert!(before <= clamped);

            let prev = prev_boundary(text, at);
            assert!(text.is_char_boundary(prev));
            assert!(prev <= clamped);

            let next = next_boundary(text, at);
            assert!(text.is_char_boundary(next));
            assert!(next <= text.len());
            if at < text.len() {
                assert!(next > at);
            } else {
                assert_eq!(next, text.len());
            }
        }
    }
}
