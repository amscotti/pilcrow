//! Undo and redo, owned by the app.
//!
//! Each entry is a diff: the bytes an edit removed, the bytes it inserted, and
//! the selection on either side. Undo applies the inverse diff and restores the
//! selection that was there before the edit, so a replaced word comes back
//! selected and the caret lands where the person was working.
//!
//! Consecutive typing in one word run groups into one step while it stays
//! inside [`GROUP_WINDOW_MS`]. A paste, cut, format command, IME commit,
//! replace-all or any other explicit gesture is always its own step. History
//! is bounded by payload bytes and by step count; the oldest step is dropped
//! when either bound is crossed.
//!
//! A selection is recorded as `(anchor, caret)`, not as a sorted range. The
//! direction matters: undo should put the selection back the way it was.

use std::collections::VecDeque;

use crate::buffer::{Buffer, boundary_at_or_before};

/// Consecutive typing within this many milliseconds can share one undo step.
pub const GROUP_WINDOW_MS: u64 = 500;

/// Default payload budget for both stacks. Only removed and inserted bytes are
/// counted; the bookkeeping fields are small and fixed.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Default number of edits kept across both stacks.
pub const DEFAULT_MAX_STEPS: usize = 10_000;

/// The gesture that produced an edit. Only [`Group::Typing`],
/// [`Group::Backspace`] and [`Group::Delete`] can absorb a following gesture;
/// every other group is a complete step on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// A character or several characters typed into the page.
    Typing,
    /// One or more backspaces.
    Backspace,
    /// One or more forward deletes.
    Delete,
    /// A paste, including a multi-paragraph paste.
    Paste,
    /// A cut, which is a deletion that also wrote the clipboard.
    Cut,
    /// A formatting command that rewrites text.
    Format,
    /// The committed result of one IME composition.
    ImeCommit,
    /// A find/replace-all transaction. Each match still gets its own edit, but
    /// the group never merges with adjacent typing.
    ReplaceAll,
    /// Enter, Tab, drop, or any other explicit edit.
    Other,
}

/// One reversible text change.
///
/// `start` is a byte offset in the document before the edit. At that offset
/// the edit removed `removed` and inserted `inserted`. Undo replaces the
/// inserted bytes with the removed ones; redo does the reverse.
///
/// `selection_before` and `selection_after` are `(anchor, caret)` pairs, and
/// are `None` for a collapsed caret. The anchor is not sorted: a selection
/// made right-to-left keeps its direction across undo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
    pub selection_before: Option<(usize, usize)>,
    pub selection_after: Option<(usize, usize)>,
    pub group: Group,
}

/// What an undo or redo left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The byte offset where the edit happened, for layout invalidation.
    pub start: usize,
    /// The byte offset in the new text just past the applied replacement:
    /// after the restored text on undo, after the reinserted text on redo.
    pub end: usize,
    /// The caret offset after the change, clamped to the document.
    pub caret: usize,
    /// The selection anchor after the change, if a non-empty selection was
    /// recorded for that side of the edit.
    pub anchor: Option<usize>,
    /// The gesture that was undone or redone.
    pub group: Group,
}

