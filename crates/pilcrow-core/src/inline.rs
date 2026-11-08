//! Which characters are bold, italic, struck through, or code — one answer per
//! byte.
//!
//! `**bold**` has to be *bold*, and a bold word is wider than the same word in
//! regular weight, so the wrapper cannot pretend the paragraph has one style.
//! Rather than a list of spans to search, every byte gets a kind: measuring a
//! range is then a walk over the range, and painting is a walk over runs of
//! equal kind. A paragraph is short, and the array is one byte per character.
//!
//! The delimiters themselves keep their own kind: they are drawn (dimmed) rather
//! than hidden, because this is a writing surface and not a preview. A link's
//! brackets and address are syntax too; only its words are the reader's.
//!
//! Spans nest: the words inside `**bold with `code`**` keep their own inner
//! style, and every delimiter of a span the scan claims — outer and inner — is
//! a marker, so reading copy and export drop it. What no span claims stays
//! text: malformed input (`**unclosed`, the spare `*` of `*a**`) keeps its
//! literal delimiters as characters. One kind per byte means nesting is
//! flattened to the innermost style on the words.
//!
//! A malformed paragraph must still scan in bounded time. The naive shape of
//! this scanner — every unmatched `*`, `_`, `~~`, backtick or `[` walking the
//! rest of the paragraph looking for a closer — is quadratic on a page of
//! asterisks or an unclosed bracket, and typing one character then costs the
//! paragraph's whole length per keystroke. The scan therefore builds a small
//! delimiter index once per call: unescaped closer positions, paren matches,
//! and runs of stars or backticks grouped by length. A closer query is a
//! binary search, so a paragraph full of unmatched openers is O(n log n) at
//! worst rather than O(n²), with the same answers as before.

use std::collections::BTreeMap;

/// The style of one byte of inline markdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Plain,
    Bold,
    Italic,
    Strike,
    Code,
    /// Syntax: `*`, `**`, `_`, `~~`, a backtick, or a link's brackets and URL.
    /// Drawn faint.
    Marker,
}

/// The ranges of a `[text](url)` link. `next` is the byte just past the closing
/// `)`, so a caller can skip the whole shape in one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub text_start: usize,
    pub text_end: usize,
    pub url_start: usize,
    pub url_end: usize,
    pub next: usize,
}

/// How deep spans may nest before the scan flattens the rest. Notes do not
/// stack emphasis more than a few levels; a cap keeps a page of asterisks from
/// recursing without bound on the paint path.
const MAX_DEPTH: usize = 16;

/// Fills `out` with one kind per byte of `text`.
///
/// `out.len()` must equal `text.len()`; the indices the scanner produces are
/// byte offsets into `text`, exactly the offsets the caret and selection use.
pub fn kinds(text: &str, out: &mut [Kind]) {
    assert_eq!(
        out.len(),
        text.len(),
        "inline kinds needs one slot per byte"
    );
    out.fill(Kind::Plain);
    // Nothing to scan: a page of plain prose must not pay for a delimiter
    // index or a recursive walk.
    if !text
        .as_bytes()
        .iter()
        .any(|&byte| matches!(byte, b'[' | b'*' | b'_' | b'~' | b'`'))
    {
        return;
    }
    let delims = Delims::new(text.as_bytes());
    scan(text.as_bytes(), out, &delims, 0, text.len(), 0);
}

