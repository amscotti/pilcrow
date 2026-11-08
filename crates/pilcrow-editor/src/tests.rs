use super::*;
use gpui::{TestAppContext, VisualTestContext};

fn host_chord(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
}

fn undo_chord() -> &'static str {
    host_chord("cmd-z", "ctrl-z")
}

fn redo_chord() -> &'static str {
    host_chord("cmd-shift-z", "ctrl-shift-z")
}

fn select_all_chord() -> &'static str {
    host_chord("cmd-a", "ctrl-a")
}

fn cut_chord() -> &'static str {
    host_chord("cmd-x", "ctrl-x")
}

fn paste_chord() -> &'static str {
    host_chord("cmd-v", "ctrl-v")
}

fn doc_end_chord() -> &'static str {
    host_chord("cmd-down", "ctrl-end")
}

fn editor_window(cx: &mut TestAppContext, text: &str) -> (VisualTestContext, Entity<EditorView>) {
    cx.update(install_fonts);
    cx.update(install_keybindings);
    let window = cx
        .update(|cx| {
            cx.open_window(Default::default(), |_window, cx| {
                cx.new(|cx| EditorView::new_light(cx, text))
            })
        })
        .expect("open editor window");
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let editor = window.root(&mut cx).expect("editor root");
    let focus_handle = editor.read_with(&cx, |editor, _| editor.focus_handle.clone());
    cx.update(|window, cx| focus_handle.focus(window, cx));
    (cx, editor)
}

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
}

fn block_lines(editor: &EditorView, block: usize) -> Vec<&LineGeometry> {
    editor
        .last_lines
        .iter()
        .filter(|line| line.block == block && !line.rule)
        .collect()
}

#[gpui::test]
fn layout_is_not_done_before_the_first_paint(cx: &mut TestAppContext) {
    let editor = cx.new(|cx| EditorView::new_light(cx, "hello"));
    assert!(!editor.read_with(cx, |editor, _| editor.layout_done()));
}

#[gpui::test]
fn types_text_and_places_the_caret(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.simulate_input("hello");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello"
    );
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 5);
}

#[gpui::test]
fn backspace_and_arrows_edit_at_the_caret(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    cx.simulate_keystrokes("end backspace");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hell"
    );
    cx.simulate_keystrokes("left left");
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 2);
    cx.simulate_input("X");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "heXll"
    );
}

#[gpui::test]
fn shift_arrows_select_and_replacement_replaces(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    cx.simulate_keystrokes("end shift-left shift-left");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(3..5)
    );
    cx.simulate_input("X");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "helX"
    );
}

#[gpui::test]
fn enter_inserts_a_newline_through_the_keymap(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "\n"
    );
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 1);
}

#[gpui::test]
fn a_collapsed_shift_selection_does_not_survive_plain_movement(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    cx.simulate_keystrokes("home shift-left");
    assert_eq!(editor.read_with(&cx, |editor, _| editor.selection()), None);
    cx.simulate_keystrokes("right");
    assert_eq!(editor.read_with(&cx, |editor, _| editor.selection()), None);
}

#[gpui::test]
fn external_utf16_ranges_are_exact_and_grapheme_safe(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "abcdef");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(2..2, window, cx);
        });
    });
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 2);

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(0..1, window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..1)
    );

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(Some(0..1), "X", window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "Xbcdef"
    );
}

#[gpui::test]
fn external_utf16_caret_snaps_out_of_a_grapheme(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "e\u{301}x");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(1..1, window, cx);
        });
    });
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 0);
}

#[gpui::test]
fn movement_and_deletion_respect_grapheme_clusters(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "e\u{301}x");
    cx.simulate_keystrokes("end left left");
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 0);
    cx.simulate_keystrokes("end left");
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 3);
    cx.simulate_keystrokes("backspace");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "x"
    );
}

#[gpui::test]
fn vertical_movement_does_not_teleport_while_layout_catches_up(cx: &mut TestAppContext) {
    let text = (0..300)
        .map(|index| format!("paragraph {index}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let (mut cx, editor) = editor_window(cx, &text);
    cx.simulate_keystrokes(doc_end_chord());
    let end = editor.read_with(&cx, |editor, _| editor.caret());
    assert!(end > 0);
    cx.simulate_keystrokes("up");
    let after_up = editor.read_with(&cx, |editor, _| editor.caret());
    assert!(after_up > 0, "Up teleported to the document start");
}

#[gpui::test]
fn vertical_movement_keeps_the_visual_column(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "WWWWW\nii");
    cx.simulate_keystrokes("end down up");
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 5);
}

#[gpui::test]
fn vertical_movement_skips_blank_lines_and_rules(cx: &mut TestAppContext) {
    let text = "one\n\n---\n\nthree";
    let (mut cx, editor) = editor_window(cx, text);
    cx.simulate_keystrokes("home down");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        text.find("three").expect("third paragraph"),
        "vertical movement stopped on a blank line or a rule"
    );
    cx.simulate_keystrokes("up");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        0,
        "up did not skip the blank lines and rule"
    );
}

#[gpui::test]
fn linux_keymap_selects_all(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    cx.update(|_, cx| install_keybindings_for(cx, Platform::Other));
    cx.simulate_keystrokes("ctrl-a");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..5)
    );
}

#[gpui::test]
fn click_places_the_caret_from_the_shaped_line(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    let (left, top) = editor.read_with(&cx, |editor, _| {
        let line = &editor.last_lines[0];
        (line.bounds.left(), line.bounds.top())
    });
    cx.simulate_click(
        point(left + px(20.0), top + px(5.0)),
        gpui::Modifiers::default(),
    );
    let caret = editor.read_with(&cx, |editor, _| editor.caret());
    assert!(caret > 0);
    assert!(caret < 5);
}

#[gpui::test]
fn select_all_reveals_the_caret_in_a_long_note(cx: &mut TestAppContext) {
    let text = (0..300)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (mut cx, editor) = editor_window(cx, &text);
    cx.simulate_keystrokes(select_all_chord());
    assert!(
        editor
            .read_with(&cx, |editor, _| editor.selection())
            .is_some()
    );
    assert!(
        editor
            .read_with(&cx, |editor, _| editor.caret_rect())
            .is_some(),
        "select-all left the caret outside the viewport"
    );
}

#[gpui::test]
fn only_visible_lines_are_shaped(cx: &mut TestAppContext) {
    let text = (0..1_000)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (cx, editor) = editor_window(cx, &text);
    let (shaped, placed) = editor.read_with(&cx, |editor, _| {
        (
            editor.last_lines.len(),
            editor
                .layout
                .as_ref()
                .map_or(0, |layout| layout.lines().len()),
        )
    });
    assert!(
        shaped < 100,
        "shaped {shaped} lines; expected a viewport window"
    );
    assert!(
        placed > shaped,
        "the document placed {placed} lines but shaped only {shaped}: the visible window is not selective"
    );
}

#[gpui::test]
fn caret_rect_matches_the_shaped_line(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    cx.simulate_keystrokes("right right right");
    let (caret_offset, rect) =
        editor.read_with(&cx, |editor, _| (editor.caret(), editor.caret_rect()));
    assert_eq!(caret_offset, 3);
    let rect = rect.expect("caret has a rect");
    let line = editor.read_with(&cx, |editor, _| {
        let line = &editor.last_lines[0];
        let expected_x = line.shaped.x_for_index(3);
        (line.bounds.left() + expected_x, line.bounds.top())
    });
    assert_eq!(rect.origin.x, line.0);
    assert_eq!(rect.origin.y, line.1);
}

#[gpui::test]
fn caret_x_includes_the_page_margin(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.simulate_input("xx");
    cx.simulate_keystrokes("left");
    let (rect, expected, margin) = editor.read_with(&cx, |editor, _| {
        let bounds = editor.last_bounds.expect("page bounds");
        let page = editor.last_page.expect("page geometry");
        let line = &editor.last_lines[0];
        let expected = bounds.left() + px(page.margin) + line.shaped.x_for_index(1);
        (
            editor.caret_rect().expect("caret rect"),
            expected,
            page.margin,
        )
    });
    assert!(margin >= 72.0, "margin collapsed to {margin}");
    assert_eq!(
        rect.origin.x, expected,
        "caret x did not include the page margin"
    );
}

#[gpui::test]
fn caret_y_includes_the_head(cx: &mut TestAppContext) {
    let (cx, editor) = editor_window(cx, "hello");
    let (rect, bounds_top) = editor.read_with(&cx, |editor, _| {
        (
            editor.caret_rect().expect("caret rect"),
            editor.last_bounds.expect("page bounds").top(),
        )
    });
    assert_eq!(rect.origin.y, bounds_top + px(HEAD));
    assert!(rect.origin.y >= bounds_top + px(HEAD));
}

#[gpui::test]
fn click_after_margin_maps_to_the_expected_boundary(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    let (target_x, margin_x, y) = editor.read_with(&cx, |editor, _| {
        let bounds = editor.last_bounds.expect("page bounds");
        let page = editor.last_page.expect("page geometry");
        let line = &editor.last_lines[0];
        (
            line.bounds.left() + line.shaped.x_for_index(3),
            bounds.left() + px(page.margin / 2.0),
            line.bounds.top() + px(5.0),
        )
    });

    cx.simulate_keystrokes("right right");
    cx.simulate_click(point(margin_x, y), gpui::Modifiers::default());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        2,
        "a click outside the measured column must not move the caret"
    );

    cx.simulate_click(point(target_x, y), gpui::Modifiers::default());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        3,
        "a click past the margin should use the shaped line's boundary"
    );
}

