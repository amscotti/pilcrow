//! System spelling, kept off the typing path.
//!
//! macOS uses `NSSpellChecker`; other platforms currently return no ranges.
//! The checker runs only after an idle pause, and its results are filtered
//! against the document's prose blocks so code, frontmatter, rules and inline
//! link URLs never get a squiggle.

use std::ops::Range;

use pilcrow_core::{Block, BlockKind, InlineKind, inline_kinds};

#[cfg(target_os = "macos")]
mod platform {
    use std::ops::Range;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSSpellChecker, NSTextCheckingOptionKey};
    use objc2_foundation::{
        NSArray, NSDictionary, NSNotFound, NSString, NSTextCheckingResult, NSTextCheckingType,
    };

    pub fn misspelled(text: &str) -> Vec<Range<usize>> {
        if text.len() > 1_000_000 {
            return Vec::new();
        }
        let checker = NSSpellChecker::sharedSpellChecker();
        if !text.contains('\n') {
            return check(&checker, text, 0);
        }
        // NSSpellChecker's batch API is unreliable for multi-paragraph text
        // on macOS: it may return nothing, or only part of the document.
        // Check each line separately and offset the result, so a partial
        // batch can never silently under-report.
        let mut found = Vec::new();
        let mut offset = 0;
        for line in text.split('\n') {
            if !line.trim().is_empty() {
                found.extend(check(&checker, line, offset));
            }
            offset += line.len() + 1;
        }
        found.sort_by_key(|range| range.start);
        found.dedup();
        found
    }

    fn check(checker: &NSSpellChecker, text: &str, offset: usize) -> Vec<Range<usize>> {
        let string = NSString::from_str(text);
        let utf16_len = text.encode_utf16().count();
        let options = NSDictionary::<NSTextCheckingOptionKey, AnyObject>::new();
        let results: Retained<NSArray<NSTextCheckingResult>> = unsafe {
            checker.checkString_range_types_options_inSpellDocumentWithTag_orthography_wordCount(
                &string,
                objc2_foundation::NSRange::new(0, utf16_len),
                NSTextCheckingType::Spelling.0,
                Some(&options),
                0,
                None,
                std::ptr::null_mut(),
            )
        };
        // Collect the UTF-16 range ends first, then map them to byte offsets
        // in one pass over the text. Mapping each hit from the start of the
        // string made a long line with many hits quadratic.
        let mut units = Vec::new();
        for index in 0..results.count() {
            let result = results.objectAtIndex(index);
            let range = result.range();
            if range.location == NSNotFound as usize {
                continue;
            }
            units.push(range.location);
            units.push(range.location.saturating_add(range.length));
        }
        let bytes = super::byte_offsets_for_utf16(text, &units);
        let mut found = Vec::new();
        for pair in bytes.as_chunks::<2>().0 {
            let (start, end) = (pair[0], pair[1]);
            if start < end {
                found.push(offset + start..offset + end);
            }
        }
        found
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::ops::Range;

    pub fn misspelled(_text: &str) -> Vec<Range<usize>> {
        Vec::new()
    }
}

/// Map UTF-16 code-unit offsets to byte offsets in one pass over `text`.
///
/// A target that falls inside a surrogate pair maps to the end of that
/// character, and a target past the end maps to `text.len()` — the same
/// answers a per-target scan gave, without rescanning from byte zero for
/// every spellcheck hit. Targets are expected in non-decreasing order (the
/// platform checker reports hits in document order); any other order falls
/// back to the per-target scan so the result stays correct.
#[cfg(any(target_os = "macos", test))]
fn byte_offsets_for_utf16(text: &str, targets: &[usize]) -> Vec<usize> {
    if targets.windows(2).any(|pair| pair[1] < pair[0]) {
        return targets
            .iter()
            .map(|&units| byte_at_utf16(text, units))
            .collect();
    }
    let mut out = Vec::with_capacity(targets.len());
    let mut next = 0;
    let mut seen = 0;
    for (byte, ch) in text.char_indices() {
        while next < targets.len() && targets[next] <= seen {
            out.push(byte);
            next += 1;
        }
        let after = seen + ch.len_utf16();
        while next < targets.len() && targets[next] < after {
            out.push(byte + ch.len_utf8());
            next += 1;
        }
        seen = after;
    }
    while next < targets.len() {
        out.push(text.len());
        next += 1;
    }
    out
}

#[cfg(any(target_os = "macos", test))]
fn byte_at_utf16(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (byte, ch) in text.char_indices() {
        if seen >= units {
            return byte;
        }
        seen += ch.len_utf16();
        if seen >= units {
            return byte + ch.len_utf8();
        }
    }
    text.len()
}

