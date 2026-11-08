//! Breaking a paragraph into visual lines.
//!
//! The width of a string is not the sum of its glyphs: kerning and shaping make
//! prefix widths the only honest measure, so this asks the measurer for a
//! prefix and finds the break point by growing an exponential bracket from the
//! line's start, then binary-searching inside it. No measurement ever runs
//! past roughly twice the line's own length — measuring the whole remaining
//! paragraph per break made a long paragraph cost its whole length per line,
//! which with a real shaper is seconds per keystroke.
//!
//! The measurer is an interface, not a call into the framework, so the whole
//! algorithm is testable with fixed advances and no window — and so that
//! run-aware measurement (spans changing weight and size) is a different
//! implementation, not a different algorithm.

use crate::buffer::{boundary_at_or_before, next_boundary};
use crate::inline::Kind;
use unicode_segmentation::UnicodeSegmentation;

/// The face a range is drawn in. `mono` selects the inline-code face; body
/// text is monospace too, but that is the view's choice, not a run style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasureStyle {
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

/// One visual line: a byte range of the paragraph. Both ends are extended
/// grapheme-cluster boundaries, so a combining sequence, a ZWJ emoji or a flag
/// is never split. `end` excludes the whitespace that was broken at, so
/// selection and underline do not include it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    pub start: usize,
    pub end: usize,
}

/// Measures a prefix of text in one face.
pub trait Measurer {
    fn width(&self, text: &str, style: MeasureStyle) -> f32;
}

/// Every extended-grapheme start, plus a sentinel at `text.len()` so a
/// boundary query can always advance at the end of the paragraph.
fn grapheme_starts(text: &str) -> Vec<usize> {
    // The fast path matters: most prose is ASCII, where every byte is a
    // grapheme boundary except the LF of a CRLF pair. Running the Unicode
    // state machine over a 100k-word note costs more than the rest of layout.
    if text.is_ascii() {
        let bytes = text.as_bytes();
        let mut starts = Vec::with_capacity(text.len().saturating_add(1));
        starts.push(0);
        for at in 1..text.len() {
            if !(bytes[at - 1] == b'\r' && bytes[at] == b'\n') {
                starts.push(at);
            }
        }
        if starts.last().copied() != Some(text.len()) {
            starts.push(text.len());
        }
        return starts;
    }

    let mut starts: Vec<usize> = text.grapheme_indices(true).map(|(at, _)| at).collect();
    if starts.first().copied() != Some(0) {
        starts.insert(0, 0);
    }
    if starts.last().copied() != Some(text.len()) {
        starts.push(text.len());
    }
    starts
}

/// The grapheme-boundary view of a paragraph.
///
/// Plain ASCII with no carriage returns has a boundary at every byte, so the
/// common case stores no vector at all; anything with a CRLF pair or a
/// non-ASCII scalar falls back to [`grapheme_starts`] (which itself takes the
/// ASCII fast path for CRLF-only text).
struct Boundaries {
    len: usize,
    /// Empty means "every byte offset is a boundary".
    starts: Vec<usize>,
}

impl Boundaries {
    fn new(text: &str) -> Self {
        // A CRLF pair is one grapheme cluster; any other ASCII text is one
        // byte per cluster.
        if text.is_ascii() && !text.as_bytes().contains(&b'\r') {
            Self {
                len: text.len(),
                starts: Vec::new(),
            }
        } else {
            Self {
                len: text.len(),
                starts: grapheme_starts(text),
            }
        }
    }

    fn floor(&self, at: usize) -> usize {
        if self.starts.is_empty() {
            return at.min(self.len);
        }
        let index = self.starts.partition_point(|&start| start <= at);
        self.starts[index.saturating_sub(1)]
    }

    fn next(&self, at: usize) -> usize {
        if self.starts.is_empty() {
            return at.saturating_add(1).min(self.len);
        }
        let index = self.starts.partition_point(|&start| start <= at);
        self.starts.get(index).copied().unwrap_or(self.len)
    }