#[gpui::test]
fn a_long_paragraph_wraps_and_clicks_use_the_placed_line(cx: &mut TestAppContext) {
    let text = "word ".repeat(400);
    let (mut cx, editor) = editor_window(cx, &text);
    let (placed, second) = editor.read_with(&cx, |editor, _| {
        let layout = editor.layout.as_ref().expect("layout");
        assert!(
            layout.lines().len() > 2,
            "the paragraph did not wrap: {} lines",
            layout.lines().len()
        );
        let second = editor
            .last_lines
            .iter()
            .find(|line| line.index == 1)
            .expect("second visual line is visible");
        (
            layout.lines().len(),
            (
                second.bounds.left(),
                second.bounds.top(),
                second.start,
                second.end,
            ),
        )
    });
    assert!(placed > 2);

    cx.simulate_click(
        point(second.0 + px(20.0), second.1 + px(5.0)),
        gpui::Modifiers::default(),
    );
    let caret = editor.read_with(&cx, |editor, _| editor.caret());
    assert!(
        caret >= second.2 && caret <= second.3,
        "click at the second placed line produced caret {caret}, expected {}..={}",
        second.2,
        second.3
    );
    assert!(caret > 0, "a click inside the second line hit the start");
}

#[gpui::test]
fn heading_markers_hang_and_heading_words_align_with_body_text(cx: &mut TestAppContext) {
    let (cx, editor) = editor_window(cx, "# Heading\n\nbody");
    let (hang, marker_x, heading_left, body_left, marker) = editor.read_with(&cx, |editor, _| {
        let layout = editor.layout.as_ref().expect("layout");
        let placed = layout.lines()[0];
        assert!(placed.x < 0.0, "heading marker did not hang");
        let marker = editor.blocks[0].marker;
        let heading = block_lines(editor, 0).remove(0);
        let body = block_lines(editor, 2).remove(0);
        (
            -placed.x,
            heading.shaped.x_for_index(marker),
            heading.bounds.left(),
            body.bounds.left(),
            marker,
        )
    });
    assert!(hang > 0.0);
    assert!(marker > 0);
    assert!(
        (heading_left + marker_x - body_left).abs() < px(1.0),
        "heading words at {} do not align with body edge {body_left} (hang {hang}, marker {marker})",
        heading_left + marker_x
    );
}

#[gpui::test]
fn list_continuations_and_task_boxes_hang_under_their_words(cx: &mut TestAppContext) {
    let list = format!("- item {}", "word ".repeat(40));
    let (mut cx, list_editor) = editor_window(cx, &list);
    let (first_x, continuation_x) = list_editor.read_with(&cx, |editor, _| {
        let layout = editor.layout.as_ref().expect("layout");
        let lines: Vec<_> = layout.lines().iter().filter(|line| !line.rule).collect();
        assert!(lines.len() > 1, "the list item did not wrap");
        (lines[0].x, lines[lines.len() - 1].x)
    });
    assert_eq!(first_x, list_indent());
    assert!(
        continuation_x > first_x,
        "list continuation {continuation_x} did not hang past {first_x}"
    );

    let task = format!("- [ ] task {}", "word ".repeat(40));
    let (cx, task_editor) = editor_window(&mut cx.cx, &task);
    let (task_first_x, task_continuation_x) = task_editor.read_with(&cx, |editor, _| {
        let layout = editor.layout.as_ref().expect("layout");
        let lines: Vec<_> = layout.lines().iter().filter(|line| !line.rule).collect();
        assert!(lines.len() > 1, "the task item did not wrap");
        (lines[0].x, lines[lines.len() - 1].x)
    });
    assert_eq!(task_first_x, first_x);
    assert!(
        task_continuation_x > continuation_x,
        "task continuation {} should hang past the plain list's {continuation_x}",
        task_continuation_x
    );
}

fn list_indent() -> f32 {
    style::look(Block {
        kind: BlockKind::ListItem,
        level: 0,
        start: 0,
        end: 0,
        marker: 0,
    })
    .indent
}

#[gpui::test]
fn a_quote_is_indented_on_every_wrapped_line(cx: &mut TestAppContext) {
    let text = format!("> {}", "quoted ".repeat(60));
    let (cx, editor) = editor_window(cx, &text);
    let (lines, indent) = editor.read_with(&cx, |editor, _| {
        let layout = editor.layout.as_ref().expect("layout");
        (
            layout.lines().len(),
            layout.lines().iter().map(|line| line.x).collect::<Vec<_>>(),
        )
    });
    assert!(lines > 1, "the quote did not wrap");
    let expected = style::look(Block {
        kind: BlockKind::Quote,
        level: 0,
        start: 0,
        end: 0,
        marker: 0,
    })
    .indent;
    for x in indent {
        assert_eq!(x, expected, "quote line was not indented");
    }
}

#[gpui::test]
fn code_lines_have_abutting_backgrounds_and_do_not_wrap(cx: &mut TestAppContext) {
    let text = "```\nfn main() {}\nlet x = 1;\n```";
    let (cx, editor) = editor_window(cx, text);
    let (placed, boxes, column) = editor.read_with(&cx, |editor, _| {
        let layout = editor.layout.as_ref().expect("layout");
        assert_eq!(layout.lines().len(), 4, "one placed line per code line");
        assert!(
            editor
                .blocks
                .iter()
                .all(|block| block.kind == BlockKind::Code)
        );
        (
            layout.lines().len(),
            editor.last_code_boxes.clone(),
            editor.layout_width,
        )
    });
    assert_eq!(boxes.len(), placed);
    for pair in boxes.windows(2) {
        assert!(
            (pair[0].bottom() - pair[1].top()).abs() < px(0.01),
            "code backgrounds did not abut"
        );
    }
    for bounds in boxes {
        assert!((bounds.size.width / px(1.0) - column).abs() < 0.01);
        assert!(bounds.size.height > px(0.0));
    }
}

#[gpui::test]
fn a_rule_is_placed_as_a_one_pixel_line(cx: &mut TestAppContext) {
    let (cx, editor) = editor_window(cx, "before\n\n---\n\nafter");
    let (rules, first_y, last_y, box_y, column, expected_top) =
        editor.read_with(&cx, |editor, _| {
            let layout = editor.layout.as_ref().expect("layout");
            let rule = layout
                .lines()
                .iter()
                .find(|line| line.rule)
                .expect("rule line");
            assert_eq!(editor.blocks[rule.block as usize].kind, BlockKind::Rule);
            assert_eq!(rule.h, 1.0);
            let bounds = editor.last_bounds.expect("bounds");
            (
                editor.last_rule_boxes.clone(),
                layout.lines()[0].y,
                layout.lines()[layout.lines().len() - 1].y,
                rule.y,
                editor.layout_width,
                bounds.top() + HEAD_PX + px(rule.y) - px(editor.scroll),
            )
        });
    assert_eq!(rules.len(), 1);
    assert!(box_y > first_y && box_y < last_y);
    assert!(
        (rules[0].top() / px(1.0) - expected_top / px(1.0)).abs() < 0.5,
        "rule painted at {} but placed at {expected_top}",
        rules[0].top()
    );
    assert!((rules[0].size.width / px(1.0) - column).abs() < 0.01);
    assert_eq!(rules[0].size.height, px(1.0));
}

#[gpui::test]
fn the_caret_sits_on_a_blank_line_and_a_click_lands_there(cx: &mut TestAppContext) {
    let text = "one\n\n\nthree";
    let (mut cx, editor) = editor_window(cx, text);
    cx.update(|_window, cx| {
        editor.update(cx, |editor, cx| {
            editor.caret = 4;
            editor.anchor = None;
            editor.reveal_pending = true;
            editor.reveal_caret();
            cx.notify();
        });
    });
    redraw(&mut cx);
    let (rect, expected_y, h, page_left) = editor.read_with(&cx, |editor, _| {
        let layout = editor.layout.as_ref().expect("layout");
        let top = layout.block_top(1).expect("blank block placed");
        let bounds = editor.last_bounds.expect("bounds");
        (
            editor.caret_rect().expect("caret on blank line"),
            bounds.top() + HEAD_PX + px(top) - px(editor.scroll),
            style::line_height(style::look(editor.blocks[1])),
            editor.page_left(bounds),
        )
    });
    assert!((rect.origin.y / px(1.0) - expected_y / px(1.0)).abs() < 0.5);
    assert!((rect.size.height / px(1.0) - h).abs() < 0.01);
    assert_eq!(rect.origin.x, page_left);

    cx.simulate_click(
        point(rect.origin.x + px(8.0), rect.origin.y + px(5.0)),
        gpui::Modifiers::default(),
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        4,
        "a click on the blank line did not land at its block start"
    );
}

#[gpui::test]
fn a_block_changing_edit_invalidates_the_layout(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "alpha\n\nbravo");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(0..0, window, cx);
        });
    });
    cx.simulate_input("# ");
    let (text, heading, first_x, revision, layout_revision, done) =
        editor.read_with(&cx, |editor, _| {
            let layout = editor.layout.as_ref().expect("layout");
            (
                editor.text().to_owned(),
                editor.blocks[0].kind == BlockKind::Heading,
                layout.lines()[0].x,
                editor.revision(),
                layout.revision(),
                editor.layout_done(),
            )
        });
    assert_eq!(text, "# alpha\n\nbravo");
    assert!(heading, "the block did not become a heading");
    assert!(first_x < 0.0, "the rebuilt heading didn't hang its marker");
    assert_eq!(layout_revision, revision);
    assert!(done, "the rebuilt layout was not painted");
    assert_eq!(editor.read_with(&cx, |editor, _| editor.blocks.len()), 3);
}