/// How much of the document one spellcheck pass covers. The platform checker
/// only runs on the main thread, so a whole-document pass on a long note is a
/// visible hitch after the idle pause; the editor walks the document in
/// chunks of about this size, one per frame, instead.
pub const SPELL_CHUNK_BYTES: usize = 32 * 1024;

/// The end of the spellcheck chunk starting at byte `from`: just past the
/// first newline at or after `from + budget`, or the end of the text.
///
/// Chunk boundaries always fall after a newline, so a chunk never splits a
/// word (or a character) and consecutive chunks tile the document exactly. A
/// single line longer than `budget` is one chunk: there is no safe place to
/// cut it, and the platform check splits it no further either.
pub fn chunk_end(text: &str, from: usize, budget: usize) -> usize {
    if from >= text.len() {
        return text.len();
    }
    let target = from.saturating_add(budget);
    if target >= text.len() {
        return text.len();
    }
    match text.as_bytes()[target..]
        .iter()
        .position(|byte| *byte == b'\n')
    {
        Some(offset) => target + offset + 1,
        None => text.len(),
    }
}

/// Misspelled ranges that fall inside prose, for the whole document in one
/// pass. The platform returns source-byte ranges; a word that overlaps a
/// code/frontmatter block or an inline marker is dropped. Production checks
/// chunk by chunk through `prose_ranges_in`; this whole-document form is the
/// reference the chunked tests compare against.
#[cfg(all(test, target_os = "macos"))]
pub fn prose_ranges(text: &str, blocks: &[Block]) -> Vec<Range<usize>> {
    prose_ranges_in(text, blocks, 0..text.len())
}

/// Like a whole-document check, but only the words inside `range` are checked.
///
/// The range must not split a word; [`chunk_end`] boundaries guarantee that
/// for the editor's chunked pass. Checking in chunks also lifts the platform
/// checker's whole-text size cap: each chunk is well under it, so a note
/// past the cap is now checked in full, just not in one frame.
pub fn prose_ranges_in(text: &str, blocks: &[Block], range: Range<usize>) -> Vec<Range<usize>> {
    let start = pilcrow_core::boundary_at_or_before(text, range.start.min(text.len()));
    let end = pilcrow_core::boundary_at_or_before(text, range.end.min(text.len()));
    if start >= end {
        return Vec::new();
    }
    let ranges = platform::misspelled(&text[start..end])
        .into_iter()
        .map(|range| range.start + start..range.end + start)
        .collect();
    filter_prose(text, blocks, ranges)
}