/// Scans `[from, to)` for spans, painting into `out` (which shares `text`'s
/// indices). Nested spans are scanned inside out: the outer kind is painted
/// over the whole range first and the inner scan overwrites the parts it
/// claims, so unclaimed words keep the outer style and nested delimiters
/// become markers.
fn scan(text: &[u8], out: &mut [Kind], delims: &Delims, from: usize, to: usize, depth: usize) {
    let mut i = from;
    while i < to {
        work::bump(1);
        if text[i] == b'\\' {
            // An escaped delimiter is a character, not syntax.
            i += 2;
            continue;
        }

        // A link is one shape: its words stay for reading and everything
        // around them is syntax. Its words are scanned again, so emphasis
        // inside link text is styled without leaking its own markers. A link
        // that finishes outside the range being scanned is not this range's
        // shape; treating it as text keeps spans from overlapping.
        if text[i] == b'['
            && let Some(link) = delims.link_at(text, i)
            && link.next <= to
        {
            paint(out, i, i + 1, Kind::Marker);
            paint(out, link.text_end, link.next, Kind::Marker);
            if depth < MAX_DEPTH {
                scan(text, out, delims, link.text_start, link.text_end, depth + 1);
            }
            i = link.next;
            continue;
        }

        if text[i] == b'*' {
            let n = run_at(text, i, to, b'*');
            if let Some(span) = delims.star_span(text, i, n, to) {
                let delimiter = if n.min(span.close_len) >= 2 { 2 } else { 1 };
                let inner_from = i + delimiter;
                let inner_to = span.close_at + span.close_len - delimiter;
                if inner_to > inner_from {
                    let kind = if delimiter == 2 {
                        Kind::Bold
                    } else {
                        Kind::Italic
                    };
                    paint(out, i, inner_from, Kind::Marker);
                    paint(out, inner_to, span.close_at + span.close_len, Kind::Marker);
                    paint(out, inner_from, inner_to, kind);
                    if depth < MAX_DEPTH {
                        scan(text, out, delims, inner_from, inner_to, depth + 1);
                    }
                    // A run longer than the pair it matched leaves delimiter
                    // bytes over at either end. No nested span took them, so
                    // they are the run's spare bytes: text, never styled
                    // content — `*a**` does not italicise its second `*`.
                    spare_to_text(out, inner_from, (i + n).min(inner_to), kind);
                    spare_to_text(out, span.close_at, inner_to, kind);
                    i = span.close_at + span.close_len;
                    continue;
                }
            }
            i += n;
            continue;
        }

        // `_italic_` is what the menu writes. Unlike `*`, an underscore inside
        // a word is part of the word: `foo_bar_baz` must stay a name.
        if text[i] == b'_' && underscore_open(text, i, to) {
            if let Some(close) = delims.underscore_close(text, i + 1, to) {
                paint(out, i, i + 1, Kind::Marker);
                paint(out, i + 1, close, Kind::Italic);
                paint(out, close, close + 1, Kind::Marker);
                if depth < MAX_DEPTH {
                    scan(text, out, delims, i + 1, close, depth + 1);
                }
                i = close + 1;
                continue;
            }
            i += 1;
            continue;
        }

        if text[i] == b'~' && i + 2 < to && text[i + 1] == b'~' && !is_space(text[i + 2]) {
            if let Some(close) = delims.tilde_close(text, i + 2, to) {
                paint(out, i, i + 2, Kind::Marker);
                paint(out, i + 2, close, Kind::Strike);
                paint(out, close, close + 2, Kind::Marker);
                if depth < MAX_DEPTH {
                    scan(text, out, delims, i + 2, close, depth + 1);
                }
                i = close + 2;
                continue;
            }
            i += 2;
            continue;
        }

        if text[i] == b'`' {
            let n = run_at(text, i, to, b'`');
            // Backslashes are literal inside code, so the closer search does
            // not honour them; runs pair by length, the way ``` and ` differ.
            if let Some(close) = delims.code_close(i + n, to, n) {
                paint(out, i, i + n, Kind::Marker);
                paint(out, i + n, close, Kind::Code);
                paint(out, close, close + n, Kind::Marker);
                i = close + n;
                continue;
            }
            i += n;
            continue;
        }

        i += 1;
    }
}

/// The length of the run of `ch` starting at `at`, not passing `to`.
fn run_at(text: &[u8], at: usize, to: usize, ch: u8) -> usize {
    let mut n = 0;
    while at + n < to && text[at + n] == ch {
        n += 1;
    }
    n
}

#[derive(Clone, Copy)]
struct StarClose {
    close_at: usize,
    close_len: usize,
}

/// Delimiter bytes a longer run left inside its span, still wearing the
/// span's own kind once the nested scan is done: no inner pair claimed them,
/// so they are the run's spare bytes — plain text a reader keeps, never
/// styled content.
fn spare_to_text(out: &mut [Kind], from: usize, to: usize, kind: Kind) {
    let end = to.min(out.len());
    let mut i = from;
    while i < end {
        if out[i] == kind {
            out[i] = Kind::Plain;
        }
        i += 1;
    }
}

/// One maximal run of a delimiter byte.
#[derive(Clone, Copy, Debug)]
struct Run {
    start: usize,
    len: usize,
}

/// Runs longer than this are rare enough to live in a `BTreeMap`; the common
/// 1–4 byte runs get a direct slot and no tree allocation.
const SMALL_RUN: usize = 4;

/// A precomputed index of the delimiters in one paragraph.
///
/// The scanner asks the same questions repeatedly while walking a malformed
/// paragraph ("is there a closer after this opener?"). Answering each by
/// walking forward is quadratic; answering them from this index keeps the
/// scan within a logarithmic factor of the paragraph's length.
struct Delims {
    /// Unescaped `]` positions, ascending.
    brackets: Vec<usize>,
    /// Every `\n` or `\r`, ascending: a link never crosses one.
    newlines: Vec<usize>,
    /// `(` to its matching `)`, sorted by the opener. Parentheses do not
    /// balance across a newline.
    parens: Vec<(usize, usize)>,
    /// Every unescaped run of `*`, ascending.
    star_runs: Vec<Run>,
    /// Star-run starts whose run is not preceded by whitespace, ascending:
    /// only those can close emphasis.
    star_candidates: Vec<Run>,
    /// Candidate star-run starts grouped by run length.
    stars_by_len: [Vec<usize>; SMALL_RUN],
    stars_long: BTreeMap<usize, Vec<usize>>,
    /// Unescaped `_` with a non-space predecessor: a letter after the
    /// underscore would make it part of a name, not a closer.
    underscore_loose: Vec<usize>,
    /// The subset of [`Self::underscore_loose`] that is also not followed by
    /// a word character.
    underscore_strict: Vec<usize>,
    /// Unescaped `~~` pairs with a non-space predecessor.
    tildes: Vec<usize>,
    /// Every maximal run of backticks, ascending. Backslashes are literal
    /// inside code, so a backtick after one still starts the raw run that can
    /// close a span; the ordinary scan skips the pair but this index does not.
    ticks: Vec<Run>,
    /// Backtick-run starts grouped by run length.
    ticks_by_len: [Vec<usize>; SMALL_RUN],
    ticks_long: BTreeMap<usize, Vec<usize>>,
}