#[test]
fn block_and_quote_markers_are_marker_kinds() {
    let cases = ["# Title", "> one\n> two", "- item", "```\ncode\n```"];
    for text in cases {
        let mut blocks = Vec::new();
        scan_blocks(text, &mut blocks);
        for block in blocks {
            let full = &text[block.start..block.end];
            let mut kinds = vec![InlineKind::Plain; full.len()];
            if block.kind != BlockKind::Code {
                inline_kinds(full, &mut kinds);
            }
            apply_block_markers(block, full, &mut kinds);
            if block.kind == BlockKind::Code && block.marker == 0 {
                continue;
            }
            assert!(
                kinds[..block.marker]
                    .iter()
                    .all(|kind| *kind == InlineKind::Marker),
                "block marker bytes were not marked in {text:?}"
            );
        }
    }

    let text = "> one\n> two";
    let mut blocks = Vec::new();
    scan_blocks(text, &mut blocks);
    let block = blocks[0];
    let mut kinds = vec![InlineKind::Plain; text.len()];
    inline_kinds(text, &mut kinds);
    apply_block_markers(block, text, &mut kinds);
    let second = text.rfind('>').expect("second quote marker");
    assert_eq!(kinds[second], InlineKind::Marker);
    assert_eq!(kinds[second + 2], InlineKind::Plain);
}

#[gpui::test]
fn inline_styles_and_links_shape_with_file_offsets(cx: &mut TestAppContext) {
    let text = "**bold** *italic* ~~gone~~ `code` [link](https://example.com)";
    let (mut cx, editor) = editor_window(cx, text);
    let (x, y, link_start) = editor.read_with(&cx, |editor, _| {
        let line = &editor.last_lines[0];
        let link_start = text.find("link").expect("link words");
        (
            line.bounds.left() + line.shaped.x_for_index(link_start + 2),
            line.bounds.top() + px(5.0),
            link_start,
        )
    });
    cx.simulate_click(point(x, y), gpui::Modifiers::default());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        link_start + 2,
        "a click in the link words used the wrong byte offset"
    );
}

#[gpui::test]
fn selection_bounds_span_each_wrapped_line(cx: &mut TestAppContext) {
    let text = "word ".repeat(400);
    let (mut cx, editor) = editor_window(cx, &text);
    let bounds = editor.read_with(&cx, |editor, _| editor.last_bounds.expect("bounds"));
    let quads = cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let end = editor.text().encode_utf16().count();
            editor.selection_bounds_for_range(0..end, bounds, window, cx)
        })
    });
    assert!(
        quads.len() > 1,
        "a wrapped selection returned {} rect(s)",
        quads.len()
    );
}

#[gpui::test]
fn typing_undoes_to_empty_and_redoes_as_one_step(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.simulate_input("hello");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello"
    );

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "",
        "undo did not remove the typing run"
    );
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 0);
    assert!(editor.read_with(&cx, |editor, _| editor.can_redo()));

    redraw(&mut cx);
    assert!(
        editor.read_with(&cx, |editor, _| editor.layout_done()),
        "undo left the layout stale"
    );

    cx.simulate_keystrokes(redo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello"
    );
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 5);
    assert!(!editor.read_with(&cx, |editor, _| editor.can_redo()));
}

#[gpui::test]
fn undo_respects_word_boundaries(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.simulate_input("a b");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "a b"
    );

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "a ",
        "undo should take back the last word run, not the whole sentence"
    );
    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "a"
    );
    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        ""
    );
    assert!(!editor.read_with(&cx, |editor, _| editor.can_undo()));
}

#[gpui::test]
fn movement_does_not_add_an_undo_step(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.simulate_input("hi");
    cx.simulate_keystrokes("left right home end");
    assert!(editor.read_with(&cx, |editor, _| editor.can_undo()));

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "",
        "selection-only changes were recorded as edits"
    );
    assert!(!editor.read_with(&cx, |editor, _| editor.can_undo()));
}

#[gpui::test]
fn backspace_run_is_one_undo_step(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    cx.simulate_keystrokes("end backspace backspace");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hel"
    );
    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello",
        "a run of backspaces was split into several undo steps"
    );
}

#[gpui::test]
fn paste_of_multiple_paragraphs_is_one_step(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.write_to_clipboard(ClipboardItem::new_string("one\n\ntwo".into()));
    cx.simulate_keystrokes(paste_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "one\n\ntwo"
    );

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "",
        "a multi-paragraph paste was split into several undo steps"
    );
}

#[gpui::test]
fn cut_undo_restores_the_selection(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    cx.simulate_keystrokes(select_all_chord());
    cx.simulate_keystrokes(cut_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        ""
    );

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..5),
        "undoing a cut did not restore the selection"
    );
}

#[gpui::test]
fn selection_replacement_undo_restores_text_and_selection(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello world");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(6..11, window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(6..11)
    );

    cx.simulate_input("there");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello there"
    );

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello world"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(6..11),
        "undo did not restore the replaced selection"
    );

    cx.simulate_keystrokes(redo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello there"
    );
}

#[gpui::test]
fn arrows_during_a_live_composition_stay_with_the_ime(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_and_mark_text_in_range(Some(0..0), "に", Some(1..1), window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "に"
    );

    cx.simulate_keystrokes("left right");
    assert!(
        !editor.read_with(&cx, |editor, _| editor.can_undo()),
        "an arrow committed the live preedit into history"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "に",
        "an arrow edited the page during a live composition"
    );

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "にほ", window, cx);
            editor.unmark_text(window, cx);
        });
    });
    assert!(editor.read_with(&cx, |editor, _| editor.can_undo()));
}

#[gpui::test]
fn linux_ime_delete_text_removes_the_marked_preedit(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "abc");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(3..3, window, cx);
            editor.replace_and_mark_text_in_range(Some(3..3), "に", Some(1..1), window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "abcに"
    );

    let marked = editor.read_with(&cx, |editor, _| editor.marked.clone());
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(marked, "", window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "abc",
        "DeleteText restored the preedit instead of deleting it"
    );
    assert!(
        !editor.read_with(&cx, |editor, _| editor.can_undo()),
        "a live preedit delete was recorded as history"
    );

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "にほ", window, cx);
            editor.unmark_text(window, cx);
        });
    });
    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "abc"
    );
}

#[gpui::test]
fn ime_mark_and_commit_is_one_undo_step(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_and_mark_text_in_range(Some(0..0), "に", Some(1..1), window, cx);
        });
    });
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_and_mark_text_in_range(None, "にほ", Some(2..2), window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "にほ"
    );
    assert!(
        !editor.read_with(&cx, |editor, _| editor.can_undo()),
        "the live preedit was recorded"
    );

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "にほ", window, cx);
            editor.unmark_text(window, cx);
        });
    });
    assert!(editor.read_with(&cx, |editor, _| editor.can_undo()));

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "",
        "the IME commit was not one undo step"
    );
    assert!(!editor.read_with(&cx, |editor, _| editor.can_undo()));
    cx.simulate_keystrokes(redo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "にほ"
    );
}

#[gpui::test]
fn ime_cancel_restores_text_and_records_nothing(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "abc");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(1..1, window, cx);
            editor.replace_and_mark_text_in_range(Some(1..1), "に", Some(1..1), window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "aにbc"
    );

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_and_mark_text_in_range(None, "", Some(0..0), window, cx);
            editor.unmark_text(window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "abc"
    );
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), 1);
    assert!(
        !editor.read_with(&cx, |editor, _| editor.can_undo()),
        "cancelling a composition recorded an undo step"
    );
}

#[gpui::test]
fn ime_commit_over_a_selection_restores_it(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello world");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(6..11, window, cx);
            editor.replace_and_mark_text_in_range(Some(6..11), "に", Some(1..1), window, cx);
        });
    });
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "にほ", window, cx);
            editor.unmark_text(window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello にほ"
    );

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello world"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(6..11),
        "undoing an IME commit did not restore the replaced selection"
    );
}

#[gpui::test]
fn the_other_platform_keymap_covers_undo_and_both_redo_chords(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "");
    cx.update(|_, cx| install_keybindings_for(cx, Platform::Other));
    cx.simulate_input("hi");

    cx.simulate_keystrokes("ctrl-z");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        ""
    );
    cx.simulate_keystrokes("ctrl-y");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hi"
    );

    cx.simulate_keystrokes("ctrl-z");
    cx.simulate_keystrokes("ctrl-shift-z");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hi"
    );
}
#[test]
fn task_visual_finds_the_box_and_checked_state() {
    let text = "- [x] done";
    let mut blocks = Vec::new();
    scan_blocks(text, &mut blocks);
    let task = task_visual(blocks[0], text).expect("task item");
    assert_eq!(task.box_start, 2);
    assert_eq!(task.box_end, 5);
    assert_eq!(task.text_start, 6);
    assert!(task.checked);

    let text = "- [ ] open";
    let mut blocks = Vec::new();
    scan_blocks(text, &mut blocks);
    let task = task_visual(blocks[0], text).expect("open task");
    assert!(!task.checked);
    assert_eq!(task.text_start, 6);
}

#[gpui::test]
fn clicking_a_task_box_toggles_it_without_moving_the_caret(cx: &mut TestAppContext) {
    let text = "- [ ] buy milk\n- [x] done";
    let (mut cx, editor) = editor_window(cx, text);
    let boxes = editor.read_with(&cx, |editor, _| editor.last_task_boxes.clone());
    assert_eq!(boxes.len(), 2, "both task boxes should be painted");
    let first = boxes[0].bounds;
    let click = point(
        first.origin.x + first.size.width * 0.5,
        first.origin.y + first.size.height * 0.5,
    );
    cx.simulate_click(click, gpui::Modifiers::default());

    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "- [x] buy milk\n- [x] done"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        0,
        "clicking the checkbox moved the caret"
    );

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        text
    );
}
#[gpui::test]
fn format_actions_rewrite_selection_and_undo_as_one_step(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello world");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(6..11, window, cx);
        });
    });
    cx.dispatch_action(FormatBold);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello **world**"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(8..13)
    );

    cx.simulate_keystrokes(undo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "hello world"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(6..11),
        "undo did not restore the formatted selection"
    );
}