fn filter_prose(text: &str, blocks: &[Block], ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    // Hits arrive in document order, so consecutive ranges usually share a
    // block. Cache that block's inline kinds instead of rebuilding them for
    // every range in the block.
    let mut cached: Option<(usize, Vec<InlineKind>)> = None;
    for range in ranges {
        let start = pilcrow_core::boundary_at_or_before(text, range.start.min(text.len()));
        let end = pilcrow_core::boundary_at_or_before(text, range.end.min(text.len()));
        if start >= end {
            continue;
        }
        let index = blocks
            .partition_point(|block| block.start <= start)
            .saturating_sub(1);
        let Some(block) = blocks.get(index) else {
            continue;
        };
        if !matches!(
            block.kind,
            BlockKind::Paragraph | BlockKind::Heading | BlockKind::Quote | BlockKind::ListItem
        ) {
            continue;
        }
        let block_end = block.end.min(text.len());
        if start < block.start || end > block_end {
            continue;
        }
        if cached
            .as_ref()
            .is_none_or(|(cached_index, _)| *cached_index != index)
        {
            let mut kinds = vec![InlineKind::Plain; block_end - block.start];
            inline_kinds(&text[block.start..block_end], &mut kinds);
            cached = Some((index, kinds));
        }
        let kinds = &cached
            .as_ref()
            .expect("inline kinds cached for this block")
            .1;
        let overlaps_marker = (start..end).any(|byte| {
            kinds
                .get(byte.saturating_sub(block.start))
                .is_some_and(|kind| *kind == InlineKind::Marker)
        });
        if !overlaps_marker {
            out.push(start..end);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilcrow_core::scan_blocks;

    fn blocks(text: &str) -> Vec<Block> {
        let mut blocks = Vec::new();
        scan_blocks(text, &mut blocks);
        blocks
    }

    #[test]
    fn filters_code_markers_and_urls_from_platform_ranges() {
        let text = "One **bold** [site](https://example.com)\n\n```\nteh code\n```";
        let all_blocks = blocks(text);
        let bold = text.find("bold").expect("bold");
        let url = text.find("example").expect("url");
        let code = text.find("teh").expect("code");
        let kept = filter_prose(
            text,
            &all_blocks,
            vec![0..3, bold..bold + 4, url..url + 7, code..code + 3],
        );
        assert_eq!(
            kept,
            vec![0..3, bold..bold + 4],
            "prose and emphasised words should survive"
        );
    }

    #[test]
    fn utf16_offsets_map_in_one_pass_like_a_scan_per_hit() {
        let text = "aé\u{1f600}b";
        let targets = [0, 1, 2, 3, 4, 5, 99];
        let expected: Vec<usize> = targets
            .iter()
            .map(|&units| byte_at_utf16(text, units))
            .collect();
        assert_eq!(byte_offsets_for_utf16(text, &targets), expected);
        assert_eq!(
            byte_offsets_for_utf16(text, &targets),
            vec![0, 1, 3, 7, 7, 8, 8]
        );
        // Out-of-order targets keep the per-target answers.
        assert_eq!(byte_offsets_for_utf16(text, &[4, 0, 2]), vec![7, 0, 3]);
        assert_eq!(byte_offsets_for_utf16(text, &[]), Vec::<usize>::new());
        assert_eq!(byte_offsets_for_utf16("", &[0, 3]), vec![0, 0]);
    }

    #[test]
    fn filters_many_ranges_in_one_block() {
        let text = "teh **bold** wrod and [x](https://example.com) mroe";
        let all_blocks = blocks(text);
        let strikes = ["teh", "bold", "wrod", "example", "mroe"]
            .iter()
            .map(|word| {
                let start = text.find(word).expect("word present");
                start..start + word.len()
            })
            .collect::<Vec<_>>();
        let kept = filter_prose(text, &all_blocks, strikes.clone());
        assert_eq!(
            kept,
            vec![
                strikes[0].clone(),
                strikes[1].clone(),
                strikes[2].clone(),
                strikes[4].clone()
            ],
            "only the URL inside the link marker is dropped"
        );
    }

    #[test]
    fn chunks_tile_the_document_at_line_boundaries() {
        let text = "one two\nthree\n\nfour five six\nseven";
        let mut chunks = Vec::new();
        let mut from = 0;
        loop {
            let end = chunk_end(text, from, 8);
            assert!(end > from, "chunk at {from} made no progress");
            assert!(
                end == text.len() || text.as_bytes()[end - 1] == b'\n',
                "chunk {from}..{end} does not end after a newline"
            );
            chunks.push(from..end);
            from = end;
            if from >= text.len() {
                break;
            }
        }
        // Every byte is covered exactly once, in order.
        let covered: String = chunks.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(covered, text);
        assert!(
            chunks.len() > 1,
            "budget should split this text: {chunks:?}"
        );
        // Past-the-end and no-newline cases end at the text length.
        assert_eq!(chunk_end(text, text.len() + 5, 8), text.len());
        assert_eq!(chunk_end("no newlines at all", 0, 4), 18);
        assert_eq!(chunk_end("", 0, 4), 0);
        // A multibyte character under the budget cut is never split: the cut
        // searches bytes for the next newline from inside the character.
        let wide = "éééé\nnext";
        let end = chunk_end(wide, 0, 3);
        assert_eq!(&wide[..end], "éééé\n");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ranged_checks_match_a_whole_document_check_chunk_by_chunk() {
        let text = "teh first\n\n```\nteh code\n```\n\nlast wrod here";
        let all_blocks = blocks(text);
        let whole = prose_ranges(text, &all_blocks);
        let mut chunked = Vec::new();
        let mut from = 0;
        while from < text.len() {
            let end = chunk_end(text, from, 8);
            chunked.extend(prose_ranges_in(text, &all_blocks, from..end));
            from = end;
        }
        assert_eq!(chunked, whole);
        // A range around only the last paragraph reports only its word.
        let last = text.find("wrod").expect("word");
        let start = text.rfind('\n').expect("line start") + 1;
        assert_eq!(
            prose_ranges_in(text, &all_blocks, start..text.len()),
            vec![last..last + 4]
        );
        assert!(prose_ranges_in(text, &all_blocks, 4..4).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_platform_smoke() {
        assert_eq!(platform::misspelled("teh word"), vec![0..3]);
        let text = "teh word\n\n```\nteh word\n```";
        assert_eq!(prose_ranges(text, &blocks(text)), vec![0..3]);
    }

    #[test]
    fn unknown_platform_returns_no_ranges_off_macos() {
        #[cfg(not(target_os = "macos"))]
        assert!(platform::misspelled("teh").is_empty());
    }

    #[test]
    fn filter_prose_rejects_ranges_starting_before_block_start_or_straddling_blocks() {
        let text = "first\nsecond";
        let all_blocks = vec![Block {
            kind: BlockKind::Paragraph,
            level: 0,
            start: 6,
            end: 12,
            marker: 0,
        }];
        let kept = filter_prose(text, &all_blocks, vec![2..8, 6..12]);
        assert_eq!(kept, vec![6..12]);
    }
}
