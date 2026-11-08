//! One platform keymap model for the shell and the editor.
//!
//! Tests can install either dialect on any host, which is how the macOS and
//! Linux/Windows maps stay honest without separate runners for every key.

use gpui::{App, KeyBinding};

use crate::app_actions::*;
use crate::editor_actions::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Other,
}

pub fn install_editor_keybindings_for(cx: &mut App, platform: Platform) {
    let (
        select_all,
        copy,
        cut,
        paste,
        line_start,
        line_end,
        doc_start,
        doc_end,
        undo,
        redo,
        redo_alt,
        bold,
        italic,
        strike,
        code,
        link,
        bullet,
        ordered,
        task,
        quote,
    ) = match platform {
        Platform::MacOs => (
            "cmd-a",
            "cmd-c",
            "cmd-x",
            "cmd-v",
            "cmd-left",
            "cmd-right",
            "cmd-up",
            "cmd-down",
            "cmd-z",
            "cmd-shift-z",
            None,
            "cmd-b",
            "cmd-i",
            "alt-cmd-u",
            "cmd-j",
            "cmd-k",
            "cmd-l",
            "cmd-shift-l",
            "alt-cmd-l",
            "cmd-shift-.",
        ),
        Platform::Other => (
            "ctrl-a",
            "ctrl-c",
            "ctrl-x",
            "ctrl-v",
            "home",
            "end",
            "ctrl-home",
            "ctrl-end",
            "ctrl-z",
            "ctrl-shift-z",
            Some("ctrl-y"),
            "ctrl-b",
            "ctrl-i",
            "ctrl-alt-u",
            "ctrl-j",
            "ctrl-k",
            "ctrl-l",
            "ctrl-shift-l",
            "ctrl-alt-l",
            "ctrl-shift-.",
        ),
    };

    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("Editor")),
        KeyBinding::new("delete", Delete, Some("Editor")),
        KeyBinding::new("enter", InsertNewline, Some("Editor")),
        KeyBinding::new("tab", InsertTab, Some("Editor")),
        KeyBinding::new("left", Left, Some("Editor")),
        KeyBinding::new("right", Right, Some("Editor")),
        KeyBinding::new("up", Up, Some("Editor")),
        KeyBinding::new("down", Down, Some("Editor")),
        KeyBinding::new("home", Home, Some("Editor")),
        KeyBinding::new("end", End, Some("Editor")),
        KeyBinding::new("shift-left", SelectLeft, Some("Editor")),
        KeyBinding::new("shift-right", SelectRight, Some("Editor")),
        KeyBinding::new(select_all, SelectAll, Some("Editor")),
        KeyBinding::new(copy, Copy, Some("Editor")),
        KeyBinding::new(cut, Cut, Some("Editor")),
        KeyBinding::new(paste, Paste, Some("Editor")),
        KeyBinding::new(line_start, Home, Some("Editor")),
        KeyBinding::new(line_end, End, Some("Editor")),
        KeyBinding::new(doc_start, DocStart, Some("Editor")),
        KeyBinding::new(doc_end, DocEnd, Some("Editor")),
        KeyBinding::new(undo, Undo, Some("Editor")),
        KeyBinding::new(redo, Redo, Some("Editor")),
        KeyBinding::new(bold, FormatBold, Some("Editor")),
        KeyBinding::new(italic, FormatItalic, Some("Editor")),
        KeyBinding::new(strike, FormatStrike, Some("Editor")),
        KeyBinding::new(code, FormatCode, Some("Editor")),
        KeyBinding::new(link, FormatLink, Some("Editor")),
        KeyBinding::new(bullet, BulletList, Some("Editor")),
        KeyBinding::new(ordered, OrderedList, Some("Editor")),
        KeyBinding::new(task, TaskList, Some("Editor")),
        KeyBinding::new(quote, Quote, Some("Editor")),
    ]);
    match platform {
        Platform::MacOs => cx.bind_keys([
            KeyBinding::new("cmd-1", Heading1, Some("Editor")),
            KeyBinding::new("cmd-2", Heading2, Some("Editor")),
            KeyBinding::new("cmd-3", Heading3, Some("Editor")),
            KeyBinding::new("cmd-4", Heading4, Some("Editor")),
            KeyBinding::new("cmd-5", Heading5, Some("Editor")),
            KeyBinding::new("cmd-6", Heading6, Some("Editor")),
        ]),
        Platform::Other => cx.bind_keys([
            KeyBinding::new("ctrl-1", Heading1, Some("Editor")),
            KeyBinding::new("ctrl-2", Heading2, Some("Editor")),
            KeyBinding::new("ctrl-3", Heading3, Some("Editor")),
            KeyBinding::new("ctrl-4", Heading4, Some("Editor")),
            KeyBinding::new("ctrl-5", Heading5, Some("Editor")),
            KeyBinding::new("ctrl-6", Heading6, Some("Editor")),
        ]),
    }
    if let Some(redo_alt) = redo_alt {
        cx.bind_keys([KeyBinding::new(redo_alt, Redo, Some("Editor"))]);
    }
    match platform {
        Platform::MacOs => cx.bind_keys([
            KeyBinding::new("cmd-d", ToggleFocus, Some("Editor")),
            KeyBinding::new("cmd-alt-p", FocusParagraph, Some("Editor")),
            KeyBinding::new("cmd-alt-s", FocusSentence, Some("Editor")),
            KeyBinding::new("cmd-alt-t", ToggleTypewriter, Some("Editor")),
            KeyBinding::new("alt-left", WordLeft, Some("Editor")),
            KeyBinding::new("alt-right", WordRight, Some("Editor")),
            KeyBinding::new("shift-alt-left", SelectWordLeft, Some("Editor")),
            KeyBinding::new("shift-alt-right", SelectWordRight, Some("Editor")),
            KeyBinding::new("cmd-f", Find, Some("Editor")),
            KeyBinding::new("cmd-g", FindNext, Some("Editor")),
            KeyBinding::new("cmd-shift-g", FindPrevious, Some("Editor")),
            KeyBinding::new("escape", CloseFind, Some("Editor")),
            KeyBinding::new("cmd-r", ToggleReading, Some("Editor")),
            KeyBinding::new("cmd-alt-x", ToggleTask, Some("Editor")),
            KeyBinding::new("alt-backspace", BackspaceWord, Some("Editor")),
            KeyBinding::new("alt-delete", DeleteWord, Some("Editor")),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, Some("Editor")),
            KeyBinding::new("cmd-delete", DeleteToLineEnd, Some("Editor")),
            KeyBinding::new("shift-up", SelectUp, Some("Editor")),
            KeyBinding::new("shift-down", SelectDown, Some("Editor")),
            KeyBinding::new("shift-home", SelectHome, Some("Editor")),
            KeyBinding::new("shift-end", SelectEnd, Some("Editor")),
            KeyBinding::new("cmd-shift-left", SelectHome, Some("Editor")),
            KeyBinding::new("cmd-shift-right", SelectEnd, Some("Editor")),
            KeyBinding::new("cmd-shift-up", SelectDocStart, Some("Editor")),
            KeyBinding::new("cmd-shift-down", SelectDocEnd, Some("Editor")),
            KeyBinding::new("pageup", PageUp, Some("Editor")),
            KeyBinding::new("pagedown", PageDown, Some("Editor")),
            KeyBinding::new("shift-pageup", SelectPageUp, Some("Editor")),
            KeyBinding::new("shift-pagedown", SelectPageDown, Some("Editor")),
        ]),
        Platform::Other => cx.bind_keys([
            KeyBinding::new("ctrl-d", ToggleFocus, Some("Editor")),
            KeyBinding::new("ctrl-alt-p", FocusParagraph, Some("Editor")),
            KeyBinding::new("ctrl-alt-s", FocusSentence, Some("Editor")),
            KeyBinding::new("ctrl-alt-t", ToggleTypewriter, Some("Editor")),
            KeyBinding::new("ctrl-left", WordLeft, Some("Editor")),
            KeyBinding::new("ctrl-right", WordRight, Some("Editor")),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, Some("Editor")),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, Some("Editor")),
            KeyBinding::new("ctrl-f", Find, Some("Editor")),
            KeyBinding::new("ctrl-g", FindNext, Some("Editor")),
            KeyBinding::new("ctrl-shift-g", FindPrevious, Some("Editor")),
            KeyBinding::new("escape", CloseFind, Some("Editor")),
            KeyBinding::new("ctrl-r", ToggleReading, Some("Editor")),
            KeyBinding::new("ctrl-alt-x", ToggleTask, Some("Editor")),
            KeyBinding::new("ctrl-backspace", BackspaceWord, Some("Editor")),
            KeyBinding::new("ctrl-delete", DeleteWord, Some("Editor")),
            KeyBinding::new("shift-up", SelectUp, Some("Editor")),
            KeyBinding::new("shift-down", SelectDown, Some("Editor")),
            KeyBinding::new("shift-home", SelectHome, Some("Editor")),
            KeyBinding::new("shift-end", SelectEnd, Some("Editor")),
            KeyBinding::new("ctrl-shift-home", SelectDocStart, Some("Editor")),
            KeyBinding::new("ctrl-shift-end", SelectDocEnd, Some("Editor")),
            KeyBinding::new("ctrl-shift-up", SelectDocStart, Some("Editor")),
            KeyBinding::new("ctrl-shift-down", SelectDocEnd, Some("Editor")),
            KeyBinding::new("pageup", PageUp, Some("Editor")),
            KeyBinding::new("pagedown", PageDown, Some("Editor")),
            KeyBinding::new("shift-pageup", SelectPageUp, Some("Editor")),
            KeyBinding::new("shift-pagedown", SelectPageDown, Some("Editor")),
        ]),
    }
}