#[gpui::test]
fn heading_and_list_actions_toggle_every_selected_line(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one\ntwo");
    cx.dispatch_action(SelectAll);
    cx.dispatch_action(BulletList);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "- one\n- two"
    );

    cx.dispatch_action(BulletList);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "one\ntwo"
    );

    cx.dispatch_action(Heading1);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "# one\n# two"
    );
}

#[gpui::test]
fn link_action_selects_the_url_placeholder(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "see docs");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(4..8, window, cx);
        });
    });
    cx.dispatch_action(FormatLink);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "see [docs](url)"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(11..14),
        "the URL placeholder was not selected"
    );
}

#[gpui::test]
fn format_keybindings_dispatch_the_real_actions(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(0..5, window, cx);
        });
    });
    cx.simulate_keystrokes(host_chord("cmd-b", "ctrl-b"));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "**hello**"
    );
}
#[gpui::test]
fn list_and_quote_keybindings_dispatch(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one");
    cx.simulate_keystrokes(host_chord("alt-cmd-l", "ctrl-alt-l"));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "- [ ] one"
    );
    cx.simulate_keystrokes(host_chord("alt-cmd-l", "ctrl-alt-l"));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "one"
    );
    cx.simulate_keystrokes(host_chord("cmd-shift-.", "ctrl-shift-."));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "> one"
    );
}
#[gpui::test]
fn stats_refresh_after_a_typing_pause_not_on_every_key(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one two");
    assert_eq!(editor.read_with(&cx, |editor, _| editor.stats().words), 2);

    cx.simulate_keystrokes("end");
    cx.simulate_input(" three");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.stats().words),
        2,
        "stats ran in the keystroke's frame"
    );
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    let stats = editor.read_with(&cx, |editor, _| editor.stats());
    assert_eq!(stats.words, 3);
    assert_eq!(stats.chars, 13);
}
#[gpui::test]
fn focus_mode_tracks_paragraph_and_sentence(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "One. Two.\n\nNext paragraph.");
    assert!(
        editor
            .read_with(&cx, |editor, _| editor.focus_view())
            .is_none()
    );

    cx.dispatch_action(ToggleFocus);
    let focus = editor
        .read_with(&cx, |editor, _| editor.focus_view())
        .expect("paragraph focus");
    assert_eq!((focus.start, focus.end), (0, 9));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(5..5, window, cx);
        });
    });
    cx.dispatch_action(FocusSentence);
    let focus = editor
        .read_with(&cx, |editor, _| editor.focus_view())
        .expect("sentence focus");
    assert_eq!((focus.start, focus.end), (5, 9));

    cx.dispatch_action(ToggleFocus);
    assert!(
        editor
            .read_with(&cx, |editor, _| editor.focus_view())
            .is_none()
    );
}

#[gpui::test]
fn typewriter_holds_the_caret_near_the_middle(cx: &mut TestAppContext) {
    let text = (0..300)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (mut cx, editor) = editor_window(cx, &text);
    let target = text.find("line 150").expect("line 150");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(target..target, window, cx);
        });
    });
    cx.dispatch_action(ToggleTypewriter);
    cx.run_until_parked();
    let (caret_y, page_top, page_height) = editor.read_with(&cx, |editor, _| {
        let bounds = editor.last_bounds.expect("bounds");
        let caret = editor.caret_rect().expect("caret");
        (
            caret.origin.y / px(1.0),
            bounds.top() / px(1.0),
            (bounds.size.height - HEAD_PX) / px(1.0),
        )
    });
    let centre = page_top + (HEAD_PX / px(1.0)) + page_height / 2.0;
    assert!(
        (caret_y - centre).abs() < line_height() * 2.0,
        "caret {caret_y} not near centre {centre}"
    );
}
#[test]
fn focus_dims_text_outside_the_active_range() {
    let text = "one\n\ntwo";
    let mut blocks = Vec::new();
    scan_blocks(text, &mut blocks);
    let tokens = pilcrow_ui::theme::light();
    let focus = FocusView { start: 0, end: 3 };
    let active = run_style(
        blocks[0],
        style::look(blocks[0]),
        None,
        Some(focus),
        None,
        &[],
        None,
        0,
        &tokens,
    );
    let dim = run_style(
        blocks[2],
        style::look(blocks[2]),
        None,
        Some(focus),
        None,
        &[],
        None,
        0,
        &tokens,
    );
    assert_eq!(active.color, tokens.ink);
    assert_eq!(dim.color, tokens.mid);
}
#[gpui::test]
fn mouse_drag_selects_a_range(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello world");
    let (start, end, y) = editor.read_with(&cx, |editor, _| {
        let line = &editor.last_lines[0];
        (
            line.bounds.left() + line.shaped.x_for_index(2),
            line.bounds.left() + line.shaped.x_for_index(8),
            line.bounds.top() + px(5.0),
        )
    });
    cx.simulate_mouse_down(
        point(start, y),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(end, y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(point(end, y), MouseButton::Left, gpui::Modifiers::default());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(2..8),
        "drag did not select the dragged range"
    );
}
#[gpui::test]
fn dragging_past_the_column_selects_the_line_end(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello world");
    let (start, end, y) = editor.read_with(&cx, |editor, _| {
        let line = &editor.last_lines[0];
        let bounds = editor.last_bounds.expect("bounds");
        let page = editor.last_page.expect("page");
        (
            line.bounds.left() + line.shaped.x_for_index(2),
            bounds.left() + px(page.margin) + px(page.column) + px(100.0),
            line.bounds.top() + px(5.0),
        )
    });
    cx.simulate_mouse_down(
        point(start, y),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(end, y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(point(end, y), MouseButton::Left, gpui::Modifiers::default());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(2..11),
        "a drag outside the column froze instead of clamping to the line end"
    );
}

#[gpui::test]
fn dragging_outside_the_window_extends_and_releases(cx: &mut TestAppContext) {
    // the platform delivers drag/release outside the frame to the
    // press window; the offset clamps into the page, and the outside
    // release ends the drag like any other.
    let (mut cx, editor) = editor_window(cx, "hello world");
    let (start, y) = editor.read_with(&cx, |editor, _| {
        let line = &editor.last_lines[0];
        (
            line.bounds.left() + line.shaped.x_for_index(2),
            line.bounds.top() + px(5.0),
        )
    });
    cx.simulate_mouse_down(
        point(start, y),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    // Far past the window's bottom-right corner.
    let outside = point(px(5000.0), px(5000.0));
    cx.simulate_mouse_move(outside, Some(MouseButton::Left), gpui::Modifiers::default());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(2..11),
        "an outside drag froze instead of clamping to the page edge"
    );
    assert!(editor.read_with(&cx, |editor, _| editor.mouse_selecting));
    cx.simulate_mouse_up(outside, MouseButton::Left, gpui::Modifiers::default());
    assert!(
        !editor.read_with(&cx, |editor, _| editor.mouse_selecting),
        "an outside release left the drag stuck"
    );
}

#[gpui::test]
fn a_missed_mouse_up_and_a_blur_both_end_the_drag(cx: &mut TestAppContext) {
    // if the release is never delivered (focus loss, capture lost),
    // the drag must end anyway — on the blur, or on the first later move
    // that arrives with no button held.
    let (mut cx, editor) = editor_window(cx, "hello world");
    // A real drag happens in the active window; GPUI only reports focus
    // paths for active windows.
    cx.update(|window, _cx| window.activate_window());
    redraw(&mut cx);
    let (start, mid, end, y) = editor.read_with(&cx, |editor, _| {
        let line = &editor.last_lines[0];
        (
            line.bounds.left() + line.shaped.x_for_index(1),
            line.bounds.left() + line.shaped.x_for_index(4),
            line.bounds.left() + line.shaped.x_for_index(9),
            line.bounds.top() + px(5.0),
        )
    });

    // Missed up: the next buttonless move ends the drag, and later hovers
    // do not keep extending the selection.
    cx.simulate_mouse_down(
        point(start, y),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(mid, y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(1..4)
    );
    cx.simulate_mouse_move(point(end, y), None, gpui::Modifiers::default());
    assert!(
        !editor.read_with(&cx, |editor, _| editor.mouse_selecting),
        "a buttonless move did not heal the missed up"
    );
    cx.simulate_mouse_move(point(start, y), None, gpui::Modifiers::default());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(1..4),
        "hover after a missed up kept extending the selection"
    );

    // Blur mid-drag: the drag ends, and a later pressed move (a fresh
    // press this window also missed) cannot resurrect it.
    cx.simulate_mouse_down(
        point(start, y),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(mid, y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    assert!(editor.read_with(&cx, |editor, _| editor.mouse_selecting));
    cx.update(|window, cx| window.blur(cx));
    // Focus listeners fire as the next frame draws.
    redraw(&mut cx);
    cx.run_until_parked();
    assert!(
        !editor.read_with(&cx, |editor, _| editor.mouse_selecting),
        "blur mid-drag left the drag stuck"
    );
    cx.simulate_mouse_move(
        point(end, y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(1..4),
        "a move after blur resurrected the drag"
    );
}

#[gpui::test]
fn offscreen_task_boxes_are_not_clickable(cx: &mut TestAppContext) {
    let text = (0..100)
        .map(|index| format!("- [ ] task {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (mut cx, editor) = editor_window(cx, &text);
    cx.update(|_window, cx| {
        editor.update(cx, |editor, cx| {
            editor.scroll_by(60.0);
            cx.notify();
        });
    });
    cx.run_until_parked();
    let stale = editor.read_with(&cx, |editor, _| {
        let page_top = editor.last_bounds.expect("bounds").top() + HEAD_PX;
        editor
            .last_task_boxes
            .iter()
            .find(|task| task.bounds.top() < page_top)
            .map(|task| task.bounds)
            .expect("an offscreen run-up task box")
    });
    let before = editor.read_with(&cx, |editor, _| editor.text().to_owned());
    let center = point(
        stale.origin.x + stale.size.width * 0.5,
        stale.origin.y + stale.size.height * 0.5,
    );
    cx.simulate_click(center, gpui::Modifiers::default());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        before,
        "a click in the blank header toggled an offscreen task"
    );
}

#[gpui::test]
fn redo_restores_the_selection_a_format_command_created(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "hello world");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(6..11, window, cx);
        });
    });
    cx.dispatch_action(FormatBold);
    cx.simulate_keystrokes(undo_chord());
    cx.simulate_keystrokes(redo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(8..13),
        "redo lost the formatted selection"
    );
}

#[gpui::test]
fn redo_restores_the_caret_after_a_task_toggle(cx: &mut TestAppContext) {
    let text = "- [ ] one\n- [ ] two";
    let (mut cx, editor) = editor_window(cx, text);
    let eof = text.len();
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(eof..eof, window, cx);
        });
    });
    let box_rect = editor.read_with(&cx, |editor, _| editor.last_task_boxes[0].bounds);
    cx.simulate_click(
        point(
            box_rect.origin.x + box_rect.size.width * 0.5,
            box_rect.origin.y + box_rect.size.height * 0.5,
        ),
        gpui::Modifiers::default(),
    );
    assert_eq!(editor.read_with(&cx, |editor, _| editor.caret()), eof);
    cx.simulate_keystrokes(undo_chord());
    cx.simulate_keystrokes(redo_chord());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        eof,
        "redo moved the caret after a checkbox toggle"
    );
}

