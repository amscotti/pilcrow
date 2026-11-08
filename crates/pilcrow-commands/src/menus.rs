//! One menu model for the application. Actions are inert values; the app and
//! editor install the handlers.

use std::path::PathBuf;

use gpui::{Menu, MenuItem, OsAction, SystemMenuType};

use crate::app_actions::*;
use crate::editor_actions::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuMeasure {
    Narrow,
    Standard,
    Wide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAppearance {
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuFocus {
    Paragraph,
    Sentence,
}

#[derive(Clone, Debug, Default)]
pub struct MenuOptions {
    pub appearance: Option<MenuAppearance>,
    pub measure: Option<MenuMeasure>,
    pub high_contrast: bool,
    pub focus: Option<MenuFocus>,
    pub typewriter: bool,
    pub reading: bool,
    /// Label, path. Labels are prepared by the shell because they need the
    /// current file list; the action lives here so recent items stay typed.
    pub recent: Vec<(String, PathBuf)>,
}

impl MenuOptions {
    pub fn with_appearance(mut self, appearance: MenuAppearance) -> Self {
        self.appearance = Some(appearance);
        self
    }

    pub fn with_measure(mut self, measure: MenuMeasure) -> Self {
        self.measure = Some(measure);
        self
    }

    pub fn with_high_contrast(mut self, high_contrast: bool) -> Self {
        self.high_contrast = high_contrast;
        self
    }

    pub fn with_focus(mut self, focus: Option<MenuFocus>) -> Self {
        self.focus = focus;
        self
    }

    pub fn with_typewriter(mut self, typewriter: bool) -> Self {
        self.typewriter = typewriter;
        self
    }

    pub fn with_reading(mut self, reading: bool) -> Self {
        self.reading = reading;
        self
    }

    pub fn with_recent(mut self, recent: Vec<(String, PathBuf)>) -> Self {
        self.recent = recent;
        self
    }
}

pub fn menus(options: &MenuOptions) -> [Menu; 6] {
    let recent = if options.recent.is_empty() {
        vec![
            MenuItem::action(
                "No Recent Notes",
                OpenRecent {
                    path: PathBuf::new(),
                },
            )
            .disabled(true),
        ]
    } else {
        options
            .recent
            .iter()
            .map(|(label, path)| MenuItem::action(label.clone(), OpenRecent { path: path.clone() }))
            .collect()
    };
    let checked = |mode| options.appearance == Some(mode);
    let measure_checked = |mode| options.measure == Some(mode);

    [
        Menu::new("Pilcrow").items([
            MenuItem::action("About Pilcrow", About),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Pilcrow", QuitApp),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Note", NewNote),
            MenuItem::action("Open…", Open),
            MenuItem::submenu(Menu::new("Open Recent").items(recent)),
            MenuItem::action("Save", Save),
            MenuItem::action("Save As…", SaveAs),
            MenuItem::action("Export HTML…", ExportHtml),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::action("Copy as HTML", CopyHtml),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", Find),
            MenuItem::action("Find Next", FindNext),
            MenuItem::action("Find Previous", FindPrevious),
        ]),
        Menu::new("Format").items([
            MenuItem::action("Bold", FormatBold),
            MenuItem::action("Italic", FormatItalic),
            MenuItem::action("Strikethrough", FormatStrike),
            MenuItem::action("Inline Code", FormatCode),
            MenuItem::action("Link", FormatLink),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Heading").items([
                MenuItem::action("Heading 1", Heading1),
                MenuItem::action("Heading 2", Heading2),
                MenuItem::action("Heading 3", Heading3),
                MenuItem::action("Heading 4", Heading4),
                MenuItem::action("Heading 5", Heading5),
                MenuItem::action("Heading 6", Heading6),
            ])),
            MenuItem::separator(),
            MenuItem::action("Bulleted List", BulletList),
            MenuItem::action("Numbered List", OrderedList),
            MenuItem::action("Task List", TaskList),
            MenuItem::action("Toggle Task", ToggleTask),
            MenuItem::action("Blockquote", Quote),
        ]),
        Menu::new("View").items([
            MenuItem::submenu(
                Menu::new("Focus").items([
                    MenuItem::action("Focus Mode", ToggleFocus).checked(options.focus.is_some()),
                    MenuItem::separator(),
                    MenuItem::action("Paragraph", FocusParagraph)
                        .checked(options.focus == Some(MenuFocus::Paragraph)),
                    MenuItem::action("Sentence", FocusSentence)
                        .checked(options.focus == Some(MenuFocus::Sentence)),
                ]),
            ),
            MenuItem::action("Typewriter", ToggleTypewriter).checked(options.typewriter),
            MenuItem::action("Reading View", ToggleReading).checked(options.reading),
            MenuItem::submenu(Menu::new("Text Size").items([
                MenuItem::action("Increase", TextSizeUp),
                MenuItem::action("Decrease", TextSizeDown),
                MenuItem::action("Actual Size", TextSizeReset),
            ])),
            MenuItem::submenu(
                Menu::new("Measure").items([
                    MenuItem::action("Narrow", MeasureNarrow)
                        .checked(measure_checked(MenuMeasure::Narrow)),
                    MenuItem::action("Standard", MeasureStandard)
                        .checked(measure_checked(MenuMeasure::Standard)),
                    MenuItem::action("Wide", MeasureWide)
                        .checked(measure_checked(MenuMeasure::Wide)),
                ]),
            ),
            MenuItem::separator(),
            MenuItem::submenu(
                Menu::new("Appearance").items([
                    MenuItem::action("Follow System", AppearanceAuto)
                        .checked(checked(MenuAppearance::Auto)),
                    MenuItem::action("Light", AppearanceLight)
                        .checked(checked(MenuAppearance::Light)),
                    MenuItem::action("Dark", AppearanceDark).checked(checked(MenuAppearance::Dark)),
                    MenuItem::separator(),
                    MenuItem::action("High Contrast", ToggleHighContrast)
                        .checked(options.high_contrast),
                    MenuItem::action("Cycle Appearance", CycleAppearance),
                ]),
            ),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Next Note", NextNote),
            MenuItem::action("Previous Note", PreviousNote),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
    ]
}