    fn prev(&self, at: usize) -> usize {
        if self.starts.is_empty() {
            return at.saturating_sub(1);
        }
        let index = self.starts.partition_point(|&start| start < at);
        self.starts[index.saturating_sub(1)]
    }

    fn unit_end(&self, at: usize) -> usize {
        if self.starts.is_empty() {
            return at.saturating_add(1).min(self.len);
        }
        let index = self.starts.partition_point(|&start| start < at);
        self.starts.get(index + 1).copied().unwrap_or(self.len)
    }
}

/// A unit a line may break after *without* including it: space, tab, a soft
/// newline, or CRLF. The line ends before the whitespace.
fn is_soft_break(unit: &str) -> bool {
    // A newline inside a paragraph is markdown's soft break: a space, not a
    // line ending. Treating it as a break is what reflows a note that was
    // hard-wrapped wider than the column.
    matches!(unit.as_bytes().first(), Some(b' ' | b'\t' | b'\n')) || unit == "\r\n"
}

/// A unit a line may break after: the soft breaks, plus a hyphen, which is
/// the one piece of punctuation English actually breaks on.
fn is_break_unit(unit: &str) -> bool {
    is_soft_break(unit) || unit.as_bytes().first() == Some(&b'-')
}

/// Whitespace a line end may drop: the old byte-level trim also dropped `\r`.
fn is_trim_whitespace(unit: &str) -> bool {
    matches!(unit.as_bytes().first(), Some(b' ' | b'\t' | b'\n' | b'\r'))
}

/// Whitespace that the next line swallows. `\r` is deliberately not here, as
/// in the original: a lone carriage return is content, while CRLF is consumed
/// as one grapheme unit by the break itself.
fn is_leading_skip(unit: &str) -> bool {
    matches!(unit.as_bytes().first(), Some(b' ' | b'\t' | b'\n'))
}

/// The face a kind is drawn in. Strike keeps the base face: the line through
/// it changes the paint, not the advance, so the caret does not move.
fn style_for(base: MeasureStyle, kind: Kind) -> MeasureStyle {
    match kind {
        Kind::Bold => MeasureStyle {
            size: base.size,
            bold: true,
            italic: false,
            mono: base.mono,
        },
        Kind::Italic => MeasureStyle {
            size: base.size,
            bold: base.bold,
            italic: true,
            mono: base.mono,
        },
        Kind::Code => MeasureStyle {
            size: base.size,
            bold: base.bold,
            italic: false,
            mono: true,
        },
        Kind::Plain | Kind::Marker | Kind::Strike => base,
    }
}