#[test]
fn word_selection_keeps_a_zwj_emoji_whole() {
    let text = "a👨‍👩‍👧‍👦b";
    let range = word_range(text, 1);
    assert_eq!(&text[range], "👨‍👩‍👧‍👦");
}
#[gpui::test]
fn sentence_focus_handles_cjk_and_closing_quotes(cx: &mut TestAppContext) {
    let text = "He said \"Go.\" Then left.";
    let (mut cx, editor) = editor_window(cx, text);
    let go = text.find("\"Go").expect("quote") + 1;
    cx.update(|_window, cx| {
        editor.update(cx, |editor, _| editor.caret = go);
    });
    cx.dispatch_action(FocusSentence);
    let range = editor
        .read_with(&cx, |editor, _| editor.focus_view())
        .expect("sentence");
    assert_eq!(&text[range.start..range.end], "He said \"Go.\"");

    let then_pos = text.find("Then").expect("second sentence");
    cx.update(|_window, cx| {
        editor.update(cx, |editor, _| editor.caret = then_pos);
    });
    let range = editor
        .read_with(&cx, |editor, _| editor.focus_view())
        .expect("second sentence");
    assert_eq!(&text[range.start..range.end], "Then left.");

    let text = "日本語。次です。";
    let (mut cx, editor) = editor_window(&mut cx.cx, text);
    let next = text.find('次').expect("next sentence");
    cx.update(|_window, cx| {
        editor.update(cx, |editor, _| editor.caret = next);
    });
    cx.dispatch_action(FocusSentence);
    let range = editor
        .read_with(&cx, |editor, _| editor.focus_view())
        .expect("CJK sentence");
    assert_eq!(&text[range.start..range.end], "次です。");
}
#[gpui::test]
fn all_list_forms_paint_and_toggle_task_boxes(cx: &mut TestAppContext) {
    let text = "- [ ] dash\n* [ ] star\n+ [x] plus\n1. [ ] ordered\n  - [ ] nested";
    let (mut cx, editor) = editor_window(cx, text);
    let boxes = editor.read_with(&cx, |editor, _| editor.last_task_boxes.clone());
    assert_eq!(boxes.len(), 5, "not every task form painted a checkbox");
    let star = boxes[1].bounds;
    let center = point(
        star.origin.x + star.size.width * 0.5,
        star.origin.y + star.size.height * 0.5,
    );
    cx.simulate_click(center, gpui::Modifiers::default());
    assert!(editor.read_with(&cx, |editor, _| editor.text().contains("* [x] star")));
    cx.simulate_click(center, gpui::Modifiers::default());
    assert!(editor.read_with(&cx, |editor, _| editor.text().contains("* [ ] star")));
}

#[gpui::test]
fn every_format_keybinding_dispatches(cx: &mut TestAppContext) {
    let cases = [
        (host_chord("cmd-i", "ctrl-i"), "_hello_"),
        (host_chord("alt-cmd-u", "ctrl-alt-u"), "~~hello~~"),
        (host_chord("cmd-j", "ctrl-j"), "`hello`"),
        (host_chord("cmd-2", "ctrl-2"), "## hello"),
        (host_chord("cmd-shift-l", "ctrl-shift-l"), "1. hello"),
    ];
    for (chord, expected) in cases {
        let (mut cx, editor) = editor_window(cx, "hello");
        cx.dispatch_action(SelectAll);
        cx.simulate_keystrokes(chord);
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.text().to_owned()),
            expected,
            "chord {chord} did not dispatch its format command"
        );
    }

    let (mut cx, editor) = editor_window(cx, "docs");
    cx.dispatch_action(SelectAll);
    cx.simulate_keystrokes(host_chord("cmd-k", "ctrl-k"));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "[docs](url)"
    );
}

#[gpui::test]
fn a_multi_paragraph_selection_is_not_claimed_by_inline_format(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one\n\ntwo");
    cx.dispatch_action(SelectAll);
    cx.dispatch_action(FormatBold);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "one\n\ntwo",
        "inline formatting wrote markers across a paragraph break"
    );
    assert!(!editor.read_with(&cx, |editor, _| editor.can_undo()));
}
#[gpui::test]
fn stats_follow_undo_and_redo(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one two");
    cx.simulate_keystrokes("end");
    cx.simulate_input(" three");
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(editor.read_with(&cx, |editor, _| editor.stats().words), 3);

    cx.simulate_keystrokes(undo_chord());
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.stats().words),
        2,
        "undo left the old word count in the footer"
    );

    cx.simulate_keystrokes(redo_chord());
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(editor.read_with(&cx, |editor, _| editor.stats().words), 3);
}
#[test]
fn completed_task_text_is_mid_and_struck() {
    let text = "- [x] done\n- [ ] open";
    let mut blocks = Vec::new();
    scan_blocks(text, &mut blocks);
    let tokens = pilcrow_ui::theme::light();
    let done = task_visual(blocks[0], &text[blocks[0].start..blocks[0].end]).unwrap();
    let style = run_style(
        blocks[0],
        style::look(blocks[0]),
        Some(done),
        None,
        None,
        &[],
        None,
        done.text_start,
        &tokens,
    );
    assert_eq!(style.color, tokens.mid);
    assert!(
        style.strikethrough.is_some(),
        "completed task was not struck"
    );

    // The `[x]` marker itself stays ordinary text; only the body is struck.
    let marker = run_style(
        blocks[0],
        style::look(blocks[0]),
        Some(done),
        None,
        None,
        &[],
        None,
        done.box_start,
        &tokens,
    );
    assert_eq!(
        marker.color, tokens.ink,
        "task marker was hidden or replaced"
    );

    let open = task_visual(blocks[1], &text[blocks[1].start..blocks[1].end]).unwrap();
    let style = run_style(
        blocks[1],
        style::look(blocks[1]),
        Some(open),
        None,
        None,
        &[],
        None,
        open.text_start,
        &tokens,
    );
    assert_eq!(style.color, tokens.ink);
    assert!(style.strikethrough.is_none());
}

#[gpui::test]
fn typewriter_leaves_a_short_note_at_the_top_and_keeps_the_end_visible(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one line");
    cx.dispatch_action(ToggleTypewriter);
    assert_eq!(editor.read_with(&cx, |editor, _| editor.scroll()), 0.0);

    let text = (0..300)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let eof = text.len();
    let (mut cx, editor) = editor_window(&mut cx.cx, &text);
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(eof..eof, window, cx);
        });
    });
    cx.dispatch_action(ToggleTypewriter);
    cx.run_until_parked();
    assert!(editor.read_with(&cx, |editor, _| editor.scroll()) > 0.0);
    assert!(
        editor
            .read_with(&cx, |editor, _| editor.caret_rect())
            .is_some(),
        "the end caret left the viewport in typewriter mode"
    );
}
#[gpui::test]
fn dragging_from_a_heading_marker_selects_the_hash(cx: &mut TestAppContext) {
    let text = "# Heading\n\nbody";
    let (mut cx, editor) = editor_window(cx, text);
    let (left, end, y) = editor.read_with(&cx, |editor, _| {
        let line = &editor.last_lines[0];
        (
            line.bounds.left() + px(1.0),
            line.bounds.left() + line.shaped.x_for_index(9),
            line.bounds.top() + px(5.0),
        )
    });
    cx.simulate_mouse_down(
        point(left, y),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(end, y),
        Some(MouseButton::Left),
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(point(end, y), MouseButton::Left, gpui::Modifiers::default());
    let selection = editor
        .read_with(&cx, |editor, _| editor.selection())
        .expect("heading selection");
    assert!(
        text[selection.clone()].starts_with('#'),
        "the hanging hash was not selectable: {selection:?}"
    );
}
#[gpui::test]
fn editing_inside_a_long_paragraph_matches_a_fresh_layout(cx: &mut TestAppContext) {
    let text = (0..400)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let (mut cx, editor) = editor_window(cx, &text);
    let target = text.find("word200").expect("middle word");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(target..target, window, cx);
        });
    });
    cx.simulate_input("X");
    cx.run_until_parked();

    let (incremental, fresh) = cx.update(|window, cx| {
        let editor = editor.read(cx);
        let incremental = editor.layout.as_ref().expect("layout").lines().to_vec();
        let mut layout = Layout::begin_at(
            &editor.blocks,
            editor.last_layout_width,
            editor.buffer.revision(),
        );
        layout.extend(
            editor.buffer.text(),
            &editor.blocks,
            &WindowMeasurer::new(window.text_system()),
            0,
        );
        (incremental, layout.lines().to_vec())
    });
    assert_eq!(
        incremental, fresh,
        "line-level rewrap diverged from a fresh layout"
    );
}
#[gpui::test]
fn incremental_rescan_preserves_earlier_blocks_and_fences(cx: &mut TestAppContext) {
    let text = "alpha\n\n```\nlet code = 1;\n```\n\nomega";
    let (mut cx, editor) = editor_window(cx, text);
    let before = editor.read_with(&cx, |editor, _| editor.blocks.clone());
    let code_at = text.find("let code").expect("code");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(code_at + 4..code_at + 4, window, cx);
        });
    });
    cx.simulate_input("X");
    let (blocks, changed_text) = editor.read_with(&cx, |editor, _| {
        (editor.blocks.clone(), editor.text().to_owned())
    });
    assert_eq!(blocks[0], before[0], "prefix block changed");
    let code_block = blocks
        .iter()
        .find(|block| block.kind == BlockKind::Code && block.start <= code_at)
        .expect("a code block");
    assert_eq!(code_block.kind, BlockKind::Code);
    assert!(changed_text.contains("let Xcode"));

    let omega = changed_text.find("omega").expect("omega");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(omega..omega, window, cx);
        });
    });
    cx.simulate_input("Y");
    let blocks = editor.read_with(&cx, |editor, _| editor.blocks.clone());
    assert!(
        blocks.iter().any(|block| {
            block.kind == BlockKind::Paragraph && block.start <= omega && block.end >= omega
        }),
        "the text after the fence was parsed as code"
    );
}

