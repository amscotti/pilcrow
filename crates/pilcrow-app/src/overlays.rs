//! Modal overlays for the window shell: the Command Palette and the local
//! Browse Versions history browser.

use std::any::TypeId;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    Action, AnyElement, App, Context, FocusHandle, FontWeight, KeyBinding, KeyDownEvent,
    MouseDownEvent, OwnedMenu, OwnedMenuItem, Role, SharedString, Window, div, px,
};
use pilcrow_commands::app_actions::ToggleCommandPalette;
use pilcrow_commands::menus;

use crate::{NoteWindow, app_data_root, backups, file, menu_options_for};

pub(crate) struct PaletteItem {
    pub(crate) category: String,
    pub(crate) label: String,
    pub(crate) shortcut: String,
    pub(crate) checked: bool,
    pub(crate) action: Box<dyn Action>,
}

pub(crate) struct CommandPaletteState {
    pub(crate) query: String,
    pub(crate) selected: usize,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) items: Vec<PaletteItem>,
}

impl CommandPaletteState {
    pub(crate) fn filtered_indices(&self) -> Vec<usize> {
        let tokens: Vec<String> = self
            .query
            .split_whitespace()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                if tokens.is_empty() {
                    return true;
                }
                let haystack = format!("{} {}", item.category, item.label).to_ascii_lowercase();
                tokens.iter().all(|tok| haystack.contains(tok))
            })
            .map(|(index, _)| index)
            .collect()
    }
}

pub(crate) struct VersionEntry {
    pub(crate) label: String,
    pub(crate) summary: String,
    pub(crate) preview: Option<String>,
    pub(crate) error: Option<String>,
}

pub(crate) struct VersionsModalState {
    pub(crate) note_title: String,
    pub(crate) has_path: bool,
    pub(crate) loading: bool,
    pub(crate) entries: Vec<VersionEntry>,
    pub(crate) selected: usize,
    pub(crate) focus_handle: FocusHandle,
}

fn format_shortcut(bindings: &[KeyBinding]) -> String {
    let Some(binding) = bindings.last() else {
        return String::new();
    };
    let mut out = String::new();
    for (index, ks) in binding.keystrokes().iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let mods = ks.modifiers();
        if mods.control {
            out.push_str(if cfg!(target_os = "macos") {
                "⌃"
            } else {
                "Ctrl+"
            });
        }
        if mods.alt {
            out.push_str(if cfg!(target_os = "macos") {
                "⌥"
            } else {
                "Alt+"
            });
        }
        if mods.shift {
            out.push_str(if cfg!(target_os = "macos") {
                "⇧"
            } else {
                "Shift+"
            });
        }
        if mods.platform {
            out.push_str(if cfg!(target_os = "macos") {
                "⌘"
            } else {
                "Super+"
            });
        }
        let key: &str = ks.key();
        match key {
            "up" => out.push('↑'),
            "down" => out.push('↓'),
            "left" => out.push('←'),
            "right" => out.push('→'),
            "enter" => out.push('↵'),
            "backspace" => out.push('⌫'),
            "escape" => out.push_str("Esc"),
            _ => out.push_str(&key.to_ascii_uppercase()),
        }
    }
    out
}

fn collect_menu_items(menu: &OwnedMenu, prefix: &str, window: &Window, out: &mut Vec<PaletteItem>) {
    let category = if prefix.is_empty() {
        menu.name.to_string()
    } else {
        format!("{prefix} › {}", menu.name)
    };
    for item in &menu.items {
        match item {
            OwnedMenuItem::Action {
                name,
                action,
                checked,
                disabled,
                ..
            } => {
                if *disabled || action.as_any().type_id() == TypeId::of::<ToggleCommandPalette>() {
                    continue;
                }
                let shortcut = format_shortcut(&window.bindings_for_action(action.as_ref()));
                out.push(PaletteItem {
                    category: category.clone(),
                    label: name.clone(),
                    shortcut,
                    checked: *checked,
                    action: action.boxed_clone(),
                });
            }
            OwnedMenuItem::Submenu(submenu) if !submenu.disabled => {
                collect_menu_items(submenu, &category, window, out);
            }
            _ => {}
        }
    }
}

pub(crate) fn build_palette_items(window: &Window, cx: &App) -> Vec<PaletteItem> {
    let owned_menus = cx.get_menus().unwrap_or_else(|| {
        menus::menus(&menu_options_for(cx))
            .into_iter()
            .map(gpui::Menu::owned)
            .collect()
    });
    let mut items = Vec::new();
    for menu in &owned_menus {
        if !menu.disabled {
            collect_menu_items(menu, "", window, &mut items);
        }
    }
    items
}