/// The width of `text[from..to]`, measured run by run.
///
/// A bold word is wider than the same word in regular weight, so a paragraph
/// with any styling cannot be measured with one style. Summing per run is
/// exact rather than approximate: a face change ends kerning anyway, so there
/// is no pair of glyphs across a boundary whose spacing would be lost.
pub fn width_range(
    m: &dyn Measurer,
    text: &str,
    kinds: Option<&[Kind]>,
    base: MeasureStyle,
    from: usize,
    to: usize,
) -> f32 {
    if from >= to || from >= text.len() {
        return 0.0;
    }
    let to = boundary_at_or_before(text, to);
    let from = boundary_at_or_before(text, from).min(to);
    if from >= to {
        return 0.0;
    }

    let kinds = kinds.filter(|kinds| kinds.len() == text.len());
    let Some(kinds) = kinds else {
        // No run boundaries: preserve the whole-range measurement except for
        // a soft newline, which is painted as a space. Do not measure
        // individual UTF-8 bytes — that changes kerning and codepoint counts.
        let mut total = 0.0;
        let mut start = from;
        while start < to {
            let segment = &text[start..to];
            let newline = segment.find('\n').map_or(to, |offset| start + offset);
            total += m.width(&text[start..newline], base);
            if newline < to {
                total += m.width(" ", base);
            }
            start = newline + 1;
        }
        return total;
    };

    let bytes = text.as_bytes();
    let mut total = 0.0;
    let mut i = from;
    while i < to {
        // A soft newline has no advance in a shaper, but it is painted as a
        // space by the editor. Measuring the raw byte put the caret and the
        // selection wash one character-width left per soft newline.
        let kind = kinds[i];
        if bytes[i] == b'\n' {
            total += m.width(" ", style_for(base, kind));
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < to && kinds[j] == kind && bytes[j] != b'\n' {
            j += 1;
        }
        // A caller-supplied kinds array should already break on codepoint
        // boundaries (the scanner paints whole scalars), but a malformed one
        // must not panic on a slice.
        if j < text.len() && !text.is_char_boundary(j) {
            j = next_boundary(text, j).min(to);
        }
        if j > i {
            total += m.width(&text[i..j], style_for(base, kind));
        }
        i = j;
    }
    total
}

/// The largest offset in `(from, text.len()]` whose prefix still fits
/// `max_width`.
///
/// The break point is found by growing an exponential bracket from `from`
/// until a prefix passes the column, then binary-searching inside it. The old
/// way measured the whole remaining paragraph first — one full-width shaping
/// call per line — so a keystroke in a 20k-word paragraph cost seconds:
/// quadratic in the paragraph, invisible with a fake measurer.
fn fit(
    m: &dyn Measurer,
    text: &str,
    kinds: Option<&[Kind]>,
    style: MeasureStyle,
    from: usize,
    max_width: f32,
    boundaries: &Boundaries,
) -> usize {
    let mut lo = from;
    let mut hi = text.len();

    // Grow a bracket [lo, hi] where `lo` fits and `hi` may not: each probe
    // doubles the step, so no measurement ever runs past roughly twice the
    // line's own length.
    let mut step: usize = 16;
    while lo < text.len() {
        let probe = lo.saturating_add(step).min(text.len());
        if step < text.len() {
            step = step.saturating_mul(2);
        }
        let at = boundaries.floor(probe);
        if at <= lo {
            continue;
        }
        if width_range(m, text, kinds, style, from, at) <= max_width {
            lo = at;
            continue;
        }
        hi = at;
        break;
    }
    if lo >= text.len() {
        return text.len();
    }

    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let at = boundaries.floor(mid);
        if at <= lo {
            break;
        }
        if width_range(m, text, kinds, style, from, at) <= max_width {
            lo = at;
        } else {
            hi = at - 1;
        }
        if hi <= lo {
            break;
        }
    }
    // Never return a range that does not advance: an empty line would loop.
    // The next *grapheme*, not the next codepoint: a family emoji is one unit.
    if lo <= from {
        return boundaries.next(from);
    }
    // The binary search can stop before the grapheme just past `lo` when the
    // midpoint fell inside it. Test that boundary once so a line is filled.
    let next = boundaries.next(lo);
    if next > lo && next <= hi && width_range(m, text, kinds, style, from, next) <= max_width {
        return next;
    }
    lo
}

/// Wraps `text` into visual lines.
///
/// `kinds` gives one inline style per byte (or `None` for one face). `hang` is
/// a hanging indent: continuation lines wrap to `max_width - hang`, which is
/// what puts a wrapped list item's words under its text rather than under its
/// marker. A whitespace-only paragraph still yields one line, so a blank code
/// line keeps its height.
pub fn wrap(
    m: &dyn Measurer,
    text: &str,
    kinds: Option<&[Kind]>,
    style: MeasureStyle,
    max_width: f32,
    hang: f32,
) -> Vec<Line> {
    let mut lines = Vec::new();
    wrap_into(m, text, kinds, style, max_width, hang, &mut lines);
    lines
}