/// The two stacks, their payload accounting, and the open typing run.
pub struct History {
    undo: VecDeque<Edit>,
    redo: VecDeque<Edit>,
    max_bytes: usize,
    max_steps: usize,
    /// Payload bytes across both stacks.
    bytes: usize,
    /// Timestamp of the last recorded step, while a run may still accept more.
    last_time: Option<u64>,
    group_window_ms: u64,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// A history with the default byte and step bounds.
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_BYTES, DEFAULT_MAX_STEPS)
    }

    /// A history with explicit bounds, for tests and for unusually small
    /// documents.
    pub fn with_limits(max_bytes: usize, max_steps: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            max_bytes,
            max_steps,
            bytes: 0,
            last_time: None,
            group_window_ms: GROUP_WINDOW_MS,
        }
    }

    /// Drop every step. Opening another note calls this.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
        self.last_time = None;
    }

    /// End the open typing run. Movement, a click, a selection change or a
    /// composition boundary calls this so the next keystroke starts a new
    /// step even when it is contiguous and within the time window.
    pub fn break_group(&mut self) {
        self.last_time = None;
    }

    /// Records an edit that has already been applied to `buffer`.
    ///
    /// `now_ms` is a monotonic millisecond timestamp. Callers own the clock so
    /// grouping is deterministic in tests. No-op edits are ignored and do not
    /// clear redo.
    pub fn record(&mut self, edit: Edit, now_ms: u64) {
        if edit.removed == edit.inserted {
            return;
        }

        self.clear_redo();

        if self.merge_into_last(&edit, now_ms) {
            self.last_time = Some(now_ms);
            self.trim();
            return;
        }

        self.bytes = self.bytes.saturating_add(edit_payload(&edit));
        self.undo.push_back(edit);
        self.last_time = Some(now_ms);
        self.trim();
    }

    /// Update the placement recorded for the newest undo step. Handlers that
    /// choose a selection after applying an edit (formatting, task toggles)
    /// call this so redo lands in the same place as the first application.
    pub fn set_last_selection_after(&mut self, selection: Option<(usize, usize)>) {
        if let Some(edit) = self.undo.back_mut() {
            edit.selection_after = selection;
        }
    }

    /// Whether the last undo step can absorb `next`.
    fn merge_into_last(&mut self, next: &Edit, now_ms: u64) -> bool {
        let Some(last_time) = self.last_time else {
            return false;
        };
        if now_ms < last_time || now_ms - last_time > self.group_window_ms {
            return false;
        }
        // A new gesture that replaced a selection starts a fresh step. Only a
        // pure insertion may extend a typing run, and only a pure deletion may
        // extend a backspace or forward-delete run.
        if next.selection_before.is_some() {
            return false;
        }

        let Some(last) = self.undo.back_mut() else {
            return false;
        };
        if last.group != next.group {
            return false;
        }

        match next.group {
            Group::Typing => {
                if !next.removed.is_empty()
                    || next.inserted.is_empty()
                    || last.inserted.is_empty()
                    || next.start != last.start + last.inserted.len()
                    || !is_word_run(&last.inserted)
                    || !is_word_run(&next.inserted)
                {
                    return false;
                }
                last.inserted.push_str(&next.inserted);
                last.selection_after = next.selection_after;
                self.bytes = self.bytes.saturating_add(next.inserted.len());
                true
            }
            Group::Backspace => {
                if !next.inserted.is_empty()
                    || next.removed.is_empty()
                    || last.removed.is_empty()
                    || next.start + next.removed.len() != last.start
                    || !is_word_run(&next.removed)
                    || !is_word_run(&last.removed)
                {
                    return false;
                }
                let mut removed = next.removed.clone();
                removed.push_str(&last.removed);
                last.removed = removed;
                last.start = next.start;
                last.selection_after = next.selection_after;
                self.bytes = self.bytes.saturating_add(next.removed.len());
                true
            }
            Group::Delete => {
                if !next.inserted.is_empty()
                    || next.removed.is_empty()
                    || last.removed.is_empty()
                    || next.start != last.start
                    || !is_word_run(&next.removed)
                    || !is_word_run(&last.removed)
                {
                    return false;
                }
                last.removed.push_str(&next.removed);
                last.selection_after = next.selection_after;
                self.bytes = self.bytes.saturating_add(next.removed.len());
                true
            }
            _ => false,
        }
    }

    /// Applies the inverse of the newest undo step and moves it to redo.
    ///
    /// The caret and anchor in the returned [`Applied`] are the selection that
    /// existed before the edit. For an insertion run that means the run's
    /// start; for a deletion it means just past the restored text.
    pub fn undo(&mut self, buffer: &mut Buffer) -> Option<Applied> {
        let edit = self.undo.pop_back()?;
        let start = edit.start.min(buffer.len());
        let end = start.saturating_add(edit.inserted.len()).min(buffer.len());
        buffer.replace(start..end, &edit.removed);
        let applied = applied_for(&edit, buffer, Side::Before);
        self.redo.push_back(edit);
        self.last_time = None;
        Some(applied)
    }

    /// Applies the newest redo step again and moves it back to undo.
    pub fn redo(&mut self, buffer: &mut Buffer) -> Option<Applied> {
        let edit = self.redo.pop_back()?;
        let start = edit.start.min(buffer.len());
        let end = start.saturating_add(edit.removed.len()).min(buffer.len());
        buffer.replace(start..end, &edit.inserted);
        let applied = applied_for(&edit, buffer, Side::After);
        self.undo.push_back(edit);
        self.last_time = None;
        Some(applied)
    }

    /// Whether there is a step to undo.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether there is a step to redo.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Number of undo steps currently held.
    pub fn undo_steps(&self) -> usize {
        self.undo.len()
    }

    /// Number of redo steps currently held.
    pub fn redo_steps(&self) -> usize {
        self.redo.len()
    }

    /// Payload bytes held across both stacks.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn clear_redo(&mut self) {
        for edit in self.redo.drain(..) {
            self.bytes = self.bytes.saturating_sub(edit_payload(&edit));
        }
    }

    /// Drops the oldest steps until both bounds are satisfied. A single edit
    /// larger than the byte budget is dropped too: the bound is a promise, not
    /// a suggestion.
    fn trim(&mut self) {
        while self.undo.len() + self.redo.len() > self.max_steps || self.bytes > self.max_bytes {
            let dropped = if let Some(edit) = self.undo.pop_front() {
                edit
            } else if let Some(edit) = self.redo.pop_front() {
                edit
            } else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(edit_payload(&dropped));
        }
        if self.undo.is_empty() {
            self.last_time = None;
        }
    }
}