#[gpui::test]
fn enter_continues_lists_and_exits_an_empty_item(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "- [ ] item");
    cx.simulate_keystrokes("end enter");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "- [ ] item\n- [ ] "
    );
    cx.simulate_input("next");
    cx.simulate_keystrokes("enter enter");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "- [ ] item\n- [ ] next\n"
    );

    let (mut cx, ordered) = editor_window(&mut cx.cx, "2. item");
    cx.simulate_keystrokes("end enter");
    assert_eq!(
        ordered.read_with(&cx, |editor, _| editor.text().to_owned()),
        "2. item\n3. "
    );
}

#[gpui::test]
fn enter_splits_a_plain_line_or_a_selection(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one two");
    cx.simulate_keystrokes("end left left left enter");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "one \ntwo"
    );
}
#[gpui::test]
fn word_and_selection_keys_follow_platform_conventions(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one two three");
    cx.simulate_keystrokes("end");
    cx.simulate_keystrokes(host_chord("alt-left", "ctrl-left"));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        8,
        "word-left did not stop at the last word"
    );
    cx.simulate_keystrokes(host_chord("shift-alt-left", "ctrl-shift-left"));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(4..8)
    );

    cx.simulate_keystrokes("right");
    cx.simulate_keystrokes(host_chord("alt-backspace", "ctrl-backspace"));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "one three"
    );

    let (mut cx, editor) = editor_window(&mut cx.cx, "one two");
    cx.simulate_keystrokes("end");
    cx.simulate_keystrokes(host_chord("cmd-shift-left", "shift-home"));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..7)
    );
    cx.simulate_keystrokes("home");
    cx.simulate_keystrokes(host_chord("cmd-shift-right", "shift-end"));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..7)
    );
    cx.dispatch_action(DeleteToLineStart);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        ""
    );

    let (mut cx, editor) = editor_window(&mut cx.cx, "first\n\nsecond");
    cx.update(|_, cx| install_keybindings_for(cx, Platform::Other));
    cx.simulate_keystrokes("home ctrl-shift-end");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..13)
    );
    cx.simulate_keystrokes("end ctrl-shift-home");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..13)
    );
}
#[gpui::test]
fn deleting_a_blank_line_rebuilds_a_merged_paragraph_correctly(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one\n\ntwo");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(5..5, window, cx);
        });
    });
    cx.simulate_keystrokes("backspace");
    cx.run_until_parked();
    let (text, incremental, fresh) = cx.update(|window, cx| {
        let editor = editor.read(cx);
        let text = editor.buffer.text().to_owned();
        let incremental = editor.layout.as_ref().expect("layout").lines().to_vec();
        let mut layout = Layout::begin_at(
            &editor.blocks,
            editor.last_layout_width,
            editor.buffer.revision(),
        );
        layout.extend(
            &text,
            &editor.blocks,
            &WindowMeasurer::new(window.text_system()),
            0,
        );
        (text, incremental, layout.lines().to_vec())
    });
    assert_eq!(text, "one\ntwo");
    assert_eq!(incremental, fresh, "merged paragraph kept stale geometry");
}
#[gpui::test]
fn find_finds_matches_and_walks_them_with_wrapping(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one cat two cat cat");
    cx.dispatch_action(Find);
    cx.simulate_input("cat");
    assert!(editor.read_with(&cx, |editor, _| editor.is_finding()));
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.find_query().to_owned()),
        "cat"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.find_matches().to_vec()),
        vec![4..7, 12..15, 16..19]
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.active_find_match()),
        Some(4..7)
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(4..7)
    );

    cx.dispatch_action(FindNext);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.active_find_match()),
        Some(12..15)
    );
    cx.dispatch_action(FindNext);
    cx.dispatch_action(FindNext);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.active_find_match()),
        Some(4..7),
        "find-next did not wrap"
    );
    cx.dispatch_action(FindPrevious);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.active_find_match()),
        Some(16..19),
        "find-previous did not wrap"
    );

    cx.dispatch_action(CloseFind);
    assert!(!editor.read_with(&cx, |editor, _| editor.is_finding()));
    cx.simulate_keystrokes("end");
    cx.simulate_input("X");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "one cat two cat catX"
    );
}

#[gpui::test]
fn find_can_be_case_sensitive_and_never_matches_an_empty_query(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "Cat cat CAT");
    cx.dispatch_action(Find);
    cx.simulate_input("cat");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.find_matches().len()),
        3
    );
    let bounds = {
        redraw(&mut cx);
        cx.debug_bounds("find-case").expect("case button")
    };
    cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.find_matches().to_vec()),
        vec![4..7]
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "Cat cat CAT",
        "finding moved text"
    );
}

#[gpui::test]
fn replace_all_is_one_undo_step(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "cat and cat");
    cx.dispatch_action(Find);
    cx.simulate_input("cat");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            if let Some(find) = editor.find.as_mut() {
                find.replacement = "dog".to_owned();
                find.replacement_cursor = 3;
                find.field = FindField::Replacement;
            }
            editor.replace_current_match(true, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "dog and dog"
    );
    cx.dispatch_action(Undo);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "cat and cat",
        "replace-all was not one undo step"
    );
    cx.dispatch_action(Redo);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "dog and dog"
    );
}

#[gpui::test]
fn replace_one_advances_to_the_next_remaining_match(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "cat cat cat");
    cx.dispatch_action(Find);
    cx.simulate_input("cat");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            if let Some(find) = editor.find.as_mut() {
                find.replacement = "dog".to_owned();
                find.replacement_cursor = 3;
                find.field = FindField::Replacement;
            }
            editor.replace_current_match(false, window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "dog cat cat"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.active_find_match()),
        Some(4..7)
    );
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_current_match(false, window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "dog dog cat"
    );
}

#[gpui::test]
fn editing_the_document_refreshes_find_without_stealing_the_caret(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "cat");
    cx.dispatch_action(Find);
    cx.simulate_input("cat");
    cx.dispatch_action(CloseFind);
    cx.simulate_keystrokes("end");
    cx.simulate_input("s");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "cats"
    );
    cx.dispatch_action(Find);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.find_matches().to_vec()),
        vec![0..3]
    );
}

#[gpui::test]
fn reading_view_uses_the_syntax_free_copy_and_pages(cx: &mut TestAppContext) {
    let text = format!(
        "# Title\n\n- [x] task\n\n{}",
        (0..80)
            .map(|index| format!("paragraph {index}\n\n"))
            .collect::<String>()
    );
    let (mut cx, editor) = editor_window(cx, &text);
    cx.dispatch_action(ToggleReading);
    assert!(editor.read_with(&cx, |editor, _| editor.reading));
    redraw(&mut cx);
    assert!(
        cx.debug_bounds("reading-page").is_some(),
        "reading view did not render"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.reading_scroll.offset().y),
        px(0.0)
    );
    cx.dispatch_action(PageDown);
    redraw(&mut cx);
    assert!(
        editor.read_with(&cx, |editor, _| editor.reading_scroll.offset().y) < px(0.0),
        "reading view did not scroll to the next page"
    );
    cx.dispatch_action(ToggleReading);
    assert!(!editor.read_with(&cx, |editor, _| editor.reading));
}