/// [`wrap`], writing into a caller-owned buffer that is cleared first. Layout
/// reuses one `Vec` for every block instead of allocating a fresh line list per
/// paragraph.
pub(crate) fn wrap_into(
    m: &dyn Measurer,
    text: &str,
    kinds: Option<&[Kind]>,
    style: MeasureStyle,
    max_width: f32,
    hang: f32,
    lines: &mut Vec<Line>,
) {
    wrap_into_from(m, text, kinds, style, max_width, hang, true, lines);
}

/// [`wrap_into`] for a suffix that starts on a continuation line. The first
/// suffix line already sits under the list/heading hang, so it gets the
/// continuation width instead of the full column.
#[allow(clippy::too_many_arguments)]
pub(crate) fn wrap_into_from(
    m: &dyn Measurer,
    text: &str,
    kinds: Option<&[Kind]>,
    style: MeasureStyle,
    max_width: f32,
    hang: f32,
    first_line_full: bool,
    lines: &mut Vec<Line>,
) {
    lines.clear();
    if text.is_empty() {
        lines.push(Line { start: 0, end: 0 });
        return;
    }

    let boundaries = Boundaries::new(text);
    let mut start = 0;
    while start < text.len() {
        // Only the first line gets the full width; the rest sit beside the
        // hanging indent.
        let available = if start == 0 && first_line_full {
            max_width
        } else {
            max_width - hang
        };
        let limit = fit(m, text, kinds, style, start, available, &boundaries);
        if limit >= text.len() {
            lines.push(Line {
                start,
                end: text.len(),
            });
            break;
        }

        // Greedy filling. A soft break exactly at `limit` is the best break
        // the line can have: the prefix fits, and the whitespace itself is
        // consumed rather than drawn. Missing this used to break
        // "one two three" at 70pt into three lines instead of two.
        let after_limit = boundaries.unit_end(limit);
        let mut cut = limit;
        if is_soft_break(&text[limit..after_limit]) {
            cut = after_limit;
        } else {
            // Otherwise prefer the last break opportunity inside the fitted
            // prefix. A hyphen exactly at the edge is not included: it would
            // push the line one glyph past the column. If there is no break,
            // the word is longer than the column and gets cut at a grapheme
            // boundary.
            let mut back = boundaries.prev(limit);
            while back > start {
                let back_end = boundaries.unit_end(back);
                if is_break_unit(&text[back..back_end]) {
                    cut = back_end;
                    break;
                }
                back = boundaries.prev(back);
            }
        }
        if cut <= start {
            cut = boundaries.next(start);
        }

        let mut shown = cut;
        // A break at a newline or a space must not leave it at the end of the
        // line: it belongs to the boundary, not to the text being drawn.
        while shown > start {
            let unit_start = boundaries.prev(shown);
            if unit_start < start || !is_trim_whitespace(&text[unit_start..shown]) {
                break;
            }
            shown = unit_start;
        }
        // Leading whitespace that did not fit the column is swallowed with
        // the break, not drawn as a visual line of its own: a zero-length
        // line still occupies the page's vertical rhythm while showing
        // nothing.
        if shown > start {
            lines.push(Line { start, end: shown });
        }

        start = cut;
        while start < text.len() {
            let end = boundaries.unit_end(start);
            if !is_leading_skip(&text[start..end]) {
                break;
            }
            start = end;
        }
        // A line of trailing spaces still has to consume them, or the loop
        // spins on whitespace at the end of a paragraph.
        if start >= text.len() {
            break;
        }
    }
    // Every break the loop found was air: the text is nothing but whitespace
    // the column could not hold. It is still a line of the document — a
    // whitespace-only code line must keep its height on the page rather than
    // vanish between its fences.
    if lines.is_empty() {
        lines.push(Line { start: 0, end: 0 });
    }
}

#[cfg(test)]
mod tests {
    use super::{Line, MeasureStyle, Measurer, grapheme_starts, width_range, wrap};
    use crate::buffer::next_boundary;
    use crate::inline::{Kind, kinds};

