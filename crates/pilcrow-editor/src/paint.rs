//! What the window draws for the page: the view's `Render` (find bar,
//! reading page, the prose element) and `ProseElement`'s prepaint/paint,
//! which shape only the visible lines and derive the caret from the same
//! shaped line that paints. Split from `lib.rs`; the state and
//! editing logic stay in the crate root.

use super::*;

impl EditorView {
    fn reading_page(&self, window: &Window) -> impl IntoElement {
        let window_width = window.viewport_size().width / px(1.0);
        let width = responsive_page_for(window_width, self.measure_column(window)).column;
        reading::page(
            self.buffer.text(),
            width,
            &self.focus_handle,
            &self.tokens,
            &self.reading_scroll,
            self.scale,
        )
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Announce a selection move once, however it happened. Comparing
        // here covers every movement path (keys, mouse, find, undo, IME)
        // because each already notifies for repaint. The emit itself is
        // deferred out of the draw.
        if self.announced_selection != (self.caret, self.anchor) {
            self.announced_selection = (self.caret, self.anchor);
            cx.defer_in(window, |_editor, _window, cx| {
                cx.emit(EditorEvent::SelectionChanged);
            });
        }
        let find_bar = self.find.as_ref().map(|find| {
            let count = if find.query.is_empty() {
                String::new()
            } else if find.matches.is_empty() {
                "No results".to_owned()
            } else {
                format!(
                    "{}/{}",
                    find.active.map_or(0, |active| active + 1),
                    find.matches.len()
                )
            };
            let query = if find.query.is_empty() {
                SharedString::from("Find")
            } else {
                SharedString::from(find.query.clone())
            };
            let replacement = if find.replacement.is_empty() {
                SharedString::from("Replace with")
            } else {
                SharedString::from(find.replacement.clone())
            };
            let query_active = find.field == FindField::Query && find.editing;
            let replace_active = find.field == FindField::Replacement && find.editing;
            let query_color = if find.query.is_empty() {
                self.tokens.dim
            } else if query_active {
                self.tokens.ink
            } else {
                self.tokens.mid
            };
            let replacement_color = if find.replacement.is_empty() {
                self.tokens.dim
            } else if replace_active {
                self.tokens.ink
            } else {
                self.tokens.mid
            };
            div()
                .id("find-bar")
                .debug_selector(|| "find-bar".into())
                .occlude()
                .font_family(BODY_FONT)
                .flex()
                .items_center()
                .gap_2()
                .h(px(40.0))
                .px(px(20.0))
                .text_size(px(12.0))
                .bg(self.tokens.code_bg)
                .text_color(self.tokens.mid)
                .child(
                    div()
                        .id("find-query")
                        .debug_selector(|| "find-query".into())
                        .flex_1()
                        .min_w(px(80.0))
                        .px_2()
                        .py_1()
                        .rounded(px(3.0))
                        .bg(self.tokens.paper)
                        .border_1()
                        .border_color(if query_active {
                            self.tokens.accent
                        } else {
                            self.tokens.rule
                        })
                        .cursor(CursorStyle::IBeam)
                        .text_color(query_color)
                        .child(query)
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::focus_find_query)),
                )
                .child(
                    div()
                        .debug_selector(|| "find-count".into())
                        .min_w(px(64.0))
                        .child(SharedString::from(count)),
                )
                .child(
                    div()
                        .id("find-prev")
                        .cursor_pointer()
                        .child("↑")
                        .on_click(cx.listener(Self::find_previous_clicked)),
                )
                .child(
                    div()
                        .id("find-next")
                        .cursor_pointer()
                        .child("↓")
                        .on_click(cx.listener(Self::find_next_clicked)),
                )
                .child(
                    div()
                        .id("find-case")
                        .debug_selector(|| "find-case".into())
                        .cursor_pointer()
                        .text_color(if find.case_sensitive {
                            self.tokens.accent
                        } else {
                            self.tokens.mid
                        })
                        .child("Aa")
                        .on_click(cx.listener(Self::toggle_find_case)),
                )
                .child(
                    div()
                        .id("find-replacement")
                        .debug_selector(|| "find-replacement".into())
                        .flex_1()
                        .min_w(px(80.0))
                        .px_2()
                        .py_1()
                        .rounded(px(3.0))
                        .bg(self.tokens.paper)
                        .border_1()
                        .border_color(if replace_active {
                            self.tokens.accent
                        } else {
                            self.tokens.rule
                        })
                        .cursor(CursorStyle::IBeam)
                        .text_color(replacement_color)
                        .child(replacement)
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::focus_find_replace)),
                )
                .child(
                    div()
                        .id("find-replace-one")
                        .debug_selector(|| "find-replace-one".into())
                        .cursor_pointer()
                        .text_color(self.tokens.accent)
                        .child("Replace")
                        .on_click(cx.listener(Self::replace_one)),
                )
                .child(
                    div()
                        .id("find-replace-all")
                        .debug_selector(|| "find-replace-all".into())
                        .cursor_pointer()
                        .text_color(self.tokens.accent)
                        .child("All")
                        .on_click(cx.listener(Self::replace_all)),
                )
                .child(
                    div()
                        .id("find-close")
                        .debug_selector(|| "find-close".into())
                        .cursor_pointer()
                        .child("×")
                        .on_click(cx.listener(Self::close_find_clicked)),
                )
        });

        let mut root = div()
            .id("editor")
            .key_context("Editor")
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::insert_newline))
            .on_action(cx.listener(Self::shift_enter))
            .on_action(cx.listener(Self::insert_tab))
            .on_action(cx.listener(Self::switch_find_field_action))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::format_bold))
            .on_action(cx.listener(Self::format_italic))
            .on_action(cx.listener(Self::format_strike))
            .on_action(cx.listener(Self::format_code))
            .on_action(cx.listener(Self::format_link))
            .on_action(cx.listener(Self::heading_1))
            .on_action(cx.listener(Self::heading_2))
            .on_action(cx.listener(Self::heading_3))
            .on_action(cx.listener(Self::heading_4))
            .on_action(cx.listener(Self::heading_5))
            .on_action(cx.listener(Self::heading_6))
            .on_action(cx.listener(Self::bullet_list))
            .on_action(cx.listener(Self::ordered_list))
            .on_action(cx.listener(Self::task_list))
            .on_action(cx.listener(Self::quote))
            .on_action(cx.listener(Self::toggle_focus))
            .on_action(cx.listener(Self::focus_paragraph))
            .on_action(cx.listener(Self::focus_sentence))
            .on_action(cx.listener(Self::toggle_typewriter))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::backspace_word))
            .on_action(cx.listener(Self::delete_word_forward))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::select_page_up))
            .on_action(cx.listener(Self::select_page_down))
            .on_action(cx.listener(Self::delete_line_start))
            .on_action(cx.listener(Self::delete_line_end))
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_previous))
            .on_action(cx.listener(Self::close_find))
            .on_action(cx.listener(Self::toggle_reading))
            .on_action(cx.listener(Self::toggle_task_action))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_scroll(cx.listener(Self::on_scroll));
        if let Some(find_bar) = find_bar {
            root = root.child(find_bar);
        }
        if self.reading {
            root.child(self.reading_page(window))
        } else {
            let a11y_text = if window.is_a11y_active() {
                self.buffer.text().to_owned()
            } else {
                String::new()
            };
            let a11y_caret = self.caret;
            let a11y_anchor = self.anchor;
            let editor_for_text = cx.entity();
            let editor_for_selection = editor_for_text.clone();
            let editor_for_replace = editor_for_text.clone();
            let editor_for_value = editor_for_text.clone();
            let text_for_children = a11y_text.clone();
            root.child(
                div()
                    .id("page-column")
                    .debug_selector(|| "prose-input".into())
                    .cursor(CursorStyle::IBeam)
                    .role(Role::MultilineTextInput)
                    .aria_label("Note")
                    .aria_value(SharedString::from(a11y_text))
                    .track_focus(&self.focus_handle)
                    .a11y_synthetic_children(move |builder| {
                        a11y::publish_text_run(
                            builder,
                            &text_for_children,
                            a11y_caret,
                            a11y_anchor,
                        );
                    })
                    .on_a11y_action(
                        accesskit::Action::SetTextSelection,
                        move |data, window, cx| {
                            let Some(accesskit::ActionData::SetTextSelection(selection)) = data
                            else {
                                return;
                            };
                            let anchor = selection.anchor.character_index;
                            let focus = selection.focus.character_index;
                            editor_for_selection.update(cx, |editor, cx| {
                                editor.set_accessibility_selection(anchor, focus, window, cx);
                            });
                        },
                    )
                    .on_a11y_action(
                        accesskit::Action::ReplaceSelectedText,
                        move |data, window, cx| {
                            let Some(accesskit::ActionData::Value(text)) = data else {
                                return;
                            };
                            let text = text.to_string();
                            editor_for_replace.update(cx, |editor, cx| {
                                editor.replace_accessibility_selection(&text, window, cx);
                            });
                        },
                    )
                    .on_a11y_action(accesskit::Action::SetValue, move |data, window, cx| {
                        let Some(accesskit::ActionData::Value(text)) = data else {
                            return;
                        };
                        let text = text.to_string();
                        editor_for_value.update(cx, |editor, cx| {
                            editor.set_accessibility_value(&text, window, cx);
                        });
                    })
                    .flex_1()
                    .min_h(px(0.0))
                    .child(ProseElement {
                        editor: editor_for_text,
                    }),
            )
        }
    }
}