#[gpui::test]
fn the_page_is_a_focusable_note_with_accessibility_actions(cx: &mut TestAppContext) {
    let (mut cx, _editor) = editor_window(cx, "alpha beta");
    redraw(&mut cx);
    assert!(
        cx.debug_bounds("prose-input").is_some(),
        "the accessible page wrapper did not render"
    );
}
#[gpui::test]
fn selection_stats_report_only_the_selected_words(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "one two three");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(4..7, window, cx);
        });
    });
    let stats = editor
        .read_with(&cx, |editor, _| editor.selection_stats())
        .expect("selection stats");
    assert_eq!(stats.words, 1);
    assert_eq!(stats.chars, 3);
}
#[gpui::test]
fn document_actions_do_not_edit_the_note_while_the_find_field_is_focused(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "keep this\n");
    cx.dispatch_action(Find);
    cx.simulate_input("needle");
    let before = editor.read_with(&cx, |editor, _| editor.text().to_owned());
    cx.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("PASTE".to_owned()));
    });
    cx.dispatch_action(Paste);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        before,
        "Cmd-V in the find field pasted into the document"
    );
    cx.dispatch_action(BackspaceWord);
    cx.dispatch_action(DeleteToLineStart);
    cx.dispatch_action(Undo);
    cx.dispatch_action(FormatBold);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        before,
        "a find-field action leaked into the document"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.find_query().to_owned()),
        "",
        "the find field did not consume the word delete"
    );
    cx.dispatch_action(CloseFind);
    cx.dispatch_action(DocEnd);
    cx.simulate_input("X");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "keep this\nX"
    );
}
#[cfg(target_os = "macos")]
#[gpui::test]
fn spellcheck_marks_prose_but_not_code(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "teh word\n\n```\nteh word\n```");
    cx.update(|_, cx| {
        editor.update(cx, |editor, cx| editor.refresh_spellcheck(cx));
    });
    let ranges = editor.read_with(&cx, |editor, _| editor.spell.clone());
    assert!(
        ranges.contains(&(0..3)),
        "the prose misspelling was not marked: {ranges:?}"
    );
    assert!(
        ranges.iter().all(|range| range.start < 4),
        "a code-block word was marked: {ranges:?}"
    );
}
#[cfg(target_os = "macos")]
#[gpui::test]
fn spellcheck_waits_for_the_idle_pause(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "teh word");
    cx.simulate_keystrokes("end");
    cx.simulate_input("s");
    assert!(
        editor.read_with(&cx, |editor, _| editor.spell.is_empty()),
        "spellcheck ran in the typing frame"
    );
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(700));
    cx.run_until_parked();
    assert!(
        editor.read_with(&cx, |editor, _| editor.spell.contains(&(0..3))),
        "spellcheck did not run after the idle pause"
    );
}
#[gpui::test]
fn typing_after_a_closing_fence_keeps_prose_a_paragraph(cx: &mut TestAppContext) {
    // The rescan must restart at the fence opener: restarting at the closing
    // fence would reopen it and swallow the prose below as code.
    let (mut cx, editor) = editor_window(cx, "```\ncode\n```\n\nprose text\n\nlast para");
    cx.simulate_keystrokes(doc_end_chord());
    cx.simulate_input(" more");
    let (text, blocks) = editor.read_with(&cx, |editor, _| {
        (editor.text().to_owned(), editor.blocks.clone())
    });
    let mut fresh = Vec::new();
    scan_blocks(&text, &mut fresh);
    assert_eq!(blocks, fresh, "blocks diverged from a fresh scan");
    assert_eq!(
        blocks.last().expect("last block").kind,
        BlockKind::Paragraph
    );
}
#[gpui::test]
fn resynced_blocks_match_a_fresh_scan_after_edit_and_undo(cx: &mut TestAppContext) {
    let mut text = String::new();
    for index in 0..40 {
        text.push_str(&format!("paragraph {index} has some words\n\n"));
    }
    let (mut cx, editor) = editor_window(cx, &text);
    cx.simulate_input("x");
    let assert_fresh = |cx: &VisualTestContext| {
        let (text, blocks) = editor.read_with(cx, |editor, _| {
            (editor.text().to_owned(), editor.blocks.clone())
        });
        let mut fresh = Vec::new();
        scan_blocks(&text, &mut fresh);
        assert_eq!(blocks, fresh, "blocks diverged from a fresh scan");
    };
    assert_fresh(&cx);
    editor.read_with(&cx, |editor, _| {
        assert!(editor.text().starts_with("xparagraph"));
    });
    cx.simulate_keystrokes(undo_chord());
    assert_fresh(&cx);
    editor.read_with(&cx, |editor, _| {
        assert!(editor.text().starts_with("paragraph"));
    });
}
#[gpui::test]
fn a_hundred_thousand_word_note_types_at_the_start(cx: &mut TestAppContext) {
    // Perf fixture: opening and typing in a long note must shape only the
    // visible lines and keep the caret placed. Work is asserted structurally
    // (visible line count, fresh-scan equality), never by wall time.
    let mut text = String::new();
    for paragraph in 0..1_000 {
        text.push_str(&format!("para{paragraph}"));
        for _ in 1..100 {
            text.push_str(" word");
        }
        text.push_str("\n\n");
    }
    let (mut cx, editor) = editor_window(cx, &text);
    cx.simulate_input("x");
    cx.run_until_parked();
    let (text_now, blocks, visible, caret_rect, frame_seq) = editor.read_with(&cx, |editor, _| {
        (
            editor.text().to_owned(),
            editor.blocks.clone(),
            editor.visible_lines().len(),
            editor.caret_rect(),
            editor.frame_seq(),
        )
    });
    assert!(text_now.starts_with("xpara0"));
    let mut fresh = Vec::new();
    scan_blocks(&text_now, &mut fresh);
    assert_eq!(blocks, fresh, "blocks diverged from a fresh scan at scale");
    assert!(
        visible < 200,
        "shaped {visible} visible lines for one viewport of a long note"
    );
    assert!(caret_rect.is_some(), "caret lost its place in a long note");
    assert!(frame_seq > 0, "nothing painted");
}

#[gpui::test]
fn jumping_to_the_end_of_a_long_note_lands_without_wrapping_it_all(cx: &mut TestAppContext) {
    // doc-end reseeds placement near the caret from the estimated
    // prefix. The caret is placed on the first drawn frame, and the
    // layout window covers only the tail, not the whole document.
    let mut text = String::new();
    for paragraph in 0..1_000 {
        text.push_str(&format!("para{paragraph}"));
        for _ in 1..100 {
            text.push_str(" word");
        }
        text.push_str("\n\n");
    }
    let (mut cx, editor) = editor_window(cx, &text);
    cx.simulate_keystrokes(doc_end_chord());

    let (caret, len, caret_rect, window) = editor.read_with(&cx, |editor, _| {
        let layout = editor.layout.as_ref().expect("layout installed");
        (
            editor.caret(),
            editor.text().len(),
            editor.caret_rect(),
            (
                layout.window_start(),
                layout.next_block(),
                editor.blocks.len(),
            ),
        )
    });
    assert_eq!(caret, len, "doc-end did not reach the end");
    assert!(
        caret_rect.is_some(),
        "caret not placed after the jump frame"
    );
    let (window_start, next_block, blocks) = window;
    assert!(window_start > 0, "no jump: placement still starts at 0");
    assert!(
        next_block - window_start < blocks / 2,
        "jump placed {} of {blocks} blocks",
        next_block - window_start
    );

    // Typing lands at the end and stays placed; moving up from the jumped
    // caret works instead of sticking at the window's top edge.
    cx.simulate_input("x");
    assert!(editor.read_with(&cx, |editor, _| editor.text().ends_with('x')));
    assert!(
        editor
            .read_with(&cx, |editor, _| editor.caret_rect())
            .is_some()
    );
    let caret_at_end = editor.read_with(&cx, |editor, _| editor.caret());
    cx.simulate_keystrokes("up");
    assert!(
        editor.read_with(&cx, |editor, _| editor.caret()) < caret_at_end,
        "up from a jumped caret did not move"
    );

    // And back: doc-start reseeds at the top, exact from y=0.
    cx.simulate_keystrokes(host_chord("cmd-up", "ctrl-home"));
    let (caret, caret_rect, window_start) = editor.read_with(&cx, |editor, _| {
        (
            editor.caret(),
            editor.caret_rect(),
            editor.layout.as_ref().expect("layout").window_start(),
        )
    });
    assert_eq!(caret, 0);
    assert_eq!(window_start, 0);
    assert!(caret_rect.is_some());
}
#[gpui::test]
fn trace_records_inputs_and_painted_frames(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "trace me\n\nsecond para");
    cx.update(|_, cx| {
        editor.update(cx, |editor, _| editor.trace_start());
    });
    cx.simulate_input("x");
    cx.update(|_, cx| {
        editor.update(cx, |editor, _| editor.note_driver_input());
    });
    cx.run_until_parked();
    let (records, revision, active_after_stop) = cx.update(|_, cx| {
        editor.update(cx, |editor, _| {
            let records = editor.trace_stop();
            (records, editor.revision(), editor.trace_active())
        })
    });
    assert!(!active_after_stop);
    assert!(
        records.len() >= 2,
        "baseline and input records: {records:?}"
    );
    assert_eq!(records.first().expect("baseline").input_seq, 0);
    assert!(
        records.iter().any(|record| record.input_seq == 1),
        "the driver input was not recorded: {records:?}"
    );
    assert!(
        records
            .windows(2)
            .all(|pair| pair[1].input_seq >= pair[0].input_seq && pair[1].seq >= pair[0].seq),
        "trace records are not ordered: {records:?}"
    );
    let last = records.last().expect("last record");
    assert_eq!(last.revision, revision, "trace settled on a stale revision");
    assert!(last.frame_ms >= 0.0);
    // A stopped trace records nothing further.
    let more = cx.update(|_, cx| editor.update(cx, |editor, _| editor.trace_stop()));
    assert!(more.is_empty());
}
#[gpui::test]
fn reading_view_is_read_only(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "words stay put");
    cx.dispatch_action(ToggleReading);
    cx.simulate_input("x");
    cx.dispatch_action(FormatBold);
    cx.simulate_keystrokes("backspace delete");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "words stay put",
        "reading view edited the hidden document"
    );
}
#[cfg(target_os = "macos")]
#[gpui::test]
fn spellcheck_runs_after_opening_a_note(cx: &mut TestAppContext) {
    let (cx, editor) = editor_window(cx, "teh word");
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(700));
    cx.run_until_parked();
    assert!(
        editor.read_with(&cx, |editor, _| editor.spell.contains(&(0..3))),
        "opening a note did not schedule spellcheck"
    );
}
#[cfg(target_os = "macos")]
#[gpui::test]
fn spellcheck_walks_a_long_note_chunk_by_chunk(cx: &mut TestAppContext) {
    // Past one chunk budget, with a misspelling in the first and last lines:
    // the pass has to walk every chunk before the last one is marked.
    let filler = "the quick brown fox jumps over the lazy dog\n\n".repeat(1600);
    let text = format!("teh first line\n\n{filler}teh last line");
    let last = text.rfind("teh").expect("last misspelling");
    assert!(text.len() > 2 * crate::spell::SPELL_CHUNK_BYTES);
    let (cx, editor) = editor_window(cx, &text);
    cx.background_executor
        .advance_clock(std::time::Duration::from_secs(10));
    cx.run_until_parked();
    let ranges = editor.read_with(&cx, |editor, _| editor.spell.clone());
    assert!(
        ranges.contains(&(0..3)),
        "the first chunk was not marked: {} ranges",
        ranges.len()
    );
    assert!(
        ranges.contains(&(last..last + 3)),
        "the chunked pass never reached the end of the note: {} ranges",
        ranges.len()
    );
}