    const BODY: MeasureStyle = MeasureStyle {
        size: 16.0,
        bold: false,
        italic: false,
        mono: false,
    };

    /// Every scalar is a fixed advance; bold and italic widen it, code
    /// narrows it. Prefix widths are exact sums, so a test can state the
    /// expected line without guessing at shaping.
    struct FakeMeasurer {
        advance: f32,
        wide: f32,
        bold: f32,
        italic: f32,
        mono: f32,
    }

    impl FakeMeasurer {
        fn new(advance: f32) -> Self {
            Self {
                advance,
                wide: advance * 2.0,
                bold: 1.5,
                italic: 1.2,
                mono: 0.9,
            }
        }

        fn advance_of(&self, ch: char) -> f32 {
            match ch {
                '\u{4e00}'..='\u{9fff}' | '😀' => self.wide,
                _ => self.advance,
            }
        }
    }

    impl Measurer for FakeMeasurer {
        fn width(&self, text: &str, style: MeasureStyle) -> f32 {
            let mut scale = 1.0;
            if style.bold {
                scale *= self.bold;
            }
            if style.italic {
                scale *= self.italic;
            }
            if style.mono {
                scale *= self.mono;
            }
            text.chars().map(|ch| self.advance_of(ch) * scale).sum()
        }
    }

    fn simple() -> FakeMeasurer {
        FakeMeasurer::new(10.0)
    }

    fn widths(m: &FakeMeasurer, text: &str, lines: &[Line]) -> Vec<f32> {
        lines
            .iter()
            .map(|line| width_range(m, text, None, BODY, line.start, line.end))
            .collect()
    }

    #[test]
    fn empty_text_is_one_empty_line() {
        let lines = wrap(&simple(), "", None, BODY, 100.0, 0.0);
        assert_eq!(lines, [Line { start: 0, end: 0 }]);
    }

    #[test]
    fn whitespace_only_text_still_yields_one_line() {
        let wide = wrap(&simple(), "   ", None, BODY, 100.0, 0.0);
        assert_eq!(wide, [Line { start: 0, end: 3 }]);

        // At a narrow column every break is air; the line survives with an
        // empty range so the page keeps its vertical rhythm.
        let narrow = wrap(&simple(), "   ", None, BODY, 1.0, 0.0);
        assert_eq!(narrow, [Line { start: 0, end: 0 }]);
    }

    #[test]
    fn a_paragraph_breaks_at_spaces_and_drops_the_break() {
        let text = "aa bb cc dd";
        let lines = wrap(&simple(), text, None, BODY, 50.0, 0.0);
        assert_eq!(
            lines,
            [Line { start: 0, end: 5 }, Line { start: 6, end: 11 }]
        );
        assert_eq!(widths(&simple(), text, &lines), [50.0, 50.0]);
    }

    #[test]
    fn exact_fill_uses_the_break_at_the_limit() {
        // "one two" is exactly 70; the space at the limit is a better break
        // than the earlier one, so this is two lines, not three.
        let text = "one two three";
        let lines = wrap(&simple(), text, None, BODY, 70.0, 0.0);
        assert_eq!(
            lines,
            [Line { start: 0, end: 7 }, Line { start: 8, end: 13 }]
        );
        assert_eq!(widths(&simple(), text, &lines), [70.0, 50.0]);
    }

    #[test]
    fn a_hyphen_at_the_exact_edge_is_not_pushed_past_the_column() {
        let text = "foo-bar";
        let lines = wrap(&simple(), text, None, BODY, 40.0, 0.0);
        assert_eq!(lines[0], Line { start: 0, end: 4 });
        assert_eq!(&text[lines[0].start..lines[0].end], "foo-");
        assert!(width_range(&simple(), text, None, BODY, lines[0].start, lines[0].end) <= 40.0);
    }

