//! Document counts for the quiet footer.
//!
//! Words are prose words, not Markdown tokens: `# Title` is one word, list and
//! quote markers are syntax, task boxes are not words, and frontmatter/rules
//! are not prose. Inline markers stay attached to their word (`**bold**` is one
//! token), which is what a writer means by a word.

use crate::blocks::{Block, Kind, task_box_len};

/// Words per minute used for the reading-time estimate.
pub const WORDS_PER_MINUTE: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub words: usize,
    pub chars: usize,
    /// Rounded up to whole seconds, so a non-empty note never says zero.
    pub reading_seconds: usize,
}

impl Stats {
    pub fn reading_label(self) -> String {
        if self.words == 0 {
            return "0 min read".to_owned();
        }
        let minutes = self.reading_seconds.div_ceil(60);
        if minutes <= 1 {
            "< 1 min read".to_owned()
        } else {
            format!("{minutes} min read")
        }
    }

    pub fn footer_label(self) -> String {
        let words = if self.words == 1 {
            "1 word".to_owned()
        } else {
            format!("{} words", self.words)
        };
        let chars = if self.chars == 1 {
            "1 character".to_owned()
        } else {
            format!("{} characters", self.chars)
        };
        format!("{words} · {chars} · {}", self.reading_label())
    }
}

/// Counts for a selected slice. Selection can start and end inside Markdown
/// syntax, so this uses the plain prose rule rather than block structure.
pub fn compute_range(text: &str, range: std::ops::Range<usize>) -> Stats {
    let start = crate::boundary_at_or_before(text, range.start.min(text.len()));
    let end = crate::boundary_at_or_before(text, range.end.min(text.len()));
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let slice = &text[start..end];
    let words = count_plain_words(slice);
    Stats {
        words,
        chars: slice.chars().count(),
        reading_seconds: words.saturating_mul(60).div_ceil(WORDS_PER_MINUTE),
    }
}

pub fn compute(text: &str, blocks: &[Block]) -> Stats {
    let words = count_words(text, blocks);
    Stats {
        words,
        chars: text.chars().count(),
        reading_seconds: words.saturating_mul(60).div_ceil(WORDS_PER_MINUTE),
    }
}

/// The prose counter: markers are syntax, punctuation-only tokens are
/// not words, and a code fence line is not content.
pub fn count_words(text: &str, blocks: &[Block]) -> usize {
    let mut words = 0usize;
    for block in blocks {
        if matches!(block.kind, Kind::Blank | Kind::Rule | Kind::Frontmatter) {
            continue;
        }
        if block.kind == Kind::Code && block.marker > 0 {
            continue;
        }
        let end = block.end.min(text.len());
        let mut i = block.start.min(end);
        let mut first_line = true;
        let mut at_line_start = true;
        let mut in_word = false;
        let mut has_text = false;
        while i < end {
            if at_line_start {
                if first_line && block.kind == Kind::ListItem && block.marker > 0 {
                    let mut skip = block.marker;
                    if i + skip <= end
                        && let Some(box_len) = task_box_len(&text[i + skip..end])
                    {
                        skip += box_len;
                    }
                    i += skip;
                    first_line = false;
                    continue;
                }
                if first_line && block.kind == Kind::Heading && block.marker > 0 {
                    i += block.marker;
                    first_line = false;
                    continue;
                }
                if block.kind == Kind::Quote && text.as_bytes()[i] == b'>' {
                    i += 1;
                    if i < end && text.as_bytes()[i] == b' ' {
                        i += 1;
                    }
                    continue;
                }
                if text.as_bytes()[i] == b' ' || text.as_bytes()[i] == b'\t' {
                    i += 1;
                    continue;
                }
                at_line_start = false;
            }
            let Some(ch) = text[i..end].chars().next() else {
                break;
            };
            i += ch.len_utf8();
            if ch == '\n' {
                if in_word && has_text {
                    words += 1;
                }
                in_word = false;
                has_text = false;
                at_line_start = true;
                first_line = false;
            } else if ch.is_whitespace() {
                if in_word && has_text {
                    words += 1;
                }
                in_word = false;
                has_text = false;
            } else if is_cjk(ch) {
                // CJK runs have no spaces. Count each character as a word so
                // an 18-character Japanese note is not "one word".
                if in_word && has_text {
                    words += 1;
                }
                in_word = false;
                has_text = false;
                words += 1;
            } else {
                in_word = true;
                if ch.is_alphanumeric() {
                    has_text = true;
                }
            }
        }
        if in_word && has_text {
            words += 1;
        }
    }
    words
}