impl Delims {
    fn new(text: &[u8]) -> Self {
        let mut newlines = Vec::new();
        let mut brackets = Vec::new();
        let mut star_runs = Vec::new();
        let mut underscore_loose = Vec::new();
        let mut underscore_strict = Vec::new();
        let mut tildes = Vec::new();
        let mut parens = Vec::new();
        let mut stack = Vec::new();
        let mut ticks = Vec::new();
        let mut i = 0;
        while i < text.len() {
            work::bump(1);
            match text[i] {
                b'\\' => {
                    let next = text.get(i + 1).copied();
                    match next {
                        // A backslash still does not let a link cross a
                        // newline.
                        Some(b'\n' | b'\r') => {
                            newlines.push(i + 1);
                            stack.clear();
                            i += 2;
                        }
                        // Code spans do not honour backslashes: a backtick
                        // after one still begins the raw run that closes a
                        // span, so collect it here rather than in a second
                        // pass.
                        Some(b'`') => {
                            let start = i + 1;
                            let mut end = start + 1;
                            while end < text.len() && text[end] == b'`' {
                                work::bump(1);
                                end += 1;
                            }
                            ticks.push(Run {
                                start,
                                len: end - start,
                            });
                            i = end;
                        }
                        _ => i += 2,
                    }
                }
                b'\n' | b'\r' => {
                    newlines.push(i);
                    stack.clear();
                    i += 1;
                }
                b']' => {
                    brackets.push(i);
                    i += 1;
                }
                b'*' => {
                    let start = i;
                    i += 1;
                    while i < text.len() && text[i] == b'*' {
                        work::bump(1);
                        i += 1;
                    }
                    star_runs.push(Run {
                        start,
                        len: i - start,
                    });
                }
                b'_' => {
                    if i == 0 || !is_space(text[i - 1]) {
                        underscore_loose.push(i);
                        if !(i + 1 < text.len() && is_word_char_after(text, i, text.len())) {
                            underscore_strict.push(i);
                        }
                    }
                    i += 1;
                }
                b'~' => {
                    if i + 1 < text.len()
                        && text[i + 1] == b'~'
                        && (i == 0 || !is_space(text[i - 1]))
                    {
                        tildes.push(i);
                    }
                    i += 1;
                }
                b'`' => {
                    let start = i;
                    i += 1;
                    while i < text.len() && text[i] == b'`' {
                        work::bump(1);
                        i += 1;
                    }
                    ticks.push(Run {
                        start,
                        len: i - start,
                    });
                }
                b'(' => {
                    stack.push(i);
                    i += 1;
                }
                b')' => {
                    if let Some(open) = stack.pop() {
                        parens.push((open, i));
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
        parens.sort_unstable_by_key(|&(open, _)| open);

        let mut star_candidates = Vec::new();
        let mut stars_by_len: [Vec<usize>; SMALL_RUN] = std::array::from_fn(|_| Vec::new());
        let mut stars_long: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for run in &star_runs {
            if run.start == 0 || !is_space(text[run.start - 1]) {
                star_candidates.push(*run);
                push_run_start(&mut stars_by_len, &mut stars_long, run.len, run.start);
            }
        }

        let mut ticks_by_len: [Vec<usize>; SMALL_RUN] = std::array::from_fn(|_| Vec::new());
        let mut ticks_long: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for run in &ticks {
            push_run_start(&mut ticks_by_len, &mut ticks_long, run.len, run.start);
        }

        Self {
            brackets,
            newlines,
            parens,
            star_runs,
            star_candidates,
            stars_by_len,
            stars_long,
            underscore_loose,
            underscore_strict,
            tildes,
            ticks,
            ticks_by_len,
            ticks_long,
        }
    }

    /// The `]` that closes a link's text, unless a newline comes first.
    fn link_end(&self, from: usize) -> Option<usize> {
        let bracket = self
            .brackets
            .get(lower_bound(&self.brackets, from))
            .copied();
        let newline = self
            .newlines
            .get(lower_bound(&self.newlines, from))
            .copied();
        if let Some(nl) = newline
            && bracket.is_none_or(|b| nl < b)
        {
            return None;
        }
        bracket
    }

    /// The `)` that balances the `(` at `open`.
    fn url_end(&self, open: usize) -> Option<usize> {
        let mut lo = 0;
        let mut hi = self.parens.len();
        while lo < hi {
            work::bump(1);
            let mid = (lo + hi) / 2;
            let (candidate, close) = self.parens[mid];
            if candidate < open {
                lo = mid + 1;
            } else if candidate > open {
                hi = mid;
            } else {
                return Some(close);
            }
        }
        None
    }

    /// Whether an opener run of `n` asterisks at `i` closes inside `[i, to)`:
    /// the run must be followed by a non-space byte, and the closing run must
    /// be preceded by a non-space one. A closer run of the same length is
    /// preferred over an earlier run of a different length — that is what lets
    /// emphasis hold bold, and `***both***` come out with no asterisk left
    /// over. The closer's delimiters are taken from the right end of its run.
    fn star_span(&self, text: &[u8], i: usize, n: usize, to: usize) -> Option<StarClose> {
        if n == 0 || i + n >= to || is_space(text[i + n]) {
            return None;
        }
        let from = i + n;
        // A run cut in half by the scan's end still closes with the bytes
        // that are inside the range: its length is `to - start`, not its
        // length in the whole paragraph.
        let boundary = self.star_boundary(text, n, from, to);
        let same = earliest(self.star_of_len(n, from, to), boundary.map(|run| run.start));
        if let Some(close_at) = same {
            return Some(StarClose {
                close_at,
                close_len: n,
            });
        }
        let first = earliest_run(self.first_star(from, to), boundary)?;
        Some(StarClose {
            close_at: first.start,
            close_len: first.len,
        })
    }

    fn star_of_len(&self, n: usize, from: usize, to: usize) -> Option<usize> {
        let starts = run_starts(&self.stars_by_len, &self.stars_long, n)?;
        let start = *starts.get(lower_bound(starts, from))?;
        (start + n <= to).then_some(start)
    }

    fn first_star(&self, from: usize, to: usize) -> Option<Run> {
        let index = lower_run(&self.star_candidates, from);
        let run = *self.star_candidates.get(index)?;
        if run.start >= to {
            return None;
        }
        Some(Run {
            start: run.start,
            len: run.len.min(to - run.start),
        })
    }

    fn star_boundary(&self, text: &[u8], n: usize, from: usize, to: usize) -> Option<Run> {
        let start = to.checked_sub(n)?;
        if start < from {
            return None;
        }
        // A run at the opener's own edge is always eligible; elsewhere a
        // preceding space makes it a list bullet, not a closer.
        if start > from && is_space(text[start - 1]) {
            return None;
        }
        let run = run_starting_at(&self.star_runs, start)?;
        (run.len > n).then_some(Run { start, len: n })
    }

    /// The first `_` after `from` that can close emphasis. A run immediately
    /// after the opener is not an empty span, and an underscore inside a word
    /// is a name (`_a_b_` closes at the last underscore).
    fn underscore_close(&self, text: &[u8], from: usize, to: usize) -> Option<usize> {
        // An opener immediately followed by another underscore is not an
        // empty span, even when that second underscore would fail the word
        // tests below: `____` says nothing.
        if from < text.len() && text[from] == b'_' {
            return None;
        }
        let index = lower_bound(&self.underscore_strict, from);
        if let Some(&close) = self.underscore_strict.get(index) {
            if close == from {
                return None;
            }
            if close < to {
                return Some(close);
            }
        }
        // At the end of the range there is no next character to make the
        // underscore part of a word, so a looser candidate can close here
        // even though the whole-paragraph index excluded it.
        let boundary = to.checked_sub(1)?;
        if boundary > from && text[boundary] == b'_' && contains(&self.underscore_loose, boundary) {
            return Some(boundary);
        }
        None
    }

    /// The `~~` that closes a strike span. An opener immediately followed by
    /// its closer is not a span: `~~~~` says nothing.
    fn tilde_close(&self, text: &[u8], from: usize, to: usize) -> Option<usize> {
        if from + 1 >= to {
            return None;
        }
        if text[from..to].starts_with(b"~~") {
            return None;
        }
        let index = lower_bound(&self.tildes, from);
        let start = *self.tildes.get(index)?;
        (start + 1 < to).then_some(start)
    }

    /// The backtick run of exactly `n` that closes a code span opened by one
    /// of `n`. A run of a different length is content — `` `a``b` `` — and a
    /// backslash before a closer does not protect it, because backslashes are
    /// literal inside code.
    fn code_close(&self, from: usize, to: usize, n: usize) -> Option<usize> {
        if n == 0 {
            return None;
        }
        if let Some(starts) = run_starts(&self.ticks_by_len, &self.ticks_long, n) {
            let index = lower_bound(starts, from + 1);
            if let Some(&start) = starts.get(index)
                && start + n <= to
            {
                return Some(start);
            }
        }
        // A longer run straddling the end of the range is measured only to
        // the range, the way `run_at` sees it.
        let start = to.checked_sub(n)?;
        if start > from
            && let Some(run) = run_starting_at(&self.ticks, start)
            && run.len > n
        {
            return Some(start);
        }
        None
    }

    /// A `[text](url)` link opening at `i`, or `None`. A link is one inline
    /// shape: no newline, no nested brackets, and an escaped `[` is text.
    /// Empty words are not a link; there is nothing for a reader to see.
    /// Parentheses in the address may balance, so a Wikipedia address keeps
    /// its own brackets.
    fn link_at(&self, text: &[u8], i: usize) -> Option<Link> {
        if i >= text.len() || text[i] != b'[' || escaped_at(text, i) {
            return None;
        }

        let text_end = self.link_end(i + 1)?;
        if text_end == i + 1 {
            return None;
        }
        let open = text_end + 1;
        if open >= text.len() || text[open] != b'(' {
            return None;
        }

        let url_end = self.url_end(open)?;
        Some(Link {
            text_start: i + 1,
            text_end,
            url_start: text_end + 2,
            url_end,
            next: url_end + 1,
        })
    }
}

fn push_run_start(
    small: &mut [Vec<usize>; SMALL_RUN],
    long: &mut BTreeMap<usize, Vec<usize>>,
    len: usize,
    start: usize,
) {
    if (1..=SMALL_RUN).contains(&len) {
        small[len - 1].push(start);
    } else {
        long.entry(len).or_default().push(start);
    }
}

fn run_starts<'a>(
    small: &'a [Vec<usize>; SMALL_RUN],
    long: &'a BTreeMap<usize, Vec<usize>>,
    len: usize,
) -> Option<&'a [usize]> {
    if (1..=SMALL_RUN).contains(&len) {
        Some(small[len - 1].as_slice())
    } else {
        long.get(&len).map(|starts| starts.as_slice())
    }
}

/// The first position in a sorted list at or after `at`.
fn lower_bound(values: &[usize], at: usize) -> usize {
    let mut lo = 0;
    let mut hi = values.len();
    while lo < hi {
        work::bump(1);
        let mid = (lo + hi) / 2;
        if values[mid] < at {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn lower_run(runs: &[Run], start: usize) -> usize {
    let mut lo = 0;
    let mut hi = runs.len();
    while lo < hi {
        work::bump(1);
        let mid = (lo + hi) / 2;
        if runs[mid].start < start {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

fn run_starting_at(runs: &[Run], start: usize) -> Option<Run> {
    let mut lo = 0;
    let mut hi = runs.len();
    while lo < hi {
        work::bump(1);
        let mid = (lo + hi) / 2;
        match runs[mid].start.cmp(&start) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Some(runs[mid]),
        }
    }
    None
}

fn contains(values: &[usize], at: usize) -> bool {
    values
        .get(lower_bound(values, at))
        .is_some_and(|&p| p == at)
}

fn earliest(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn earliest_run(a: Option<Run>, b: Option<Run>) -> Option<Run> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a.start <= b.start { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// Every link in `text`, with one delimiter index shared across the scan.
/// This is the batch form of [`link_at`]; a hover/click path can call
/// `link_at` for one bracket, but a layout pass should call this once per
/// block instead of paying for a new index at every `[`.
pub fn links(text: &str) -> Vec<Link> {
    let bytes = text.as_bytes();
    let delims = Delims::new(bytes);
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'['
            && !escaped_at(bytes, i)
            && let Some(link) = delims.link_at(bytes, i)
        {
            out.push(link);
            i = link.next;
            continue;
        }
        i += 1;
    }
    out
}

/// A `[text](url)` link opening at `i`, or `None`.
pub fn link_at(text: &str, at: usize) -> Option<Link> {
    if at >= text.len() || text.as_bytes()[at] != b'[' {
        return None;
    }
    let delims = Delims::new(text.as_bytes());
    delims.link_at(text.as_bytes(), at)
}

/// Whether the byte at `i` is preceded by an odd number of backslashes.
fn escaped_at(text: &[u8], i: usize) -> bool {
    let mut n = 0;
    let mut k = i;
    while k > 0 && text[k - 1] == b'\\' {
        k -= 1;
        n += 1;
    }
    n % 2 == 1
}

/// `_` opens emphasis only at a word edge. `foo_bar_baz` is a name; `a _word_
/// b` and `(_word_)` are emphasis.
fn underscore_open(text: &[u8], i: usize, to: usize) -> bool {
    if i > 0 && is_word_char_before(text, i) {
        return false;
    }
    i + 1 < to && !is_space(text[i + 1])
}

/// Word characters for an underscore boundary: Unicode alphanumerics and
/// underscores. Decoding the neighbouring scalar means curly quotes, dashes
/// and emoji are punctuation, while `café_bar` stays one name.
fn is_word_char_before(text: &[u8], at: usize) -> bool {
    char_before(text, at).is_some_and(|c| c == '_' || c.is_alphanumeric())
}

fn is_word_char_after(text: &[u8], at: usize, to: usize) -> bool {
    if at + 1 >= to {
        return false;
    }
    char_after(&text[at + 1..to]).is_some_and(|c| c == '_' || c.is_alphanumeric())
}

/// The scalar ending at `at`, decoding at most the four bytes a UTF-8
/// codepoint can occupy. Decoding the whole prefix made a mispelled page of
/// underscores quadratic.
fn char_before(text: &[u8], at: usize) -> Option<char> {
    let mut start = at.saturating_sub(4);
    while start < at && (text[start] & 0xC0) == 0x80 {
        start += 1;
    }
    std::str::from_utf8(&text[start..at])
        .ok()
        .and_then(|text| text.chars().next_back())
}

/// The scalar at the start of `text`, decoding only its own bytes.
fn char_after(text: &[u8]) -> Option<char> {
    let end = match *text.first()? {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => return None,
    };
    std::str::from_utf8(text.get(..end)?)
        .ok()
        .and_then(|text| text.chars().next())
}

fn paint(out: &mut [Kind], from: usize, to: usize, kind: Kind) {
    let end = to.min(out.len());
    if from >= end {
        return;
    }
    out[from..end].fill(kind);
}

fn is_space(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\n' || c == b'\r'
}

/// A coarse count of byte visits and index probes. Only compiled into tests:
/// the malformed-input tests use it to prove the scan cannot be quadratic
/// without asserting anything about wall-clock time.
#[cfg(not(test))]
mod work {
    #[inline(always)]
    pub fn bump(_: usize) {}
}

#[cfg(test)]
mod work {
    use std::cell::Cell;

    thread_local! {
        static COUNT: Cell<usize> = const { Cell::new(0) };
    }

    pub fn bump(n: usize) {
        COUNT.with(|count| count.set(count.get() + n));
    }

    pub fn reset() {
        COUNT.with(|count| count.set(0));
    }

    pub fn take() -> usize {
        COUNT.with(|count| count.replace(0))
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Link, kinds, link_at, links, work};

    fn kinds_of(text: &str) -> Vec<Kind> {
        let mut out = vec![Kind::Plain; text.len()];
        kinds(text, &mut out);
        out
    }

    /// Each byte named by the first letter of its kind: p, b, i, s, c, m.
    fn names(text: &str) -> String {
        kinds_of(text)
            .into_iter()
            .map(|kind| match kind {
                Kind::Plain => 'p',
                Kind::Bold => 'b',
                Kind::Italic => 'i',
                Kind::Strike => 's',
                Kind::Code => 'c',
                Kind::Marker => 'm',
            })
            .collect()
    }

    #[test]
    fn bold_marks_its_text_and_keeps_its_delimiters() {
        assert_eq!(names("a **b** c"), "ppmmbmmpp");
    }

    #[test]
    fn italic_strike_and_code_are_distinct() {
        assert_eq!(names("*i*"), "mim");
        assert_eq!(names("_i_"), "mim");
        assert_eq!(names("~~s~~"), "mmsmm");
        assert_eq!(names("`c`"), "mcm");
    }

    #[test]
    fn an_unclosed_delimiter_is_just_text() {
        for text in ["**unclosed", "_unclosed", "~~unclosed", "`unclosed"] {
            assert_eq!(names(text), "p".repeat(text.len()), "{text:?}");
        }
    }

    #[test]
    fn an_escaped_delimiter_is_a_character() {
        assert_eq!(names("a \\*b\\* c"), "p".repeat("a \\*b\\* c".len()));
        let all = r"\*not\* \_soft\_ \~~gone~~ \[x](y)";
        assert_eq!(names(all), "p".repeat(all.len()));
    }

    #[test]
    fn emphasis_nests_and_flattens_to_the_innermost_style() {
        let text = "**a *b* c**";
        let got = kinds_of(text);
        assert_eq!(got[0], Kind::Marker);
        assert_eq!(got[2], Kind::Bold); // a
        assert_eq!(got[4], Kind::Marker); // inner opener
        assert_eq!(got[5], Kind::Italic); // b
        assert_eq!(got[6], Kind::Marker); // inner closer
        assert_eq!(got[8], Kind::Bold); // c
        assert_eq!(got[10], Kind::Marker);
    }

    #[test]
    fn code_and_strike_inside_bold_keep_their_words_and_drop_their_markers() {
        let text = "**a `code` and ~~gone~~ b**";
        let got = kinds_of(text);
        assert_eq!(got[2], Kind::Bold); // a
        assert_eq!(got[4], Kind::Marker); // opener `
        assert_eq!(got[6], Kind::Code);
        assert_eq!(got[9], Kind::Marker); // closer `
        assert_eq!(got[15], Kind::Marker); // opener ~~
        assert_eq!(got[18], Kind::Strike);
        assert_eq!(got[21], Kind::Marker); // closer ~~
        assert_eq!(got[24], Kind::Bold); // b
        for (index, kind) in got.iter().enumerate() {
            if *kind == Kind::Marker {
                assert!(matches!(text.as_bytes()[index], b'*' | b'`' | b'~'));
            }
        }
    }

    #[test]
    fn a_link_inside_bold_leaves_only_its_words() {
        let text = "**see [the docs](url) now**";
        let got = kinds_of(text);
        assert_eq!(got[6], Kind::Marker); // [
        assert_eq!(got[7], Kind::Bold); // the link's words inherit bold
        assert_eq!(got[14], Kind::Bold);
        assert_eq!(got[15], Kind::Marker); // ]
        assert_eq!(got[17], Kind::Marker); // the url
        assert_eq!(got[20], Kind::Marker); // )
        assert_eq!(got[23], Kind::Bold); // now
    }

    #[test]
    fn bold_inside_link_text_is_styled_not_leaked() {
        let text = "[**a**](u)";
        assert_eq!(names(text), "mmmbmmmmmm");
    }

    #[test]
    fn delimiters_inside_a_code_span_are_content() {
        let text = "`a **b** c`";
        assert_eq!(names(text), "mcccccccccm");

        let mixed = "`a [x](y) *b* ~~c~~`";
        let got = kinds_of(mixed);
        assert_eq!(got[0], Kind::Marker);
        assert_eq!(got[mixed.len() - 1], Kind::Marker);
        assert!(
            got[1..mixed.len() - 1]
                .iter()
                .all(|kind| *kind == Kind::Code)
        );
    }

    #[test]
    fn a_marker_inside_bold_is_part_of_the_bold_run() {
        let text = "**a * b**";
        assert_eq!(names(text), "mmbbbbbmm");
    }

    #[test]
    fn an_escaped_closing_delimiter_does_not_close_the_span() {
        let text = "**a \\** b**";
        let got = kinds_of(text);
        assert_eq!(got[2], Kind::Bold); // a
        assert_eq!(got[8], Kind::Bold); // b
        assert_eq!(got[text.len() - 1], Kind::Marker);
    }

    #[test]
    fn an_empty_span_is_not_a_span() {
        for text in ["****", "~~~~", "____", "````"] {
            assert_eq!(names(text), "p".repeat(text.len()), "{text:?}");
        }
    }

    #[test]
    fn asterisk_runs_pair_by_length_and_leave_no_stray_asterisk() {
        let text = "***both***";
        let got = kinds_of(text);
        for (index, kind) in got.iter().enumerate() {
            if text.as_bytes()[index] == b'*' {
                assert_eq!(*kind, Kind::Marker);
            } else {
                assert_eq!(*kind, Kind::Italic);
            }
        }

        let four = "****a****";
        let got = kinds_of(four);
        for (index, kind) in got.iter().enumerate() {
            if four.as_bytes()[index] == b'*' {
                assert_eq!(*kind, Kind::Marker);
            } else {
                assert_eq!(*kind, Kind::Bold);
            }
        }
    }

    #[test]
    fn a_longer_runs_spare_asterisks_are_text_not_styled_content() {
        assert_eq!(names("*a**"), "mipm");
        assert_eq!(names("***bold**"), "mmpbbbbmm");
    }

    #[test]
    fn an_address_with_balanced_parentheses_parses_whole() {
        let text = "[Foo](https://en.wikipedia.org/wiki/Foo_(bar)) and then";
        let link = link_at(text, 0).unwrap();
        assert_eq!(&text[link.text_start..link.text_end], "Foo");
        assert_eq!(
            &text[link.url_start..link.url_end],
            "https://en.wikipedia.org/wiki/Foo_(bar)"
        );
        assert_eq!(&text[link.next..], " and then");
    }

    #[test]
    fn link_at_requires_an_opening_bracket_at_the_offset() {
        let text = "[text](url)";
        let link = link_at(text, 0).unwrap();
        assert_eq!(
            link,
            Link {
                text_start: 1,
                text_end: 5,
                url_start: 7,
                url_end: 10,
                next: 11,
            }
        );
        assert_eq!(link_at(text, 1), None);
        assert_eq!(link_at(text, text.len()), None);
        assert_eq!(link_at(text, usize::MAX), None);
    }

    #[test]
    fn empty_escaped_unfinished_and_multiline_links_are_not_links() {
        for text in [
            "[]()",
            "[](url)",
            "\\[text](url)",
            "[text](url",
            "[a\nb](u)",
        ] {
            assert_eq!(link_at(text, 0), None, "{text:?}");
            assert!(
                kinds_of(text).iter().all(|kind| *kind == Kind::Plain),
                "{text:?}"
            );
        }
        assert_eq!(link_at("\\[text](url)", 1), None);
    }

    #[test]
    fn batch_links_match_single_link_lookups() {
        let text = "[a](b) and [c](d) and [unclosed";
        let batch = links(text);
        assert_eq!(batch.len(), 2);
        assert_eq!(links("[a](b)"), vec![link_at("[a](b)", 0).unwrap()]);
        assert_eq!(links("[x](a(b)c)"), vec![link_at("[x](a(b)c)", 0).unwrap()]);
        assert!(links("[a\\\nb](u)").is_empty());
    }

    #[test]
    fn multiple_links_are_scanned_independently() {
        let text = "[a](u) and [b](v)";
        let got = names(text);
        assert_eq!(got, "mpmmmmpppppmpmmmm");
        let first = link_at(text, 0).unwrap();
        let second_at = text.find("[b]").unwrap();
        let second = link_at(text, second_at).unwrap();
        assert_eq!(&text[first.text_start..first.text_end], "a");
        assert_eq!(&text[first.url_start..first.url_end], "u");
        assert_eq!(&text[second.text_start..second.text_end], "b");
        assert_eq!(&text[second.url_start..second.url_end], "v");
        assert_eq!(second.next, text.len());
    }

    #[test]
    fn link_words_are_rescanned_for_emphasis() {
        let text = "[**a**](u)";
        let got = kinds_of(text);
        assert_eq!(got[0], Kind::Marker);
        assert_eq!(got[1], Kind::Marker);
        assert_eq!(got[3], Kind::Bold);
        assert_eq!(got[4], Kind::Marker);
        assert_eq!(got[5], Kind::Marker);
        assert_eq!(got[9], Kind::Marker);
    }

    #[test]
    fn underscore_emphasis_ignores_unicode_punctuation() {
        assert_eq!(names("“_word_”"), "pppmiiiimppp");
        assert_eq!(names("—_word_。"), "pppmiiiimppp");
        assert_eq!(names("😀_word_"), "ppppmiiiim");
    }

    #[test]
    fn a_backslash_before_a_newline_does_not_make_a_link() {
        assert_eq!(names("[a\\\nb](u)"), "ppppppppp");
        assert_eq!(names("[x](a\\\nb)"), "ppppppppp");
    }

    #[test]
    fn underscores_inside_words_are_names_not_emphasis() {
        assert_eq!(names("foo_bar_baz"), "ppppppppppp");
        assert_eq!(names("a _b_ c"), "ppmimpp");
        assert_eq!(names("(_a_b_)"), "pmiiimp");
        assert_eq!(names("café_bar"), "p".repeat("café_bar".len()));
    }

    #[test]
    fn a_later_span_still_parses_after_an_unmatched_delimiter() {
        let text = "a * b and `code`";
        let got = kinds_of(text);
        assert!(got[..10].iter().all(|kind| *kind == Kind::Plain));
        let close = text.rfind('`').unwrap();
        assert_eq!(got[close], Kind::Marker);
        assert_eq!(got[close - 1], Kind::Code);
    }

    #[test]
    fn a_closing_backtick_after_a_backslash_still_closes() {
        let text = "`a\\` and `b`";
        let got = kinds_of(text);
        assert_eq!(got[0], Kind::Marker);
        assert_eq!(got[1], Kind::Code);
        assert_eq!(got[2], Kind::Code); // the backslash
        assert_eq!(got[3], Kind::Marker);
        assert_eq!(got[9], Kind::Marker);
        assert_eq!(got[10], Kind::Code);
        assert_eq!(got[11], Kind::Marker);
    }

    #[test]
    fn multi_backtick_code_spans_pair_by_their_own_length() {
        assert_eq!(names("``a``"), "mmcmm");
        assert_eq!(names("``a`b``"), "mmcccmm");
    }

    #[test]
    fn cjk_and_emoji_keep_one_kind_per_utf8_byte() {
        let text = "**你好** 😀 _世界_";
        let got = kinds_of(text);
        assert_eq!(got.len(), text.len());
        assert_eq!(&got[0..2], [Kind::Marker, Kind::Marker]);

        let han = text.find("你好").unwrap();
        assert!(
            got[han..han + "你好".len()]
                .iter()
                .all(|kind| *kind == Kind::Bold)
        );

        let emoji = text.find('😀').unwrap();
        assert_eq!(got[emoji], Kind::Plain);

        let world = text.find("世界").unwrap();
        assert!(
            got[world..world + "世界".len()]
                .iter()
                .all(|kind| *kind == Kind::Italic)
        );
    }

    #[test]
    fn spans_nested_past_the_depth_cap_flatten_to_the_outer_style() {
        let shallow = "**_it_**";
        assert_eq!(kinds_of(shallow)[3], Kind::Italic);

        let mut deep = String::new();
        deep.push_str(&"*".repeat(34));
        deep.push_str("_it_");
        deep.push_str(&"*".repeat(34));
        let got = kinds_of(&deep);
        assert_eq!(got[34 + 1], Kind::Bold); // i
        assert_eq!(got[34 + 2], Kind::Bold); // t
    }

    #[test]
    fn kinds_covers_every_byte_of_a_mixed_document() {
        let text = "# not inline\n\n**bold** _it_ ~~gone~~ `code` [x](y) 你好 😀";
        let got = kinds_of(text);
        assert_eq!(got.len(), text.len());
        let link = text.find("[x](y)").unwrap();
        assert_eq!(got[link], Kind::Marker); // [
        assert_eq!(got[link + 1], Kind::Plain); // x
        assert_eq!(got[link + 2], Kind::Marker); // ]
        assert_eq!(got[link + 4], Kind::Marker); // the URL is syntax too

        let han = text.find("你好").unwrap();
        assert_eq!(
            got[han..han + "你好".len()],
            [
                Kind::Plain,
                Kind::Plain,
                Kind::Plain,
                Kind::Plain,
                Kind::Plain,
                Kind::Plain
            ]
        );
        assert_eq!(got[text.find('😀').unwrap()], Kind::Plain);
    }

    #[test]
    fn malformed_long_inputs_scan_in_bounded_work() {
        let cases = [
            ("unclosed stars", "*a ".repeat(2_700)),
            ("unclosed bold", "**a ".repeat(2_000)),
            ("unclosed brackets", "[".repeat(8_000)),
            ("unclosed addresses", "[a](".repeat(2_000)),
            ("unclosed underscores", "_a ".repeat(2_700)),
            ("unclosed strikes", "~~a ".repeat(2_000)),
        ];
        for (name, text) in cases {
            assert!(text.len() >= 8_000, "{name} must exercise a long input");
            let mut got = vec![Kind::Plain; text.len()];
            work::reset();
            kinds(&text, &mut got);
            let used = work::take();
            assert!(
                used <= 128 * text.len() + 4_096,
                "{name}: {used} operations for {} bytes",
                text.len()
            );
            assert!(
                got.iter().all(|kind| *kind == Kind::Plain),
                "{name}: malformed delimiters must stay literal"
            );
        }
    }
}