pub(crate) fn format_version_stamp(stamp_ms: u64, now_ms: u64) -> String {
    let elapsed_secs = now_ms.saturating_sub(stamp_ms) / 1000;
    if elapsed_secs < 60 {
        "Just now".to_owned()
    } else if elapsed_secs < 3600 {
        let mins = elapsed_secs / 60;
        if mins == 1 {
            "1 minute ago".to_owned()
        } else {
            format!("{mins} minutes ago")
        }
    } else if elapsed_secs < 86_400 {
        let hrs = elapsed_secs / 3600;
        if hrs == 1 {
            "1 hour ago".to_owned()
        } else {
            format!("{hrs} hours ago")
        }
    } else {
        let days = elapsed_secs / 86_400;
        if days == 1 {
            "1 day ago".to_owned()
        } else {
            format!("{days} days ago")
        }
    }
}

pub(crate) fn load_version_entries(root: &Path, path: &Path) -> Vec<VersionEntry> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    backups::versions(root, path)
        .into_iter()
        .take(50)
        .map(|vpath| {
            let stamp_ms = vpath
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let label = format_version_stamp(stamp_ms, now_ms);
            match file::read_bounded(&vpath, file::MAX_NOTE_BYTES) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => {
                        let words = text.split_whitespace().count();
                        let summary = format!("{words} words · {} bytes", text.len());
                        VersionEntry {
                            label,
                            summary,
                            preview: Some(text),
                            error: None,
                        }
                    }
                    Err(_) => VersionEntry {
                        label,
                        summary: "Invalid UTF-8".to_owned(),
                        preview: None,
                        error: Some("Snapshot is not valid UTF-8.".to_owned()),
                    },
                },
                Err(err) => VersionEntry {
                    label,
                    summary: "Unreadable".to_owned(),
                    preview: None,
                    error: Some(format!("{err:#}")),
                },
            }
        })
        .collect()
}

impl NoteWindow {
    pub(crate) fn toggle_command_palette(
        &mut self,
        _: &pilcrow_commands::app_actions::ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.take().is_some() {
            self.focus_editor(window, cx);
            cx.notify();
            return;
        }
        self.versions_modal = None;
        let focus_handle = cx.focus_handle();
        let items = build_palette_items(window, cx);
        window.focus(&focus_handle, cx);
        self.palette = Some(CommandPaletteState {
            query: String::new(),
            selected: 0,
            focus_handle,
            items,
        });
        cx.notify();
    }

    pub(crate) fn execute_palette_action(
        &mut self,
        filtered_pos: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = {
            let Some(palette) = self.palette.as_ref() else {
                return;
            };
            let filtered = palette.filtered_indices();
            filtered
                .get(filtered_pos)
                .and_then(|&idx| palette.items.get(idx))
                .map(|item| item.action.boxed_clone())
        };
        self.palette = None;
        self.focus_editor(window, cx);
        if let Some(action) = action {
            window.dispatch_action(action, cx);
        }
        cx.notify();
    }

