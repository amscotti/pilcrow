//! The editor's frozen public surface.
//!
//! Everything another crate may name is declared or re-exported here: the
//! view itself, the typed events it emits, the snapshots it hands out, and
//! the keybinding installers. The rest of `pilcrow-editor` — layout,
//! shaping, paint, platform input — is private implementation that can move
//! between modules without touching a caller.
//!
//! Adding to this file is a deliberate act. If the app needs something new
//! from the editor, it lands here first, with the reason; shrinking it is
//! equally welcome when a caller goes away.

pub use crate::{EditorView, install_keybindings, install_keybindings_for};
pub use pilcrow_commands::keymap::Platform;

/// The bundled body face: IBM Plex Mono 2.5.0, SIL OFL 1.1 (see
/// `assets/fonts/LICENSE.txt`). One face the app owns on every OS, so
/// layout, measurements, tests, and screenshots no longer depend on
/// whatever monospace the host happens to ship. The four faces
/// are exactly what the editor requests: regular/italic at normal
/// weight, semibold for emphasis.
const PLEX_REGULAR: &[u8] = include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf");
const PLEX_ITALIC: &[u8] = include_bytes!("../assets/fonts/IBMPlexMono-Italic.ttf");
const PLEX_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/IBMPlexMono-SemiBold.ttf");
const PLEX_SEMIBOLD_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/IBMPlexMono-SemiBoldItalic.ttf");

/// Registers the bundled body face with the text system. Call once at
/// startup, before the first window opens. A failure is logged and the
/// text system falls back to host faces rather than refusing to start.
pub fn install_fonts(cx: &mut gpui::App) {
    let fonts = vec![
        std::borrow::Cow::Borrowed(PLEX_REGULAR),
        std::borrow::Cow::Borrowed(PLEX_ITALIC),
        std::borrow::Cow::Borrowed(PLEX_SEMIBOLD),
        std::borrow::Cow::Borrowed(PLEX_SEMIBOLD_ITALIC),
    ];
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        eprintln!("pilcrow: could not install the bundled font: {error:#}");
    }
}

/// What changed in the editor, in the terms the window around it cares
/// about. The editor still notifies itself for repaint on every visual
/// change; these events are the narrow typed channel for everything else,
/// so a scroll, a spell result, or a find-bar keystroke never wakes the
/// window's save machinery or chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorEvent {
    /// The buffer bytes changed: typing, deletion, paste, format, undo,
    /// redo, an IME mark/commit/cancel, or a whole-document replace.
    /// Carries the buffer revision after the edit.
    Edited { revision: u64 },
    /// The caret or selection moved. Emitted once per distinct
    /// `(caret, anchor)` the paint path observes, coalescing a drag's
    /// intermediate positions between frames.
    SelectionChanged,
    /// The idle word/character count landed for the current text.
    StatsChanged,
    /// Focus mode, focus scope, typewriter mode, or reading view changed.
    ModeChanged,
}

/// A point-in-time view of the editor. The text is one owned snapshot, so
/// save/session code can capture a consistent revision without reading fields
/// one at a time; `Arc` keeps later clones cheap.
#[derive(Clone, Debug)]
pub struct EditorSnapshot {
    pub text: std::sync::Arc<str>,
    pub revision: u64,
    pub caret: usize,
    pub anchor: Option<usize>,
    pub reading: bool,
    pub focus: bool,
    pub typewriter: bool,
    pub scale: f32,
    pub measure_chars: usize,
}

/// What focus mode keeps at full contrast.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FocusScope {
    #[default]
    Paragraph,
    Sentence,
}

/// A placed line as the paint path saw it. This is the read-only view the test
/// driver uses; `line` indexes the layout's placed lines, and `start`/`end`
/// are absolute document bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibleLine {
    pub line: usize,
    pub block: usize,
    pub start: usize,
    pub end: usize,
    pub x: f32,
    pub y: f32,
    pub h: f32,
    pub rule: bool,
}

/// How many frame records the trace ring keeps. Oldest records are dropped
/// past this bound, so a long trace costs a fixed amount of memory.
pub const TRACE_CAPACITY: usize = 1024;

/// One traced frame: what was painted (or what an input left behind), for
/// frame-settlement assertions in tests. Produced only while a trace is
/// active; see [`EditorView::trace_start`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRecord {
    /// Completed paints at the time of the record (`frame_seq`).
    pub seq: u64,
    /// Driver inputs applied up to this record. Every input produces its own
    /// record, so a gap in the sequence means an input was dropped.
    pub input_seq: u64,
    /// Buffer revision reflected by this record.
    pub revision: u64,
    pub caret: usize,
    pub anchor: Option<usize>,
    pub scroll: f32,
    pub layout_done: bool,
    /// `(x, y, w, h)` in pixels when the caret was placed and visible.
    pub caret_rect: Option<(f32, f32, f32, f32)>,
    /// Milliseconds of layout-and-paint work for painted frames, measured
    /// from prepaint to the end of paint. Input and baseline records carry
    /// 0: they mark state, not work, so a p99 over the records judges the
    /// frame budget rather than the test client's polling gaps.
    pub frame_ms: f64,
    /// A hash of what the frame shows: content, caret, scroll, and visible
    /// line geometry. Equal hashes across the records after an input mean
    /// the paint is stable, not stale.
    pub paint_hash: u64,
}