#[gpui::test]
fn the_keyboard_toggles_a_task_with_one_undo_step(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "- [ ] buy milk\n");
    cx.simulate_keystrokes("right right right right right right");
    let caret = editor.read_with(&cx, |editor, _| editor.caret());
    cx.dispatch_action(ToggleTask);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "- [x] buy milk\n"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.caret()),
        caret,
        "toggling a task moved the caret"
    );
    cx.dispatch_action(Undo);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "- [ ] buy milk\n"
    );
}

#[gpui::test]
fn text_scale_changes_line_geometry_without_changing_bytes(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "# Heading\n\nBody");
    redraw(&mut cx);
    let (small_height, small_text) = editor.read_with(&cx, |editor, _| {
        (editor.last_lines[0].h, editor.text().to_owned())
    });
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| editor.set_scale(1.25, cx));
        window.draw(cx).clear(cx);
    });
    let large_height = editor.read_with(&cx, |editor, _| editor.last_lines[0].h);
    assert!(
        large_height > small_height,
        "text scale did not grow the page"
    );
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        small_text,
        "text scale changed document bytes"
    );
}

#[gpui::test]
fn editor_snapshots_share_text_and_capture_view_state(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "snapshot words");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(0..8, window, cx);
            editor.set_scale(1.125, cx);
            editor.set_measure(76, cx);
        });
    });
    let snap = editor.read_with(&cx, |editor, _| editor.snapshot());
    assert_eq!(&*snap.text, "snapshot words");
    assert_eq!(snap.caret, 8);
    assert_eq!(snap.anchor, Some(0));
    assert_eq!(snap.scale, 1.125);
    assert_eq!(snap.measure_chars, 76);
    assert_eq!(
        snap.revision,
        editor.read_with(&cx, |editor, _| editor.revision())
    );
}

/// Record every typed event an editor emits from now on.
#[gpui::test]
fn the_bundled_face_measures_the_column(cx: &mut TestAppContext) {
    // IBM Plex Mono's advance is exactly 0.6 em, so the 66-x column at
    // 16 px measures 633.6 pt. The realistic host fallbacks live measurably
    // elsewhere (Menlo and DejaVu Sans Mono are both ≈0.602 em), so this
    // catches a silent fallback if install_fonts ever stops registering.
    let (mut cx, editor) = editor_window(cx, "x");
    redraw(&mut cx);
    let width = editor.read_with(&cx, |editor, _| editor.laid_out_width());
    assert!(
        (width - 633.6).abs() < 0.75,
        "column measured {width}pt; the bundled face is not in use"
    );
}

fn record_events(
    cx: &mut VisualTestContext,
    editor: &Entity<EditorView>,
) -> (
    std::rc::Rc<std::cell::RefCell<Vec<EditorEvent>>>,
    gpui::Subscription,
) {
    let recorded = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorder = recorded.clone();
    let subscription = cx.update(|_window, cx| {
        cx.subscribe(editor, move |_editor, event, _cx| {
            recorder.borrow_mut().push(*event);
        })
    });
    (recorded, subscription)
}

#[gpui::test]
fn typed_events_separate_edits_from_movement(cx: &mut TestAppContext) {
    // the window subscribes to these instead of observing every
    // notify. Movement must never look like an edit (it would schedule
    // autosave work), and every edit path must announce itself.
    let (mut cx, editor) = editor_window(cx, "hello");
    let (recorded, _subscription) = record_events(&mut cx, &editor);
    let events =
        |recorded: &std::rc::Rc<std::cell::RefCell<Vec<EditorEvent>>>| recorded.borrow().clone();

    cx.simulate_keystrokes("end");
    cx.run_until_parked();
    assert!(
        events(&recorded).contains(&EditorEvent::SelectionChanged),
        "a caret move is announced: {:?}",
        events(&recorded)
    );
    assert!(
        !events(&recorded)
            .iter()
            .any(|event| matches!(event, EditorEvent::Edited { .. })),
        "a caret move is not an edit: {:?}",
        events(&recorded)
    );

    recorded.borrow_mut().clear();
    cx.simulate_input("!");
    cx.run_until_parked();
    let revision = editor.read_with(&cx, |editor, _| editor.revision());
    assert!(
        events(&recorded).contains(&EditorEvent::Edited { revision }),
        "typing announces its revision: {:?}",
        events(&recorded)
    );

    recorded.borrow_mut().clear();
    cx.simulate_keystrokes("left left");
    cx.run_until_parked();
    assert!(
        events(&recorded).contains(&EditorEvent::SelectionChanged),
        "movement after an edit is announced: {:?}",
        events(&recorded)
    );
    assert!(
        !events(&recorded)
            .iter()
            .any(|event| matches!(event, EditorEvent::Edited { .. })),
        "movement after an edit is not an edit: {:?}",
        events(&recorded)
    );

    recorded.borrow_mut().clear();
    cx.simulate_keystrokes(undo_chord());
    cx.run_until_parked();
    assert!(
        events(&recorded)
            .iter()
            .any(|event| matches!(event, EditorEvent::Edited { .. })),
        "undo is an edit: {:?}",
        events(&recorded)
    );

    recorded.borrow_mut().clear();
    cx.simulate_keystrokes(redo_chord());
    cx.run_until_parked();
    assert!(
        events(&recorded)
            .iter()
            .any(|event| matches!(event, EditorEvent::Edited { .. })),
        "redo is an edit: {:?}",
        events(&recorded)
    );

    recorded.borrow_mut().clear();
    cx.update(|_window, cx| {
        editor.update(cx, |editor, cx| editor.replace_document("new text", cx));
    });
    cx.run_until_parked();
    assert!(
        events(&recorded)
            .iter()
            .any(|event| matches!(event, EditorEvent::Edited { .. })),
        "a wholesale replace is an edit: {:?}",
        events(&recorded)
    );
}

#[gpui::test]
fn typed_events_announce_landed_stats(cx: &mut TestAppContext) {
    // The footer's word count only changes when the idle stats pass lands;
    // that landing is its own event so the window does not re-render for
    // the edit's paint alone and then go stale.
    let (mut cx, editor) = editor_window(cx, "one two");
    let (recorded, _subscription) = record_events(&mut cx, &editor);

    cx.simulate_keystrokes("end");
    cx.simulate_input(" three");
    assert!(
        !recorded.borrow().contains(&EditorEvent::StatsChanged),
        "stats have not landed in the keystroke's frame: {:?}",
        recorded.borrow()
    );

    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    assert!(
        recorded.borrow().contains(&EditorEvent::StatsChanged),
        "landed stats are announced: {:?}",
        recorded.borrow()
    );
    assert_eq!(editor.read_with(&cx, |editor, _| editor.stats().words), 3);
}

#[gpui::test]
fn vertical_and_page_selection_keys_preserve_and_extend_the_anchor(cx: &mut TestAppContext) {
    let (mut cx, editor) = editor_window(cx, "- line one\n- line two\n- line three");
    cx.simulate_keystrokes("home shift-down");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..11)
    );
    cx.simulate_keystrokes("shift-down");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..22)
    );
    cx.simulate_keystrokes("shift-up");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..11)
    );

    cx.simulate_keystrokes("up shift-pagedown");
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.selection()),
        Some(0..22)
    );
    cx.simulate_keystrokes("shift-pageup");
    assert_eq!(editor.read_with(&cx, |editor, _| editor.selection()), None);
}

#[gpui::test]
#[allow(clippy::reversed_empty_ranges)]
fn find_and_reading_modes_protect_document_selection_and_handle_reversed_ranges(
    cx: &mut TestAppContext,
) {
    let (mut cx, editor) = editor_window(cx, "alpha beta gamma");
    cx.dispatch_action(Find);
    cx.simulate_input("beta");
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let mut actual = None;
            let slice = editor.text_for_range(3..1, &mut actual, window, cx);
            assert_eq!(slice.as_deref(), Some("et"));
            assert_eq!(actual, Some(1..3));
            editor.set_selected_text_range(0..2, window, cx);
            assert_eq!(editor.find.as_ref().map(|f| f.cursor()), Some(2));
            assert_eq!(editor.selection(), Some(6..10));
        });
    });

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let click = ClickEvent::default();
            editor.close_find_clicked(&click, window, cx);
        });
    });
    cx.dispatch_action(Find);
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.find_query().to_owned()),
        "beta"
    );
    cx.dispatch_action(CloseFind);

    cx.dispatch_action(ToggleReading);
    let before = editor.read_with(&cx, |editor, _| (editor.caret(), editor.selection()));
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(0..5, window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| (editor.caret(), editor.selection())),
        before
    );
}