fn count_plain_words(slice: &str) -> usize {
    let mut words = 0usize;
    let mut in_word = false;
    let mut has_text = false;
    for ch in slice.chars() {
        if is_cjk(ch) {
            if in_word && has_text {
                words += 1;
            }
            in_word = false;
            has_text = false;
            words += 1;
        } else if ch.is_whitespace() {
            if in_word && has_text {
                words += 1;
            }
            in_word = false;
            has_text = false;
        } else {
            in_word = true;
            if ch.is_alphanumeric() {
                has_text = true;
            }
        }
    }
    if in_word && has_text {
        words += 1;
    }
    words
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30ff
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xf900..=0xfaff
            | 0x1100..=0x11ff
            | 0xac00..=0xd7af
            | 0x20000..=0x2fa1f
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::scan;

    fn stats(text: &str) -> Stats {
        let mut blocks = Vec::new();
        scan(text, &mut blocks);
        compute(text, &blocks)
    }

    #[test]
    fn prose_words_ignore_markdown_syntax() {
        assert_eq!(stats("# Title\n\none two three").words, 4);
        assert_eq!(stats("- one\n- two").words, 2);
        assert_eq!(stats("- [ ] buy milk").words, 2);
        assert_eq!(stats("- [x] done").words, 1);
        assert_eq!(stats("> quoted words").words, 2);
    }

    #[test]
    fn frontmatter_rules_and_fences_are_not_prose() {
        assert_eq!(stats("---\ntitle: Private\n---\n\n# Real\n\nbody").words, 2);
        assert_eq!(stats("---\n\nbody").words, 1);
        assert_eq!(stats("```\ncode here\n```").words, 2);
    }

    #[test]
    fn punctuation_alone_is_not_a_word() {
        assert_eq!(stats("*** ... ---").words, 0);
        assert_eq!(stats("— … “” 。！？").words, 0);
        assert_eq!(stats("**bold** and `code`").words, 3);
    }

    #[test]
    fn empty_and_whitespace_documents_have_no_words() {
        assert_eq!(stats("").words, 0);
        assert_eq!(stats("  \n\t\n").words, 0);
    }

    #[test]
    fn cjk_characters_count_as_words_in_document_and_selection() {
        let text = "日本語です";
        assert_eq!(stats(text).words, 5);
        assert_eq!(compute_range(text, 0..text.len()).words, 5);
        assert_eq!(stats("日本語 and English").words, 5);
    }

    #[test]
    fn footer_shows_characters_next_to_words() {
        let label = stats("one two").footer_label();
        assert!(label.contains("2 words"));
        assert!(label.contains("7 characters"));
    }

    #[test]
    fn characters_are_unicode_scalars_and_reading_time_rounds_up() {
        let s = stats("one two three");
        assert_eq!(s.chars, 13);
        assert_eq!(s.reading_seconds, 1);
        assert_eq!(s.reading_label(), "< 1 min read");
        let s = stats(&"word ".repeat(201));
        assert_eq!(s.words, 201);
        assert_eq!(s.reading_label(), "2 min read");
    }

    #[test]
    fn selection_stats_count_words_and_characters_in_a_range() {
        let text = "one two three";
        let selected = compute_range(text, 4..7);
        assert_eq!(selected.words, 1);
        assert_eq!(selected.chars, 3);
        assert_eq!(compute_range(text, 0..0).words, 0);
        assert_eq!(compute_range(text, 100..200).words, 0);
    }

    #[test]
    fn a_task_box_at_eof_does_not_read_past_the_block() {
        assert_eq!(stats("- [ ]").words, 0);
        assert_eq!(stats("- [x]").words, 0);
        assert_eq!(stats("- [x]\tword").words, 1);
    }
}