    pub(crate) fn on_palette_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        if key == "escape" {
            cx.stop_propagation();
            self.palette = None;
            self.focus_editor(window, cx);
            cx.notify();
            return;
        }
        if key == "enter" {
            cx.stop_propagation();
            let selected = self.palette.as_ref().map_or(0, |p| p.selected);
            self.execute_palette_action(selected, window, cx);
            return;
        }
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        let filtered_len = palette.filtered_indices().len();
        if key == "down" || (mods.control && key == "n") {
            cx.stop_propagation();
            if filtered_len > 0 {
                palette.selected = (palette.selected + 1).min(filtered_len - 1);
                cx.notify();
            }
            return;
        }
        if key == "up" || (mods.control && key == "p") {
            cx.stop_propagation();
            palette.selected = palette.selected.saturating_sub(1);
            cx.notify();
            return;
        }
        if key == "backspace" {
            cx.stop_propagation();
            palette.query.pop();
            palette.selected = 0;
            cx.notify();
            return;
        }
        if !mods.control
            && !mods.platform
            && !mods.alt
            && let Some(ch) = &event.keystroke.key_char
        {
            cx.stop_propagation();
            palette.query.push_str(ch);
            palette.selected = 0;
            cx.notify();
        }
    }

    pub(crate) fn browse_versions(
        &mut self,
        _: &pilcrow_commands::app_actions::BrowseVersions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.versions_modal.take().is_some() {
            self.focus_editor(window, cx);
            cx.notify();
            return;
        }
        self.palette = None;
        let focus_handle = cx.focus_handle();
        let note_title = self.title(cx);
        let Some(path) = self.path.clone() else {
            window.focus(&focus_handle, cx);
            self.versions_modal = Some(VersionsModalState {
                note_title,
                has_path: false,
                loading: false,
                entries: Vec::new(),
                selected: 0,
                focus_handle,
            });
            cx.notify();
            return;
        };
        let root = app_data_root(cx);
        window.focus(&focus_handle, cx);
        self.versions_modal = Some(VersionsModalState {
            note_title,
            has_path: true,
            loading: true,
            entries: Vec::new(),
            selected: 0,
            focus_handle,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let entries = cx
                .background_executor()
                .spawn(async move { load_version_entries(&root, &path) })
                .await;
            let _ = this.update(cx, |note, cx| {
                if let Some(modal) = note.versions_modal.as_mut() {
                    modal.entries = entries;
                    modal.loading = false;
                    modal.selected = 0;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn restore_selected_version(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let preview = self.versions_modal.as_ref().and_then(|modal| {
            modal
                .entries
                .get(modal.selected)
                .and_then(|entry| entry.preview.clone())
        });
        self.versions_modal = None;
        self.focus_editor(window, cx);
        if let Some(text) = preview {
            self.editor.update(cx, |editor, cx| {
                editor.replace_document_undoable(&text, window, cx);
            });
        }
        cx.notify();
    }

    pub(crate) fn on_versions_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            cx.stop_propagation();
            self.versions_modal = None;
            self.focus_editor(window, cx);
            cx.notify();
            return;
        }
        if key == "enter" {
            cx.stop_propagation();
            self.restore_selected_version(window, cx);
            return;
        }
        let Some(modal) = self.versions_modal.as_mut() else {
            return;
        };
        if key == "down" {
            cx.stop_propagation();
            if !modal.entries.is_empty() {
                modal.selected = (modal.selected + 1).min(modal.entries.len() - 1);
                cx.notify();
            }
        } else if key == "up" {
            cx.stop_propagation();
            modal.selected = modal.selected.saturating_sub(1);
            cx.notify();
        }
    }

    pub(crate) fn render_palette_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = self.palette.as_ref()?;
        let filtered = palette.filtered_indices();
        let selected = palette.selected.min(filtered.len().saturating_sub(1));
        let query_display = if palette.query.is_empty() {
            div()
                .text_color(self.tokens.dim)
                .child("Type a command…")
                .into_any_element()
        } else {
            div()
                .text_color(self.tokens.ink)
                .child(SharedString::from(palette.query.clone()))
                .into_any_element()
        };

        let mut list = div()
            .id("command-palette-list")
            .flex()
            .flex_col()
            .max_h(px(320.0))
            .overflow_y_scroll();
        if filtered.is_empty() {
            list = list.child(
                div()
                    .px(px(14.0))
                    .py(px(12.0))
                    .text_color(self.tokens.dim)
                    .child("No matching commands"),
            );
        } else {
            for (pos, &item_index) in filtered.iter().enumerate() {
                let item = &palette.items[item_index];
                let is_selected = pos == selected;
                let prefix = if item.checked { "✓ " } else { "" };
                let label = format!("{prefix}{}", item.label);
                let mut row = div()
                    .id(SharedString::from(format!("palette-item-{pos}")))
                    .debug_selector(move || format!("palette-item-{pos}"))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px(px(14.0))
                    .py(px(7.0))
                    .cursor_pointer()
                    .when(is_selected, |row| row.bg(self.tokens.selection))
                    .on_click(cx.listener(move |note, _, window, cx| {
                        note.execute_palette_action(pos, window, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(self.tokens.dim)
                                    .child(SharedString::from(item.category.clone())),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_color(self.tokens.ink)
                                    .font_weight(if is_selected {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .child(SharedString::from(label)),
                            ),
                    );
                if !item.shortcut.is_empty() {
                    row = row.child(
                        div()
                            .flex_shrink_0()
                            .text_color(self.tokens.mid)
                            .child(SharedString::from(item.shortcut.clone())),
                    );
                }
                list = list.child(row);
            }
        }

        Some(
            div()
                .id("command-palette-backdrop")
                .debug_selector(|| "command-palette-backdrop".into())
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .pt(px(56.0))
                .px(px(20.0))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|note, _: &MouseDownEvent, window, cx| {
                        note.palette = None;
                        note.focus_editor(window, cx);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("command-palette")
                        .debug_selector(|| "command-palette".into())
                        .role(Role::List)
                        .occlude()
                        .track_focus(&palette.focus_handle)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_key_down(cx.listener(Self::on_palette_key_down))
                        .w_full()
                        .max_w(px(520.0))
                        .rounded(px(8.0))
                        .bg(self.tokens.paper)
                        .border_1()
                        .border_color(self.tokens.rule)
                        .font_family("IBM Plex Mono")
                        .text_size(px(12.0))
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(
                            div()
                                .debug_selector(|| "command-palette-query".into())
                                .px(px(14.0))
                                .py(px(10.0))
                                .border_b_1()
                                .border_color(self.tokens.rule)
                                .bg(self.tokens.code_bg)
                                .child(query_display),
                        )
                        .child(list),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn render_versions_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.versions_modal.as_ref()?;
        let body = if !modal.has_path {
            div()
                .p(px(20.0))
                .text_color(self.tokens.mid)
                .child("Save this note first to start keeping local versions.")
                .into_any_element()
        } else if modal.loading {
            div()
                .p(px(20.0))
                .text_color(self.tokens.dim)
                .child("Loading saved versions…")
                .into_any_element()
        } else if modal.entries.is_empty() {
            div()
                .p(px(20.0))
                .text_color(self.tokens.mid)
                .child("No prior snapshots recorded for this note yet.")
                .into_any_element()
        } else {
            let selected = modal.selected.min(modal.entries.len() - 1);
            let mut sidebar = div()
                .id("versions-list")
                .w(px(220.0))
                .flex_shrink_0()
                .border_r_1()
                .border_color(self.tokens.rule)
                .flex()
                .flex_col()
                .overflow_y_scroll();
            for (pos, entry) in modal.entries.iter().enumerate() {
                let is_selected = pos == selected;
                sidebar = sidebar.child(
                    div()
                        .id(SharedString::from(format!("version-entry-{pos}")))
                        .debug_selector(move || format!("version-entry-{pos}"))
                        .px(px(12.0))
                        .py(px(8.0))
                        .cursor_pointer()
                        .when(is_selected, |row| row.bg(self.tokens.selection))
                        .on_click(cx.listener(move |note, _, _window, cx| {
                            if let Some(m) = note.versions_modal.as_mut() {
                                m.selected = pos;
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .text_color(self.tokens.ink)
                                .font_weight(if is_selected {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::NORMAL
                                })
                                .child(SharedString::from(entry.label.clone())),
                        )
                        .child(
                            div()
                                .text_color(self.tokens.dim)
                                .text_size(px(11.0))
                                .child(SharedString::from(entry.summary.clone())),
                        ),
                );
            }
            let current = &modal.entries[selected];
            let preview_body = match (&current.preview, &current.error) {
                (Some(text), _) => SharedString::from(text.clone()),
                (None, Some(err)) => SharedString::from(err.clone()),
                (None, None) => SharedString::from(""),
            };
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .child(sidebar)
                .child(
                    div()
                        .id("versions-preview")
                        .debug_selector(|| "versions-preview".into())
                        .flex_1()
                        .p(px(16.0))
                        .overflow_y_scroll()
                        .text_color(self.tokens.ink)
                        .child(preview_body),
                )
                .into_any_element()
        };

        let can_restore = modal
            .entries
            .get(modal.selected)
            .and_then(|e| e.preview.as_ref())
            .is_some();

        Some(
            div()
                .id("versions-backdrop")
                .debug_selector(|| "versions-backdrop".into())
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.0))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|note, _: &MouseDownEvent, window, cx| {
                        note.versions_modal = None;
                        note.focus_editor(window, cx);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("versions-modal")
                        .debug_selector(|| "versions-modal".into())
                        .occlude()
                        .track_focus(&modal.focus_handle)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_key_down(cx.listener(Self::on_versions_key_down))
                        .w_full()
                        .max_w(px(680.0))
                        .h(px(420.0))
                        .rounded(px(8.0))
                        .bg(self.tokens.paper)
                        .border_1()
                        .border_color(self.tokens.rule)
                        .font_family("IBM Plex Mono")
                        .text_size(px(12.0))
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(
                            div()
                                .px(px(16.0))
                                .py(px(10.0))
                                .border_b_1()
                                .border_color(self.tokens.rule)
                                .bg(self.tokens.code_bg)
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_color(self.tokens.ink)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(SharedString::from(format!(
                                            "Versions — {}",
                                            modal.note_title
                                        ))),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .when(can_restore, |row| {
                                            row.child(
                                                div()
                                                    .id("versions-restore")
                                                    .debug_selector(|| "versions-restore".into())
                                                    .role(Role::Button)
                                                    .cursor_pointer()
                                                    .text_color(self.tokens.accent)
                                                    .child("Restore")
                                                    .on_click(cx.listener(
                                                        |note, _, window, cx| {
                                                            note.restore_selected_version(
                                                                window, cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                        })
                                        .child(
                                            div()
                                                .id("versions-close")
                                                .debug_selector(|| "versions-close".into())
                                                .role(Role::Button)
                                                .cursor_pointer()
                                                .text_color(self.tokens.dim)
                                                .child("Close")
                                                .on_click(cx.listener(|note, _, window, cx| {
                                                    note.versions_modal = None;
                                                    note.focus_editor(window, cx);
                                                    cx.notify();
                                                })),
                                        ),
                                ),
                        )
                        .child(body),
                )
                .into_any_element(),
        )
    }
}