    #[test]
    fn zwj_emoji_and_flags_are_whole_graphemes() {
        let text = "a👩‍👩‍👧‍👦b🇺🇸c";
        let starts = grapheme_starts(text);
        let lines = wrap(&simple(), text, None, BODY, 15.0, 0.0);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                starts.contains(&line.start) && starts.contains(&line.end),
                "line {:?} split a grapheme cluster",
                line
            );
        }
        // The family cluster and the flag are each present whole in some line.
        let family = text.find('👩').unwrap();
        let family_end = family + "👩‍👩‍👧‍👦".len();
        assert!(
            lines
                .iter()
                .any(|line| line.start <= family && line.end >= family_end)
        );
        let flag = text.find('🇺').unwrap();
        let flag_end = flag + "🇺🇸".len();
        assert!(
            lines
                .iter()
                .any(|line| line.start <= flag && line.end >= flag_end)
        );
    }

    #[test]
    fn a_combining_mark_stays_with_its_base() {
        let text = "ae\u{301}b";
        let starts = grapheme_starts(text);
        let lines = wrap(&simple(), text, None, BODY, 15.0, 0.0);
        assert_eq!(
            lines,
            [
                Line { start: 0, end: 1 },
                Line { start: 1, end: 4 },
                Line { start: 4, end: 5 },
            ]
        );
        for line in &lines {
            assert!(starts.contains(&line.start) && starts.contains(&line.end));
        }
    }

    #[test]
    fn hyphens_are_break_opportunities_and_stay_on_the_line() {
        let text = "foo-bar baz";
        let lines = wrap(&simple(), text, None, BODY, 50.0, 0.0);
        assert_eq!(lines[0], Line { start: 0, end: 4 });
        assert_eq!(&text[lines[0].start..lines[0].end], "foo-");
    }

    #[test]
    fn tabs_are_break_opportunities() {
        let text = "aa\tbb";
        let lines = wrap(&simple(), text, None, BODY, 45.0, 0.0);
        assert_eq!(
            lines,
            [Line { start: 0, end: 2 }, Line { start: 3, end: 5 }]
        );
    }

    #[test]
    fn a_soft_newline_measures_as_a_space_and_can_break() {
        let text = "aa\nbb";
        let wide = wrap(&simple(), text, None, BODY, 50.0, 0.0);
        assert_eq!(wide, [Line { start: 0, end: 5 }]);
        assert_eq!(width_range(&simple(), text, None, BODY, 0, 5), 50.0);

        let narrow = wrap(&simple(), text, None, BODY, 45.0, 0.0);
        assert_eq!(
            narrow,
            [Line { start: 0, end: 2 }, Line { start: 3, end: 5 }]
        );
        assert!(narrow.iter().all(|line| line.end <= 5));
    }

    #[test]
    fn crlf_is_one_grapheme_and_one_soft_break() {
        let text = "aa\r\nbb";
        let starts = grapheme_starts(text);
        let lines = wrap(&simple(), text, None, BODY, 50.0, 0.0);
        assert_eq!(
            lines,
            [Line { start: 0, end: 2 }, Line { start: 4, end: 6 }]
        );
        for line in &lines {
            assert!(starts.contains(&line.start));
            assert!(starts.contains(&line.end));
        }
    }

    #[test]
    fn a_long_word_is_cut_at_codepoint_boundaries() {
        let text = "abcdefghij";
        let lines = wrap(&simple(), text, None, BODY, 25.0, 0.0);
        assert_eq!(
            lines,
            [
                Line { start: 0, end: 2 },
                Line { start: 2, end: 4 },
                Line { start: 4, end: 6 },
                Line { start: 6, end: 8 },
                Line { start: 8, end: 10 },
            ]
        );
        for line in &lines {
            assert!(width_range(&simple(), text, None, BODY, line.start, line.end) <= 25.0);
        }
    }

    #[test]
    fn cjk_and_emoji_are_never_split() {
        let m = simple();
        let text = "你好世界😀你好世界😀";
        let lines = wrap(&m, text, None, BODY, 45.0, 0.0);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(text.is_char_boundary(line.start));
            assert!(text.is_char_boundary(line.end));
            assert!(line.end > line.start);
            assert!(
                width_range(&m, text, None, BODY, line.start, line.end) <= 45.0,
                "a line of whole wide scalars can still overflow only if one scalar is wider than the column"
            );
        }
    }

    #[test]
    fn a_zero_advance_measurer_cannot_loop() {
        struct Zero;
        impl Measurer for Zero {
            fn width(&self, _text: &str, _style: MeasureStyle) -> f32 {
                0.0
            }
        }

        let text = "abcdef";
        let lines = wrap(&Zero, text, None, BODY, 0.0, 0.0);
        assert_eq!(lines, [Line { start: 0, end: 6 }]);

        // A negative column makes every probe fail; each line must still
        // advance by at least one codepoint.
        let lines = wrap(&Zero, text, None, BODY, -1.0, 0.0);
        assert_eq!(lines.len(), text.len());
        for (index, line) in lines.iter().enumerate() {
            assert_eq!(line.start, index);
            assert_eq!(line.end, index + 1);
        }
    }

    #[test]
    fn hang_only_changes_continuation_lines() {
        let text = "aaaaaaaaaaaaaaaaaaaa";
        let lines = wrap(&simple(), text, None, BODY, 100.0, 90.0);
        assert!(lines.len() > 1);
        assert_eq!(
            width_range(&simple(), text, None, BODY, lines[0].start, lines[0].end),
            100.0
        );
        for line in &lines[1..] {
            assert!(
                width_range(&simple(), text, None, BODY, line.start, line.end) <= 10.0,
                "continuation lines sit beside the hanging indent"
            );
        }
    }

    #[test]
    fn width_range_applies_kind_styles_run_by_run() {
        let m = simple();
        let text = "a **b** `c`";
        let mut kinds_buf = vec![Kind::Plain; text.len()];
        kinds(text, &mut kinds_buf);
        let whole = width_range(&m, text, Some(&kinds_buf), BODY, 0, text.len());

        let bold = MeasureStyle { bold: true, ..BODY };
        let mono = MeasureStyle { mono: true, ..BODY };
        // "a " (plain), "**" markers (plain), "b" bold, "**" markers, " "
        // plain, "`" marker, "c" code, "`" marker.
        let expected = m.width("a ", BODY)
            + m.width("**", BODY)
            + m.width("b", bold)
            + m.width("**", BODY)
            + m.width(" ", BODY)
            + m.width("`", BODY)
            + m.width("c", mono)
            + m.width("`", BODY);
        assert!((whole - expected).abs() < f32::EPSILON);
    }

    #[test]
    fn width_range_clamps_and_handles_soft_newlines() {
        let m = simple();
        let text = "ab\ncd";
        assert_eq!(width_range(&m, text, None, BODY, 0, 5), 50.0);
        assert_eq!(width_range(&m, text, None, BODY, 2, 2), 0.0);
        assert_eq!(width_range(&m, text, None, BODY, 4, 2), 0.0);
        assert_eq!(width_range(&m, text, None, BODY, 0, 999), 50.0);

        // Offsets inside a codepoint snap back rather than panic.
        let text = "aé中";
        assert_eq!(width_range(&m, text, None, BODY, 2, 6), 30.0);
        assert_eq!(width_range(&m, text, None, BODY, 0, 4), 20.0);
    }

    #[test]
    fn full_byte_coverage_no_overlap_and_no_empty_lines() {
        let m = simple();
        let corpus = [
            "",
            " ",
            "   \t\n",
            "one two three four five six seven",
            "a-very-long-hyphenated-word and more",
            "line one\nline two\nline three",
            "你好世界😀 a mix of widths",
            "**bold** and `code` and _italic_",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "\n\n\n",
            "a\n",
        ];
        for text in corpus {
            for width in [0.0, 1.0, 10.0, 25.0, 45.0, 100.0, 1_000.0] {
                for hang in [0.0, 8.0, 20.0] {
                    let lines = wrap(&m, text, None, BODY, width, hang);
                    assert!(!lines.is_empty(), "{text:?} {width} {hang}");
                    let mut covered = vec![false; text.len()];
                    let mut previous_end = 0;
                    for line in &lines {
                        assert!(line.start <= line.end, "{text:?}");
                        assert!(line.start >= previous_end, "{text:?}");
                        assert!(text.is_char_boundary(line.start));
                        assert!(text.is_char_boundary(line.end));
                        if !text.is_empty() {
                            assert!(line.end > line.start || text.trim().is_empty());
                        }
                        for slot in &mut covered[line.start..line.end] {
                            assert!(!*slot, "overlap in {text:?}");
                            *slot = true;
                        }
                        previous_end = line.end;
                    }
                    for (index, byte) in text.bytes().enumerate() {
                        if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                            continue;
                        }
                        assert!(covered[index], "byte {index} missing from {text:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_line_start_is_a_boundary_for_random_text() {
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

        let m = simple();
        let pieces = [
            "a",
            " ",
            "\t",
            "\n",
            "-",
            "é",
            "中",
            "😀",
            "e\u{301}",
            "🇺🇸",
            "👩‍👩‍👧‍👦",
            "word",
            "  ",
            "xy",
        ];
        let mut rng = Lcg(0x00dd_1234_abcd_9876);
        for _ in 0..500 {
            let mut text = String::new();
            for _ in 0..rng.below(40) {
                text.push_str(pieces[rng.below(pieces.len())]);
            }
            let width = rng.below(80) as f32;
            let starts = grapheme_starts(&text);
            let lines = wrap(&m, &text, None, BODY, width, rng.below(30) as f32);
            assert!(!lines.is_empty());
            let mut at = 0;
            for line in lines {
                assert!(line.start >= at);
                assert!(text.is_char_boundary(line.start));
                assert!(text.is_char_boundary(line.end));
                assert!(line.start <= line.end);
                assert!(starts.contains(&line.start));
                assert!(starts.contains(&line.end));
                at = line.end;
            }
            assert!(text[at..].trim().is_empty());
        }
    }

    #[test]
    fn a_line_that_fits_is_not_measured_past_its_own_end() {
        // The fake measurer is exact, so a correct bracket-and-search must
        // produce the same break as a hypothetical full-prefix search.
        struct Counting<'a> {
            m: &'a FakeMeasurer,
            longest: std::cell::Cell<usize>,
        }
        impl Measurer for Counting<'_> {
            fn width(&self, text: &str, style: MeasureStyle) -> f32 {
                self.longest.set(self.longest.get().max(text.len()));
                self.m.width(text, style)
            }
        }

        let m = simple();
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau";
        let counting = Counting {
            m: &m,
            longest: std::cell::Cell::new(0),
        };
        let lines = wrap(&counting, text, None, BODY, 80.0, 0.0);
        assert!(lines.len() > 1);
        // No single measurement approaches the paragraph's length; the
        // exponential bracket keeps each one near its line.
        assert!(
            counting.longest.get() <= 32,
            "longest measurement was {} for {} bytes",
            counting.longest.get(),
            text.len()
        );
    }

    #[test]
    fn a_narrow_column_still_advances_by_one_codepoint() {
        // A column too narrow for even one wide scalar cannot return an empty
        // line: a line that does not advance would loop.
        let m = simple();
        let text = "中😀";
        let lines = wrap(&m, text, None, BODY, 0.0, 0.0);
        assert_eq!(
            lines,
            [Line { start: 0, end: 3 }, Line { start: 3, end: 7 }]
        );
        assert_eq!(next_boundary(text, 0), 3);
    }
}