/// Install shell actions, then the editor map they dispatch into.
pub fn install_app_keybindings_for(cx: &mut App, platform: Platform) {
    let (new_note, open, save, save_as, appearance) = match platform {
        Platform::MacOs => ("cmd-n", "cmd-o", "cmd-s", "cmd-shift-s", "cmd-alt-a"),
        Platform::Other => ("ctrl-n", "ctrl-o", "ctrl-s", "ctrl-shift-s", "ctrl-alt-a"),
    };
    let (quit, close, next_note, previous_note, size_up, size_up_plus, size_down, size_reset) =
        match platform {
            Platform::MacOs => (
                "cmd-q",
                "cmd-w",
                "cmd-`",
                "cmd-shift-`",
                "cmd-=",
                "cmd-+",
                "cmd--",
                "cmd-0",
            ),
            Platform::Other => (
                "ctrl-q",
                "ctrl-w",
                "ctrl-`",
                "ctrl-shift-`",
                "ctrl-=",
                "ctrl-+",
                "ctrl--",
                "ctrl-0",
            ),
        };

    cx.bind_keys([
        KeyBinding::new(new_note, NewNote, Some("NoteWindow")),
        KeyBinding::new(open, Open, Some("NoteWindow")),
        KeyBinding::new(save, Save, Some("NoteWindow")),
        KeyBinding::new(save_as, SaveAs, Some("NoteWindow")),
        KeyBinding::new(appearance, CycleAppearance, Some("NoteWindow")),
        KeyBinding::new(quit, QuitApp, Some("NoteWindow")),
        KeyBinding::new(close, CloseWindow, Some("NoteWindow")),
        KeyBinding::new(next_note, NextNote, Some("NoteWindow")),
        KeyBinding::new(previous_note, PreviousNote, Some("NoteWindow")),
        KeyBinding::new(size_up, TextSizeUp, Some("NoteWindow")),
        KeyBinding::new(size_up_plus, TextSizeUp, Some("NoteWindow")),
        KeyBinding::new(size_down, TextSizeDown, Some("NoteWindow")),
        KeyBinding::new(size_reset, TextSizeReset, Some("NoteWindow")),
    ]);
    install_editor_keybindings_for(cx, platform);
}
