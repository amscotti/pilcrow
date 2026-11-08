//! AccessKit publishing for the writing page.
//!
//! The page wrapper owns the text-input role; the custom paint element does
//! not publish a second node. These helpers keep character/UTF-8 conversion in
//! one place for the tree and for incoming actions.

use gpui::{A11ySubtreeBuilder, Role, accesskit};

/// Publish the document as one AccessKit text run and select the caret and
/// anchor on it.
pub(crate) fn publish_text_run(
    builder: &mut A11ySubtreeBuilder,
    text: &str,
    caret: usize,
    anchor: Option<usize>,
) {
    let (lengths, focus_index, anchor_index) = text_metrics(text, caret, anchor);
    let mut run = accesskit::Node::new(Role::TextRun);
    run.set_value(text.to_owned());
    run.set_character_lengths(lengths);
    let run_id = builder.synthetic_node_id(0);
    builder.push_child(run_id, run);
    let caret = accesskit::TextPosition {
        node: run_id,
        character_index: focus_index,
    };
    let anchor = accesskit::TextPosition {
        node: run_id,
        character_index: anchor_index,
    };
    builder
        .parent_node()
        .set_text_selection(accesskit::TextSelection {
            anchor,
            focus: caret,
        });
    builder.parent_node().set_value(text.to_owned());
}

/// Convert an AccessKit character index to a UTF-8 byte offset.
pub(crate) fn byte_for_character(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map_or(text.len(), |(byte, _)| byte)
}

/// Character lengths and selection positions for an AccessKit text run.
pub(crate) fn text_metrics(
    text: &str,
    caret: usize,
    anchor: Option<usize>,
) -> (Vec<u8>, usize, usize) {
    let caret = pilcrow_core::boundary_at_or_before(text, caret);
    let anchor = pilcrow_core::boundary_at_or_before(text, anchor.unwrap_or(caret));
    (
        text.chars().map(|ch| ch.len_utf8() as u8).collect(),
        text[..caret].chars().count(),
        text[..anchor].chars().count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_use_character_lengths_and_selection_positions() {
        let text = "aに😀";
        let (lengths, focus, anchor) = text_metrics(text, 4, Some(1));
        assert_eq!(lengths, vec![1, 3, 4]);
        assert_eq!(focus, 2);
        assert_eq!(anchor, 1);

        let (_, focus, anchor) = text_metrics(text, 2, Some(5));
        assert_eq!(focus, 1);
        assert_eq!(anchor, 2);

        let (_, focus, anchor) = text_metrics(text, text.len() + 99, None);
        assert_eq!(focus, 3);
        assert_eq!(anchor, 3);
        assert_eq!(byte_for_character(text, 2), 4);
        assert_eq!(byte_for_character(text, 99), text.len());
    }
}