#[derive(Clone, Copy)]
enum Side {
    Before,
    After,
}

fn applied_for(edit: &Edit, buffer: &Buffer, side: Side) -> Applied {
    let (recorded, fallback) = match side {
        Side::Before => (
            edit.selection_before,
            // A forward delete removed text just after the caret. Undo must put
            // the caret back where the delete began, not after the restored
            // run. Backspace and cut restore to just past the text.
            if edit.removed.is_empty() || edit.group == Group::Delete {
                edit.start
            } else {
                edit.start + edit.removed.len()
            },
        ),
        Side::After => (edit.selection_after, edit.start + edit.inserted.len()),
    };
    let (anchor, caret) = match recorded {
        Some((anchor, caret)) => (Some(clamp(buffer, anchor)), clamp(buffer, caret)),
        None => (None, clamp(buffer, fallback)),
    };
    let inserted_len = match side {
        Side::Before => edit.removed.len(),
        Side::After => edit.inserted.len(),
    };
    Applied {
        start: clamp(buffer, edit.start),
        end: clamp(buffer, edit.start.saturating_add(inserted_len)),
        caret,
        anchor,
        group: edit.group,
    }
}

fn clamp(buffer: &Buffer, offset: usize) -> usize {
    boundary_at_or_before(buffer.text(), offset)
}

fn edit_payload(edit: &Edit) -> usize {
    edit.removed.len().saturating_add(edit.inserted.len())
}