struct ProseElement {
    editor: Entity<EditorView>,
}

impl IntoElement for ProseElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ProseElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let viewport = ((bounds.size.height - HEAD_PX) / px(1.0)).max(0.0);
        let window_width = bounds.size.width / px(1.0);
        let column = self.editor.update(cx, |editor, _cx| {
            if editor.trace_active {
                editor.trace_frame_started = Some(Instant::now());
            }
            editor.ensure_layout(window, window_width, viewport)
        });
        let editor = self.editor.read(cx);
        let text = editor.buffer.text();
        let selection = editor.selection();
        let focus = editor.focus_view();
        let layout = editor
            .layout
            .as_ref()
            .expect("ensure_layout always installs a layout");
        let page = responsive_page_for(window_width, column);
        let page_left = bounds.left() + px(page.margin.max(layout.max_hang()));
        // The scrollbar's extent is the estimated document height, so it
        // reaches the end of a long note before the frontier does; the
        // exact `covered` check below still gates frame requests on
        // placed lines only.
        let max_scroll =
            editor.max_scroll_for(editor.estimated_total.max(layout.total()), viewport);
        let mut scroll = editor.scroll.clamp(0.0, max_scroll);
        let resized = editor.last_bounds.is_some_and(|last| {
            last.size.width != bounds.size.width || last.size.height != bounds.size.height
        });
        if (resized || editor.reveal_pending)
            && let Some((y, h)) = editor.caret_band(layout)
        {
            if y < scroll {
                scroll = y;
            } else if y + h > scroll + viewport {
                scroll = y + h - viewport;
            }
            scroll = scroll.clamp(0.0, max_scroll);
        }
        if editor.typewriter
            && (resized || editor.reveal_pending)
            && let Some((y, h)) = editor.caret_band(layout)
        {
            scroll = (y + h * 0.5 - viewport * 0.5).clamp(0.0, max_scroll);
        }

        let caret_block = block_index_for_offset(&editor.blocks, editor.caret);
        let caret_ready = layout.done() || layout.next_block() > caret_block;
        let covered = layout.done() || layout.total() >= scroll + viewport * 2.0;

        let first = layout.first_at((scroll - RUN_UP).max(0.0));
        let last_y = scroll + viewport + RUN_DOWN;
        let mut lines = Vec::new();
        let mut search_quads = Vec::new();
        let mut spell_underlines = Vec::new();
        let mut selection_quads = Vec::new();
        let mut inline_code_quads = Vec::new();
        let mut code_backgrounds = Vec::new();
        let mut rules = Vec::new();
        let mut task_hits = Vec::new();
        let cache = editor.shape_cache.clone();
        let mut cache = cache.borrow_mut();
        if cache.revision != editor.buffer.revision()
            || cache.width != column
            || cache.scale != editor.scale
            || cache.tokens != Some(editor.tokens)
            || cache.marked != editor.marked
            || cache.focus != focus
        {
            cache.lines.clear();
            cache.revision = editor.buffer.revision();
            cache.width = column;
            cache.scale = editor.scale;
            cache.tokens = Some(editor.tokens);
            cache.marked = editor.marked.clone();
            cache.focus = focus;
        }

        // Inline kinds and link ranges are per block, not per visual line.
        // A wrapped paragraph asks for the same array on every line it spans.
        let mut kinds_cache: HashMap<usize, Rc<Vec<InlineKind>>> = HashMap::new();
        let mut link_cache: HashMap<usize, Rc<Vec<Range<usize>>>> = HashMap::new();

        for (index, placed) in layout.lines().iter().enumerate().skip(first) {
            if placed.y > last_y {
                break;
            }
            let block_index = placed.block as usize;
            let Some(block) = editor.blocks.get(block_index).copied() else {
                continue;
            };
            let block_start = block.start;
            let abs_start = block_start + placed.start as usize;
            let abs_end = block_start + placed.end as usize;
            let line_top = bounds.top() + HEAD_PX + px(placed.y) - px(scroll);
            let line_bounds = Bounds::new(
                point(page_left + px(placed.x), line_top),
                size(px(column), px(placed.h)),
            );

            let shaped = if placed.rule {
                ShapedLine::default()
            } else {
                let key = LineKey {
                    block: placed.block,
                    start: placed.start,
                    end: placed.end,
                };
                if let Some(shaped) = cache.lines.get(&key) {
                    shaped.clone()
                } else {
                    let kinds = kinds_cache
                        .entry(block_index)
                        .or_insert_with(|| {
                            let full = &text[block.start..block.end];
                            let mut kinds = vec![InlineKind::Plain; full.len()];
                            if block.kind != BlockKind::Code {
                                inline_kinds(full, &mut kinds);
                            }
                            apply_block_markers(block, full, &mut kinds);
                            Rc::new(kinds)
                        })
                        .clone();
                    let links = link_cache
                        .entry(block_index)
                        .or_insert_with(|| {
                            let full = &text[block.start..block.end];
                            Rc::new(if block.kind == BlockKind::Code {
                                Vec::new()
                            } else {
                                link_text_ranges(full)
                            })
                        })
                        .clone();
                    let (line_text, runs) = text_runs_for_line(
                        text,
                        block,
                        *placed,
                        Some(&kinds),
                        &links,
                        editor.marked.as_ref(),
                        focus,
                        &editor.tokens,
                        editor.scale,
                    );
                    let shaped = window.text_system().shape_line(
                        line_text,
                        px(style::look_scaled(block, editor.scale).size),
                        &runs,
                        None,
                    );
                    if cache.lines.len() > 1024 {
                        cache.lines.clear();
                    }
                    cache.lines.insert(key, shaped.clone());
                    shaped
                }
            };

            if !placed.rule
                && block.kind != BlockKind::Code
                && text.as_bytes()[block.start..block.end].contains(&b'`')
            {
                let kinds = kinds_cache
                    .entry(block_index)
                    .or_insert_with(|| {
                        let full = &text[block.start..block.end];
                        let mut kinds = vec![InlineKind::Plain; full.len()];
                        inline_kinds(full, &mut kinds);
                        apply_block_markers(block, full, &mut kinds);
                        Rc::new(kinds)
                    })
                    .clone();
                let local_start = placed.start as usize;
                let local_end = placed.end as usize;
                let mut i = local_start;
                while i < local_end {
                    if kinds.get(i).copied() == Some(InlineKind::Code) {
                        let run_start = i;
                        while i < local_end && kinds.get(i).copied() == Some(InlineKind::Code) {
                            i += 1;
                        }
                        let x0 = shaped.x_for_index(run_start - local_start);
                        let x1 = shaped.x_for_index(i - local_start);
                        inline_code_quads.push(
                            fill(
                                Bounds::new(
                                    point(
                                        line_bounds.left() + x0 - px(2.0),
                                        line_bounds.top() + px(2.0),
                                    ),
                                    size(
                                        (x1 - x0).max(px(1.0)) + px(4.0),
                                        (px(placed.h) - px(4.0)).max(px(1.0)),
                                    ),
                                ),
                                editor.tokens.code_bg,
                            )
                            .corner_radii(px(3.0)),
                        );
                    } else {
                        i += 1;
                    }
                }
            }

            if !placed.rule
                && let Some(task) = task_visual(block, &text[block.start..block.end])
                && abs_start <= block.start + task.box_start
                && block.start + task.box_end <= abs_end
            {
                // Keep `[ ]` / `[x]` as ordinary source text. The marker is
                // still a click target for toggling, but no glyph replaces it.
                let local_start = block.start + task.box_start - abs_start;
                let local_end = block.start + task.box_end - abs_start;
                let x0 = shaped.x_for_index(local_start);
                let x1 = shaped.x_for_index(local_end);
                task_hits.push(TaskBox {
                    bounds: Bounds::new(
                        point(line_bounds.left() + x0, line_top),
                        size((x1 - x0).max(px(1.0)), px(placed.h)),
                    ),
                    start: block.start + task.box_start,
                    end: block.start + task.box_end,
                });
            }

            if let Some(find) = editor.find.as_ref()
                && !placed.rule
                && !find.matches.is_empty()
            {
                let first_match = find.matches.partition_point(|range| range.end <= abs_start);
                for (match_index, range) in find.matches.iter().enumerate().skip(first_match) {
                    if range.start >= abs_end {
                        break;
                    }
                    let from = range.start.max(abs_start);
                    let to = range.end.min(abs_end);
                    if from >= to {
                        continue;
                    }
                    let x0 = shaped.x_for_index(from - abs_start);
                    let x1 = shaped.x_for_index(to - abs_start);
                    let color = if Some(match_index) == find.active {
                        Hsla {
                            a: 0.42,
                            ..editor.tokens.accent
                        }
                    } else {
                        Hsla {
                            a: 0.18,
                            ..editor.tokens.accent
                        }
                    };
                    search_quads.push(fill(
                        Bounds::new(
                            point(line_bounds.left() + x0, line_bounds.top()),
                            size((x1 - x0).max(px(1.0)), px(placed.h)),
                        ),
                        color,
                    ));
                }
            }

            if !placed.rule && !editor.spell.is_empty() {
                let first_spell = editor.spell.partition_point(|range| range.end <= abs_start);
                for range in editor.spell.iter().skip(first_spell) {
                    if range.start >= abs_end {
                        break;
                    }
                    let from = range.start.max(abs_start);
                    let to = range.end.min(abs_end);
                    if from >= to {
                        continue;
                    }
                    let x0 = shaped.x_for_index(from - abs_start);
                    let x1 = shaped.x_for_index(to - abs_start);
                    spell_underlines.push((
                        point(line_bounds.left() + x0, line_bounds.bottom() - px(2.0)),
                        (x1 - x0).max(px(1.0)),
                    ));
                }
            }

            if let Some(selection) = selection.clone()
                && !placed.rule
            {
                let from = selection.start.max(abs_start);
                let to = selection.end.min(abs_end);
                if from < to {
                    let x0 = shaped.x_for_index(from - abs_start);
                    let x1 = shaped.x_for_index(to - abs_start);
                    selection_quads.push(fill(
                        Bounds::new(
                            point(line_bounds.left() + x0, line_bounds.top()),
                            size((x1 - x0).max(px(1.0)), px(placed.h)),
                        ),
                        editor.tokens.selection,
                    ));
                }
            }

            if block.kind == BlockKind::Code {
                let is_first = (block_index == 0
                    || editor.blocks[block_index - 1].kind != BlockKind::Code)
                    && placed.start == 0;
                let is_last = (block_index + 1 == editor.blocks.len()
                    || editor.blocks[block_index + 1].kind != BlockKind::Code)
                    && block.start + placed.end as usize >= block.end.saturating_sub(1);
                let top = if is_first {
                    line_top - px(4.0)
                } else {
                    line_top
                };
                let extra_h =
                    px(if is_first { 4.0 } else { 0.0 }) + px(if is_last { 4.0 } else { 0.0 });
                let corners = gpui::Corners {
                    top_left: px(if is_first { 4.0 } else { 0.0 }),
                    top_right: px(if is_first { 4.0 } else { 0.0 }),
                    bottom_left: px(if is_last { 4.0 } else { 0.0 }),
                    bottom_right: px(if is_last { 4.0 } else { 0.0 }),
                };
                code_backgrounds.push(
                    fill(
                        Bounds::new(
                            point(page_left - px(12.0), top),
                            size(px(column) + px(24.0), px(placed.h) + extra_h),
                        ),
                        editor.tokens.code_bg,
                    )
                    .corner_radii(corners),
                );
            }
            if placed.rule {
                rules.push(fill(
                    Bounds::new(point(page_left, line_top), size(px(column), px(1.0))),
                    editor.tokens.rule,
                ));
            }

            lines.push(LineGeometry {
                index,
                block: block_index,
                start: abs_start,
                end: abs_end,
                doc_y: placed.y,
                shaped,
                bounds: line_bounds,
                h: placed.h,
                rule: placed.rule,
            });
        }

        let caret = if selection.is_some() {
            None
        } else {
            editor
                .caret_bounds_in(&lines, bounds, scroll, page_left)
                .map(|logical| {
                    let optical_h = logical.size.height * (1.22 / LEADING);
                    let pad = (logical.size.height - optical_h) * 0.5;
                    fill(
                        Bounds::new(
                            point(logical.origin.x, logical.origin.y + pad),
                            size(CARET_WIDTH, optical_h),
                        ),
                        editor.tokens.accent,
                    )
                })
        };

        Self::PrepaintState {
            lines,
            search: search_quads,
            spell: spell_underlines,
            selection: selection_quads,
            caret,
            inline_code_quads,
            code_backgrounds,
            rules,
            task_hits,
            scroll,
            page,
            caret_ready,
            covered,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Window-level move/up listeners keep a drag alive over the status bar
        // and release the selection even when the pointer leaves the page.
        // No GPUI hitbox capture on top: at this GPUI revision capture is
        // in-window routing only, and every platform already delivers drag
        // and release outside the frame to the press window (AppKit drag
        // session, X11/Wayland implicit grab, SetCapture on Windows). The
        // drag state machine heals the rest — a missed up or a focus loss
        // ends the drag instead of sticking.
        let move_editor = self.editor.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                move_editor.update(cx, |editor, cx| editor.on_mouse_move(event, window, cx));
            }
        });
        let up_editor = self.editor.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                up_editor.update(cx, |editor, cx| editor.on_mouse_up(event, window, cx));
            }
        });
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        let code_boxes = prepaint
            .code_backgrounds
            .iter()
            .map(|quad| quad.bounds)
            .collect();
        let rule_boxes = prepaint.rules.iter().map(|quad| quad.bounds).collect();
        let page_top = bounds.top() + HEAD_PX;
        let page_height = if bounds.size.height > HEAD_PX {
            bounds.size.height - HEAD_PX
        } else {
            px(0.0)
        };
        let page_bounds = Bounds::new(
            point(bounds.left(), page_top),
            size(bounds.size.width, page_height),
        );
        let in_page =
            |rect: Bounds<Pixels>| rect.bottom() > page_top && rect.top() < bounds.bottom();
        let spell_color = self.editor.read(cx).tokens.spell;
        window.with_content_mask(
            Some(ContentMask {
                bounds: page_bounds,
            }),
            |window| {
                for quad in prepaint.code_backgrounds.drain(..) {
                    if in_page(quad.bounds) {
                        window.paint_quad(quad);
                    }
                }
                for quad in prepaint.inline_code_quads.drain(..) {
                    if in_page(quad.bounds) {
                        window.paint_quad(quad);
                    }
                }
                for quad in prepaint.search.drain(..) {
                    if in_page(quad.bounds) {
                        window.paint_quad(quad);
                    }
                }
                for (origin, width) in prepaint.spell.drain(..) {
                    if origin.y > page_top && origin.y < bounds.bottom() {
                        window.paint_underline(
                            origin,
                            width,
                            &UnderlineStyle {
                                thickness: px(1.5),
                                color: Some(spell_color),
                                wavy: true,
                            },
                        );
                    }
                }
                for quad in prepaint.selection.drain(..) {
                    if in_page(quad.bounds) {
                        window.paint_quad(quad);
                    }
                }

                for line in &prepaint.lines {
                    if line.rule || !in_page(line.bounds) {
                        continue;
                    }
                    line.shaped
                        .paint(
                            line.bounds.origin,
                            px(line.h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        )
                        .expect("paint shaped line");
                }

                for quad in prepaint.rules.drain(..) {
                    if in_page(quad.bounds) {
                        window.paint_quad(quad);
                    }
                }

                if focus_handle.is_focused(window)
                    && let Some(caret) = prepaint.caret.take()
                {
                    window.paint_quad(caret);
                }
            },
        );

        let lines = std::mem::take(&mut prepaint.lines);
        let task_hits = std::mem::take(&mut prepaint.task_hits);
        let scroll = prepaint.scroll;
        let page = prepaint.page;
        let caret_ready = prepaint.caret_ready;
        let covered = prepaint.covered;
        self.editor.update(cx, |editor, _cx| {
            editor.scroll = scroll;
            editor.last_bounds = Some(bounds);
            editor.last_page = Some(page);
            editor.last_layout_width = page.column;
            editor.last_lines = lines;
            editor.last_code_boxes = code_boxes;
            editor.last_rule_boxes = rule_boxes;
            editor.last_task_boxes = task_hits;
            editor.last_revision = editor.buffer.revision();
            editor.last_scroll = scroll;
            editor.frame_seq = editor.frame_seq.wrapping_add(1);
            editor.record_painted_frame();
            if caret_ready {
                editor.reveal_pending = false;
            }
        });
        // Layout is lazy: only a caret outside the placed frontier or a
        // viewport without run-up asks for another frame. A long unfinished
        // document must not pin a full-window animation loop.
        if !caret_ready || !covered {
            window.request_animation_frame();
        }
    }
}
