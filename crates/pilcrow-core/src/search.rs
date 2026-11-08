//! Literal search over a document's bytes.
//!
//! Matching is intentionally plain text: Pilcrow's page is Markdown source, so
//! searching the bytes is the only answer that can never disagree with what is
//! saved. Case-insensitive matching folds through Unicode with a byte map, so
//! a match that spans an expansion such as `ß` -> `ss` still maps back to the
//! original character boundaries.

use std::ops::Range;

/// Every non-overlapping occurrence of `needle`, in byte order.
pub fn find_all(text: &str, needle: &str, case_sensitive: bool) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        return text
            .match_indices(needle)
            .map(|(start, found)| start..start + found.len())
            .collect();
    }

    let (folded, map) = fold_with_map(text);
    let folded_needle = needle.to_uppercase();
    if folded_needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut from = 0;
    while from < folded.len() {
        let Some(offset) = folded[from..].find(&folded_needle) else {
            break;
        };
        let start = from + offset;
        let end = start + folded_needle.len();
        let original_start = map[start];
        let original_end = map[end];
        if original_start < original_end {
            found.push(original_start..original_end);
        }
        from = end.max(start + 1);
    }
    found
}

/// Fold every character with Unicode uppercase mapping and remember the source
/// byte offset of each folded byte. Uppercase is used because it performs the
/// common expansions (`ß` -> `SS`, ligatures) that lowercase leaves alone. The
/// final map entry is the source length, so the end of a match maps to a real
/// byte boundary.
fn fold_with_map(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len() + 1);
    for (start, ch) in text.char_indices() {
        for upper in ch.to_uppercase() {
            folded.push(upper);
            for _ in 0..upper.len_utf8() {
                map.push(start);
            }
        }
    }
    map.push(text.len());
    (folded, map)
}

/// Replace sorted, non-overlapping ranges with the same literal.
pub fn replace_ranges(text: &str, ranges: &[Range<usize>], replacement: &str) -> String {
    let removed: usize = ranges
        .iter()
        .map(|range| range.end.saturating_sub(range.start))
        .sum();
    let mut out = String::with_capacity(
        text.len()
            .saturating_sub(removed)
            .saturating_add(replacement.len().saturating_mul(ranges.len())),
    );
    let mut cursor = 0;
    for range in ranges {
        if range.start < cursor
            || range.end < range.start
            || range.end > text.len()
            || !text.is_char_boundary(range.start)
            || !text.is_char_boundary(range.end)
        {
            continue;
        }
        out.push_str(&text[cursor..range.start]);
        out.push_str(replacement);
        cursor = range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// The match a search should land on. Forward searches start at or after
/// `from`; backward searches take the last match ending before it. Both wrap.
pub fn next_index(matches: &[Range<usize>], from: usize, forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if forward {
        let index = matches.partition_point(|range| range.end <= from);
        Some(if index == matches.len() { 0 } else { index })
    } else {
        let index = matches.partition_point(|range| range.start < from);
        Some(if index == 0 {
            matches.len() - 1
        } else {
            index - 1
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_literal_matches_in_order() {
        let text = "one two one one";
        assert_eq!(find_all(text, "one", true), vec![0..3, 8..11, 12..15]);
        assert!(find_all(text, "", true).is_empty());
        assert!(find_all(text, "missing", true).is_empty());
    }

    #[test]
    fn ignores_ascii_case_when_asked() {
        let text = "Rust rust RUST";
        assert_eq!(find_all(text, "rust", false), vec![0..4, 5..9, 10..14]);
        assert_eq!(find_all(text, "Rust", true), vec![0..4]);
    }

    #[test]
    fn unicode_folding_maps_back_to_source_boundaries() {
        let text = "Straße STRASSE";
        let matches = find_all(text, "strasse", false);
        assert_eq!(matches, vec![0..7, 8..15]);

        // An expansion inside one source character must not split it.
        let text = "aßb";
        let matches = find_all(text, "ss", false);
        assert_eq!(matches, vec![1..3]);
        assert!(text.is_char_boundary(matches[0].start));
        assert!(text.is_char_boundary(matches[0].end));
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init, clippy::reversed_empty_ranges)]
    fn replacing_ranges_keeps_the_text_between_them() {
        let text = "a cat and a cat";
        let ranges = find_all(text, "cat", true);
        assert_eq!(replace_ranges(text, &ranges, "dog"), "a dog and a dog");
        assert_eq!(replace_ranges(text, &[], "dog"), text);
        let one = vec![0..3];
        assert_eq!(replace_ranges("cat", &one, "longer"), "longer");
        assert_eq!(replace_ranges("hello", &[5..2], "x"), "hello");
        assert_eq!(replace_ranges("日本語", &[1..4], "x"), "日本語");
    }

    #[test]
    fn next_and_previous_wrap_without_skipping() {
        let ranges = vec![0..2, 5..7, 10..12];
        assert_eq!(next_index(&ranges, 0, true), Some(0));
        assert_eq!(next_index(&ranges, 2, true), Some(1));
        assert_eq!(next_index(&ranges, 11, true), Some(2));
        assert_eq!(next_index(&ranges, 12, true), Some(0));
        assert_eq!(next_index(&ranges, 12, false), Some(2));
        assert_eq!(next_index(&ranges, 0, false), Some(2));
        assert_eq!(next_index(&ranges, 6, false), Some(1));
        assert_eq!(next_index(&[], 0, true), None);
    }
}