/// A word run is a contiguous run of letters, digits, underscores or
/// apostrophes. Grouping stops at whitespace, punctuation and newlines, which
/// also means it cannot cross a block boundary.
fn is_word_run(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '\'' | '\u{2019}'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;

    fn apply(
        history: &mut History,
        buffer: &mut Buffer,
        range: std::ops::Range<usize>,
        text: &str,
        group: Group,
        now_ms: u64,
        selection_before: Option<(usize, usize)>,
    ) -> usize {
        let source = buffer.text();
        let mut start = boundary_at_or_before(source, range.start);
        let mut end = boundary_at_or_before(source, range.end);
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        let removed = source[start..end].to_owned();
        let caret = buffer.replace(start..end, text);
        history.record(
            Edit {
                start,
                removed,
                inserted: text.to_owned(),
                selection_before,
                selection_after: None,
                group,
            },
            now_ms,
        );
        caret
    }

    fn type_chars(history: &mut History, buffer: &mut Buffer, text: &str, now_ms: u64) {
        for (index, ch) in text.chars().enumerate() {
            let at = buffer.len();
            let mut encoded = [0u8; 4];
            let piece = ch.encode_utf8(&mut encoded);
            apply(
                history,
                buffer,
                at..at,
                piece,
                Group::Typing,
                now_ms + index as u64 * 100,
                None,
            );
        }
    }

    #[test]
    fn typing_run_undoes_and_redoes_as_one_step() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");
        type_chars(&mut history, &mut buffer, "hello", 0);

        assert_eq!(history.undo_steps(), 1);
        let undone = history.undo(&mut buffer).expect("undo");
        assert_eq!(buffer.text(), "");
        assert_eq!(undone.caret, 0);
        assert_eq!(undone.anchor, None);

        let redone = history.redo(&mut buffer).expect("redo");
        assert_eq!(buffer.text(), "hello");
        assert_eq!(redone.caret, 5);
        assert!(!history.can_redo());
    }

    #[test]
    fn typing_groups_stop_at_word_boundaries() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");
        type_chars(&mut history, &mut buffer, "ab c", 0);

        assert_eq!(history.undo_steps(), 3);
        assert_eq!(buffer.text(), "ab c");

        history.undo(&mut buffer).expect("undo c");
        assert_eq!(buffer.text(), "ab ");
        history.undo(&mut buffer).expect("undo space");
        assert_eq!(buffer.text(), "ab");
        history.undo(&mut buffer).expect("undo ab");
        assert_eq!(buffer.text(), "");
        assert!(!history.can_undo());
    }

    #[test]
    fn typing_groups_stop_after_the_pause() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");

        apply(&mut history, &mut buffer, 0..0, "h", Group::Typing, 0, None);
        apply(
            &mut history,
            &mut buffer,
            1..1,
            "i",
            Group::Typing,
            600,
            None,
        );

        assert_eq!(history.undo_steps(), 2);
        history.undo(&mut buffer).expect("undo i");
        assert_eq!(buffer.text(), "h");
        history.undo(&mut buffer).expect("undo h");
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn movement_breaks_a_typing_run() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");

        apply(&mut history, &mut buffer, 0..0, "h", Group::Typing, 0, None);
        history.break_group();
        apply(
            &mut history,
            &mut buffer,
            1..1,
            "i",
            Group::Typing,
            100,
            None,
        );

        assert_eq!(history.undo_steps(), 2);
    }

    #[test]
    fn non_contiguous_typing_does_not_merge() {
        let mut history = History::new();
        let mut buffer = Buffer::new("hello");

        apply(&mut history, &mut buffer, 0..0, "x", Group::Typing, 0, None);
        apply(
            &mut history,
            &mut buffer,
            5..5,
            "y",
            Group::Typing,
            100,
            None,
        );
        assert_eq!(history.undo_steps(), 2);
    }

    #[test]
    fn paste_is_one_step_even_across_paragraphs() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");

        apply(
            &mut history,
            &mut buffer,
            0..0,
            "one\n\ntwo\nthree",
            Group::Paste,
            0,
            None,
        );
        assert_eq!(buffer.text(), "one\n\ntwo\nthree");
        assert_eq!(history.undo_steps(), 1);

        history.undo(&mut buffer).expect("undo paste");
        assert_eq!(buffer.text(), "");
        history.redo(&mut buffer).expect("redo paste");
        assert_eq!(buffer.text(), "one\n\ntwo\nthree");
    }

    #[test]
    fn typing_and_paste_are_separate_steps() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");

        type_chars(&mut history, &mut buffer, "x", 0);
        apply(
            &mut history,
            &mut buffer,
            1..1,
            "two\nlines",
            Group::Paste,
            100,
            None,
        );

        history.undo(&mut buffer).expect("undo paste");
        assert_eq!(buffer.text(), "x");
        history.undo(&mut buffer).expect("undo typing");
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn cut_undo_restores_text_and_selection() {
        let mut history = History::new();
        let mut buffer = Buffer::new("hello world");

        apply(
            &mut history,
            &mut buffer,
            6..11,
            "",
            Group::Cut,
            0,
            Some((6, 11)),
        );
        assert_eq!(buffer.text(), "hello ");

        let undone = history.undo(&mut buffer).expect("undo cut");
        assert_eq!(buffer.text(), "hello world");
        assert_eq!(undone.anchor, Some(6));
        assert_eq!(undone.caret, 11);

        let redone = history.redo(&mut buffer).expect("redo cut");
        assert_eq!(buffer.text(), "hello ");
        assert_eq!(redone.caret, 6);
    }

    #[test]
    fn selection_replacement_restores_direction() {
        let mut history = History::new();
        let mut buffer = Buffer::new("abcdef");

        // Select "cd" right-to-left: anchor at 4, caret at 2.
        apply(
            &mut history,
            &mut buffer,
            2..4,
            "X",
            Group::Typing,
            0,
            Some((4, 2)),
        );
        apply(
            &mut history,
            &mut buffer,
            3..3,
            "y",
            Group::Typing,
            100,
            None,
        );
        assert_eq!(buffer.text(), "abXyef");

        let undone = history.undo(&mut buffer).expect("undo replacement");
        assert_eq!(buffer.text(), "abcdef");
        assert_eq!(undone.anchor, Some(4));
        assert_eq!(undone.caret, 2);
    }

    #[test]
    fn backspace_run_restores_the_caret_past_the_text() {
        let mut history = History::new();
        let mut buffer = Buffer::new("hello");

        apply(
            &mut history,
            &mut buffer,
            4..5,
            "",
            Group::Backspace,
            0,
            None,
        );
        assert_eq!(buffer.text(), "hell");

        let undone = history.undo(&mut buffer).expect("undo backspace");
        assert_eq!(buffer.text(), "hello");
        assert_eq!(undone.caret, 5);

        let redone = history.redo(&mut buffer).expect("redo backspace");
        assert_eq!(buffer.text(), "hell");
        assert_eq!(redone.caret, 4);
    }

    #[test]
    fn forward_delete_undo_returns_to_the_delete_start() {
        let mut history = History::new();
        let mut buffer = Buffer::new("hello");

        apply(&mut history, &mut buffer, 0..1, "", Group::Delete, 0, None);
        assert_eq!(buffer.text(), "ello");

        let undone = history.undo(&mut buffer).expect("undo delete");
        assert_eq!(buffer.text(), "hello");
        assert_eq!(
            undone.caret, 0,
            "undo put the caret after the restored text"
        );

        let redone = history.redo(&mut buffer).expect("redo delete");
        assert_eq!(buffer.text(), "ello");
        assert_eq!(redone.caret, 0);
    }

    #[test]
    fn ime_commit_is_one_step_and_never_merges() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");

        apply(
            &mut history,
            &mut buffer,
            0..0,
            "に",
            Group::ImeCommit,
            0,
            None,
        );
        apply(
            &mut history,
            &mut buffer,
            3..3,
            "ほ",
            Group::ImeCommit,
            100,
            None,
        );
        assert_eq!(buffer.text(), "にほ");
        assert_eq!(history.undo_steps(), 2);

        history.undo(&mut buffer).expect("undo second commit");
        assert_eq!(buffer.text(), "に");
        history.undo(&mut buffer).expect("undo first commit");
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn ime_commit_over_a_selection_restores_it() {
        let mut history = History::new();
        let mut buffer = Buffer::new("hello world");

        apply(
            &mut history,
            &mut buffer,
            6..11,
            "there",
            Group::ImeCommit,
            0,
            Some((6, 11)),
        );
        assert_eq!(buffer.text(), "hello there");

        let undone = history.undo(&mut buffer).expect("undo commit");
        assert_eq!(buffer.text(), "hello world");
        assert_eq!(undone.anchor, Some(6));
        assert_eq!(undone.caret, 11);
    }

    #[test]
    fn interleaved_undo_and_redo_keep_the_stacks_consistent() {
        let mut history = History::new();
        let mut buffer = Buffer::new("one");

        apply(
            &mut history,
            &mut buffer,
            3..3,
            "two",
            Group::Paste,
            0,
            None,
        );
        apply(
            &mut history,
            &mut buffer,
            6..6,
            "three",
            Group::Paste,
            100,
            None,
        );
        assert_eq!(buffer.text(), "onetwothree");

        history.undo(&mut buffer).expect("undo three");
        assert_eq!(buffer.text(), "onetwo");
        history.redo(&mut buffer).expect("redo three");
        assert_eq!(buffer.text(), "onetwothree");
        history.undo(&mut buffer).expect("undo three again");
        history.undo(&mut buffer).expect("undo two");
        assert_eq!(buffer.text(), "one");

        history.redo(&mut buffer).expect("redo two");
        assert_eq!(buffer.text(), "onetwo");
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");

        apply(&mut history, &mut buffer, 0..0, "a", Group::Paste, 0, None);
        history.undo(&mut buffer).expect("undo");
        assert!(history.can_redo());

        apply(
            &mut history,
            &mut buffer,
            0..0,
            "b",
            Group::Paste,
            100,
            None,
        );
        assert!(!history.can_redo());
        assert_eq!(buffer.text(), "b");
    }

    #[test]
    fn no_op_edits_do_not_record_or_clear_redo() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");

        apply(&mut history, &mut buffer, 0..0, "a", Group::Paste, 0, None);
        history.undo(&mut buffer).expect("undo");
        assert!(history.can_redo());

        history.record(
            Edit {
                start: 0,
                removed: "same".into(),
                inserted: "same".into(),
                selection_before: None,
                selection_after: None,
                group: Group::Other,
            },
            100,
        );
        assert!(history.can_redo());
        assert!(!history.can_undo());

        history.record(
            Edit {
                start: 0,
                removed: "".into(),
                inserted: "".into(),
                selection_before: None,
                selection_after: None,
                group: Group::Paste,
            },
            100,
        );
        assert!(history.can_redo());
    }

    #[test]
    fn empty_history_does_not_move() {
        let mut history = History::new();
        let mut buffer = Buffer::new("unchanged");
        assert!(history.undo(&mut buffer).is_none());
        assert!(history.redo(&mut buffer).is_none());
        assert_eq!(buffer.text(), "unchanged");
    }

    #[test]
    fn explicit_groups_never_merge() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");

        apply(&mut history, &mut buffer, 0..0, "a", Group::Format, 0, None);
        apply(
            &mut history,
            &mut buffer,
            1..1,
            "b",
            Group::Format,
            100,
            None,
        );
        apply(
            &mut history,
            &mut buffer,
            2..2,
            "c",
            Group::ReplaceAll,
            200,
            None,
        );
        assert_eq!(history.undo_steps(), 3);
    }

    #[test]
    fn a_single_edit_larger_than_the_byte_cap_is_dropped() {
        let mut history = History::with_limits(2, 10);
        let mut buffer = Buffer::new("");

        apply(
            &mut history,
            &mut buffer,
            0..0,
            "too big",
            Group::Paste,
            0,
            None,
        );
        assert_eq!(buffer.text(), "too big");
        assert!(!history.can_undo());
        assert_eq!(history.bytes(), 0);
    }

    #[test]
    fn byte_cap_drops_the_oldest_steps() {
        let mut history = History::with_limits(5, 100);
        let mut buffer = Buffer::new("");

        for index in 0..4 {
            let at = buffer.len();
            apply(
                &mut history,
                &mut buffer,
                at..at,
                "aaa",
                Group::Paste,
                index,
                None,
            );
        }
        assert_eq!(buffer.text(), "aaaaaaaaaaaa");
        assert!(history.bytes() <= 5);
        assert_eq!(history.bytes(), 3);
        assert_eq!(history.undo_steps(), 1);

        history.undo(&mut buffer).expect("undo newest");
        assert_eq!(buffer.text(), "aaaaaaaaa");
        assert!(!history.can_undo());
    }

    #[test]
    fn step_cap_drops_the_oldest_steps() {
        let mut history = History::with_limits(DEFAULT_MAX_BYTES, 2);
        let mut buffer = Buffer::new("");

        for (index, text) in ["a", "b", "c", "d"].iter().enumerate() {
            let at = buffer.len();
            apply(
                &mut history,
                &mut buffer,
                at..at,
                text,
                Group::Paste,
                index as u64,
                None,
            );
        }
        assert_eq!(buffer.text(), "abcd");
        assert_eq!(history.undo_steps(), 2);

        history.undo(&mut buffer).expect("undo d");
        history.undo(&mut buffer).expect("undo c");
        assert_eq!(buffer.text(), "ab");
        assert!(!history.can_undo());
    }

    #[test]
    fn clear_forgets_both_stacks() {
        let mut history = History::new();
        let mut buffer = Buffer::new("");

        apply(&mut history, &mut buffer, 0..0, "a", Group::Paste, 0, None);
        history.undo(&mut buffer).expect("undo");
        assert!(history.can_redo());

        history.clear();
        assert!(!history.can_undo());
        assert!(!history.can_redo());
        assert_eq!(history.bytes(), 0);
    }
}
