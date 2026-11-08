//! The platform text-input surface: GPUI's `EntityInputHandler` for the
//! page - UTF-16 ranges, marked (IME) text, and replacement. Every byte
//! change funnels into `replace_range` or the composition helpers in the
//! crate root, so undo grouping and `EditorEvent::Edited` stay in one
//! place. Split from `lib.rs`.

use super::*;

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        if self.reading {
            return None;
        }
        if let Some(find) = self.find.as_ref().filter(|find| find.editing) {
            let value = find.value();
            let len = value.encode_utf16().count();
            let mut start = Self::offset_from_utf16(value, range_utf16.start.min(len));
            let mut end = Self::offset_from_utf16(value, range_utf16.end.min(len));
            if start > end {
                std::mem::swap(&mut start, &mut end);
            }
            actual_range
                .replace(Self::offset_to_utf16(value, start)..Self::offset_to_utf16(value, end));
            return Some(value[start..end].to_owned());
        }
        let range = self.byte_range_from_utf16(range_utf16);
        actual_range.replace(
            Self::offset_to_utf16(self.buffer.text(), range.start)
                ..Self::offset_to_utf16(self.buffer.text(), range.end),
        );
        Some(self.buffer.text()[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if let Some(find) = self.find.as_ref().filter(|find| find.editing) {
            let cursor = Self::offset_to_utf16(find.value(), find.cursor());
            return Some(UTF16Selection {
                range: cursor..cursor,
                reversed: false,
            });
        }
        let start = self.selection_start();
        let end = self.selection_end();
        Some(UTF16Selection {
            range: Self::offset_to_utf16(self.buffer.text(), start)
                ..Self::offset_to_utf16(self.buffer.text(), end),
            reversed: self.anchor.is_some_and(|anchor| anchor > self.caret),
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reading {
            return;
        }
        if let Some(find) = self.find.as_mut().filter(|find| find.editing) {
            let value = find.value();
            let len = value.encode_utf16().count();
            let cursor = Self::offset_from_utf16(value, range_utf16.end.min(len));
            *find.cursor_mut() = cursor;
            window.notify_text_selection_changed(&self.focus_handle);
            cx.notify();
            return;
        }
        let range = self.byte_range_from_utf16(range_utf16);
        self.commit_composition(window);
        self.history.break_group();
        self.anchor = (range.start < range.end).then_some(range.start);
        self.caret = range.end;
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if let Some(find) = self.find.as_ref().filter(|find| find.editing) {
            return Some(find.value().encode_utf16().count());
        }
        Some(self.buffer.text().encode_utf16().count())
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            return None;
        }
        self.marked.as_ref().map(|range| {
            Self::offset_to_utf16(self.buffer.text(), range.start)
                ..Self::offset_to_utf16(self.buffer.text(), range.end)
        })
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            cx.notify();
            return;
        }
        let had_composition = self.composition.is_some() || self.marked.is_some();
        let changed = self.finish_composition();
        self.marked = None;
        self.history.break_group();
        if had_composition {
            window.notify_text_input_changed(&self.focus_handle);
        }
        if changed {
            window.notify_text_selection_changed(&self.focus_handle);
        }
        cx.notify();
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.reading {
            return;
        }
        if self.find.as_ref().is_some_and(|find| find.editing) {
            if let Some(text) = item.text() {
                self.insert_find_text(&text, window, cx);
            }
            return;
        }
        if let Some(text) = item.text() {
            self.paste_text(&text, window, cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reading {
            return;
        }
        if self.find.as_ref().is_some_and(|find| find.editing) {
            if text.is_empty() {
                if range_utf16.is_some() {
                    self.backspace_find(window, cx);
                }
            } else {
                self.insert_find_text(text, window, cx);
            }
            return;
        }
        let text = text.replace("\r\n", "\n");

        // A second call after a commit means the platform skipped unmark.
        // Record the composition before treating this as a new edit.
        if self
            .composition
            .as_ref()
            .is_some_and(|range| range.committed)
        {
            let changed = self.finish_composition();
            if changed {
                window.notify_text_selection_changed(&self.focus_handle);
            }
        }

        if self.composition.is_some() {
            // This is the commit of the live composition. Keep the composition
            // until unmark so the preedit and its final text stay one step.
            if text.is_empty() {
                // Linux IMEs send an empty replacement for DeleteText. An
                // explicit range (or the marked range) is a deletion of the
                // preedit, not a cancel; only an unranged empty replacement is
                // the cancel path.
                let deletion = range_utf16
                    .map(|range| self.byte_range_from_utf16(range))
                    .or_else(|| self.marked.clone());
                if let Some(range) = deletion {
                    let mut start = boundary_at_or_before(self.buffer.text(), range.start);
                    let mut end = boundary_at_or_before(self.buffer.text(), range.end);
                    if start > end {
                        std::mem::swap(&mut start, &mut end);
                    }
                    let old_revision = self.buffer.revision();
                    let removed = end - start;
                    self.caret = self.buffer.replace(start..end, "");
                    if let Some(composition) = self.composition.as_mut() {
                        composition.current_len = composition.current_len.saturating_sub(removed);
                        composition.committed = false;
                    }
                    self.invalidate_after_edit(old_revision, start, start, cx);
                    self.anchor = None;
                    self.marked = None;
                    self.desired_x = None;
                    self.reveal_caret();
                    window.notify_text_input_changed(&self.focus_handle);
                    window.notify_text_selection_changed(&self.focus_handle);
                    cx.notify();
                    return;
                }
                self.cancel_composition(cx);
                self.history.break_group();
                self.desired_x = None;
                self.reveal_caret();
                window.notify_text_input_changed(&self.focus_handle);
                window.notify_text_selection_changed(&self.focus_handle);
                cx.notify();
                return;
            }
            let (start, end) = self
                .composition
                .as_ref()
                .map(|composition| {
                    range_utf16
                        .map(|range| self.byte_range_from_utf16(range))
                        .or_else(|| self.marked.clone())
                        .unwrap_or_else(|| {
                            composition.start
                                ..composition.start.saturating_add(composition.current_len)
                        })
                })
                .map(|range| {
                    let mut start = boundary_at_or_before(self.buffer.text(), range.start);
                    let mut end = boundary_at_or_before(self.buffer.text(), range.end);
                    if start > end {
                        std::mem::swap(&mut start, &mut end);
                    }
                    (start, end)
                })
                .unwrap_or((0, 0));
            let old_revision = self.buffer.revision();
            self.caret = self.buffer.replace(start..end, &text);
            if let Some(composition) = self.composition.as_mut() {
                composition.current_len = composition
                    .current_len
                    .saturating_add(text.len())
                    .saturating_sub(end - start);
                composition.committed = true;
            }
            self.invalidate_after_edit(old_revision, start, start + text.len(), cx);
            self.anchor = None;
            self.marked = None;
            self.desired_x = None;
            self.reveal_caret();
            window.notify_text_input_changed(&self.focus_handle);
            window.notify_text_selection_changed(&self.focus_handle);
            cx.notify();
            return;
        }

        if text.is_empty() {
            if let Some(range) = range_utf16 {
                let range = self.byte_range_from_utf16(range);
                self.replace_range(range, "", EditGroup::Delete, window, cx);
            }
            return;
        }
        let range = range_utf16
            .map(|range| self.byte_range_from_utf16(range))
            .unwrap_or_else(|| self.selection_start()..self.selection_end());
        self.replace_range(range, &text, EditGroup::Typing, window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reading {
            return;
        }
        if self.find.as_ref().is_some_and(|find| find.editing) {
            if new_text.is_empty() {
                self.backspace_find(window, cx);
            } else {
                self.insert_find_text(new_text, window, cx);
            }
            return;
        }
        let new_text = new_text.replace("\r\n", "\n");
        let (range_utf16, new_selected_range) = (_range_utf16, _new_selected_range);

        if self
            .composition
            .as_ref()
            .is_some_and(|range| range.committed)
        {
            self.finish_composition();
        }

        if new_text.is_empty() {
            if self.composition.is_some() {
                // An empty mark is a cancel: put the page back exactly as it
                // was and record nothing.
                self.cancel_composition(cx);
            } else {
                self.marked = None;
            }
            self.history.break_group();
            self.desired_x = None;
            self.reveal_caret();
            window.notify_text_input_changed(&self.focus_handle);
            window.notify_text_selection_changed(&self.focus_handle);
            cx.notify();
            return;
        }

        let mut range = range_utf16
            .map(|range| self.byte_range_from_utf16(range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection_start()..self.selection_end());
        if range.start > range.end {
            std::mem::swap(&mut range.start, &mut range.end);
        }
        let mut start = boundary_at_or_before(self.buffer.text(), range.start);
        let mut end = boundary_at_or_before(self.buffer.text(), range.end);
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }

        let selection_before = self.selection_snapshot();
        if self.composition.is_none() {
            self.composition = Some(Composition {
                start,
                original: self.buffer.text()[start..end].to_owned(),
                current_len: end - start,
                selection_before,
                committed: false,
            });
        }

        let old_revision = self.buffer.revision();
        self.caret = self.buffer.replace(start..end, &new_text);
        if let Some(composition) = self.composition.as_mut() {
            composition.current_len = composition
                .current_len
                .saturating_add(new_text.len())
                .saturating_sub(end - start);
            composition.committed = false;
        }
        self.invalidate_after_edit(old_revision, start, start + new_text.len(), cx);
        self.anchor = None;
        self.marked = Some(start..start + new_text.len());
        if let Some(selected) = new_selected_range {
            let selected_start = grapheme_at_or_before(
                &new_text,
                Self::offset_from_utf16(&new_text, selected.start),
            );
            let selected_end =
                grapheme_at_or_after(&new_text, Self::offset_from_utf16(&new_text, selected.end));
            let selected_start = start + selected_start;
            let selected_end = start + selected_end;
            self.anchor = (selected_start < selected_end).then_some(selected_start);
            self.caret = selected_end;
        }
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_input_changed(&self.focus_handle);
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if !self.layout_is_current() {
            return None;
        }
        let range = self.byte_range_from_utf16(range_utf16);
        let Some(line) = line_geometry_for_offset(&self.blocks, &self.last_lines, range.start)
        else {
            // A blank line or rule has no shaped geometry; the candidate
            // window still belongs at the block's top.
            let layout = self.layout.as_ref()?;
            let bounds = self.last_bounds?;
            let block_index = block_index_for_offset(&self.blocks, range.start);
            let block = self.blocks.get(block_index)?;
            let y = layout.block_top(block_index)?;
            let top = bounds.top() + HEAD_PX + px(y) - px(self.scroll);
            let h = style::line_height(style::look_scaled(*block, self.scale));
            if top + px(h) <= bounds.top() + HEAD_PX || top >= bounds.bottom() {
                return None;
            }
            return Some(Bounds::new(
                point(self.page_left(bounds), top),
                size(CARET_WIDTH, px(h)),
            ));
        };
        if !self.in_viewport(line) {
            return None;
        }
        let local = range
            .start
            .saturating_sub(line.start)
            .min(line.end - line.start);
        let x = line.shaped.x_for_index(local);
        Some(Bounds::new(
            point(line.bounds.left() + x, line.bounds.top()),
            size(CARET_WIDTH, px(line.h)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.hit_test_offset(point)?;
        Some(Self::offset_to_utf16(self.buffer.text(), offset))
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        if !self.layout_is_current() {
            return Vec::new();
        }
        let range = self.byte_range_from_utf16(range_utf16);
        let mut quads = Vec::new();
        for line in self.last_lines.iter().filter(|line| !line.rule) {
            let from = range.start.max(line.start);
            let to = range.end.min(line.end);
            if from >= to {
                continue;
            }
            let x0 = line.shaped.x_for_index(from - line.start);
            let x1 = line.shaped.x_for_index(to - line.start);
            quads.push(Bounds::new(
                point(line.bounds.left() + x0, line.bounds.top()),
                size((x1 - x0).max(px(1.0)), px(line.h)),
            ));
        }
        quads
    }
}
