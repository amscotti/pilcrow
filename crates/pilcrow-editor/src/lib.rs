//! The GPUI writing surface: block layout, real shaped glyphs, a caret derived
//! from the same `ShapedLine`, selection, keyboard movement and IME plumbing.
//!
//! The buffer is still the only document. Blocks are scanned from its bytes,
//! the layout wraps those blocks into placed lines, and the paint path shapes
//! only the lines the viewport can see. Every edit goes through `Buffer`, and
//! the incremental layout resumes at the earliest block whose geometry could
//! have changed.

mod a11y;
pub mod api;
mod find;
mod input;
mod paint;
mod reading;
mod spell;

pub use api::{
    EditorEvent, EditorSnapshot, FocusScope, FrameRecord, TRACE_CAPACITY, VisibleLine,
    install_fonts,
};
pub(crate) use find::FindField;
use find::FindState;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClickEvent, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase,
    Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, Font, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Render, Role,
    ScrollEvent, ScrollHandle, ShapedLine, SharedString, StrikethroughStyle, Style, Subscription,
    Task, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WindowTextSystem, accesskit,
    div, fill, font, point, px, relative, size,
};
pub(crate) use pilcrow_commands::editor_actions::*;
pub use pilcrow_commands::keymap::Platform;
use pilcrow_core::style::{self, Look};
use pilcrow_core::{
    Applied, Block, BlockKind, Buffer, Edit, EditGroup, FormatCommand, History, InlineKind, Layout,
    MeasureStyle, Measurer, Placed, Stats, boundary_at_or_before, compute_stats, format_command,
    inline_kinds, inline_links, lists, rescan_from, scan_blocks, search as text_search,
};
use pilcrow_ui::page::{Page, page_for};
use pilcrow_ui::theme::Tokens;
use pilcrow_ui::typography::{BODY_SIZE, HEAD, MEASURE_CHARS, line_height};
use unicode_segmentation::UnicodeSegmentation;

// The bundled IBM Plex Mono (see `api::install_fonts`) is the one face the
// page is measured and painted in on every OS. The page measure is shaped
// in the same face, so the column and the glyphs can never disagree.
pub(crate) const BODY_FONT: &str = "IBM Plex Mono";
const FONT_SIZE: Pixels = px(BODY_SIZE);
const LINE_HEIGHT_F32: f32 = line_height();
const LINE_HEIGHT: Pixels = px(LINE_HEIGHT_F32);
const HEAD_PX: Pixels = px(HEAD);
const CARET_WIDTH: Pixels = px(1.5);

/// How much extra document above and below the viewport is shaped. The glyphs
/// are ready when a line straddles an edge; the paint loop drops lines that
/// are wholly outside the page.
const RUN_UP: f32 = 64.0;
const RUN_DOWN: f32 = 64.0;
/// Blocks placed per `Layout::extend` call while catching up.
const LAYOUT_CHUNK: usize = 64;
/// A hard ceiling per ensure pass. A long document finishes over later frames
/// instead of making one keystroke pay for all of it.
const LAYOUT_BUDGET: usize = 256;
/// One idle frame's background fill after the viewport is covered.
const LAYOUT_BACKGROUND: usize = 128;
/// Blocks of placed context a jump keeps above its target, so upward
/// movement from the landing spot has somewhere exact to go.
const LAYOUT_LOOKBACK: usize = 128;

/// The face a measured or painted run is shaped in. The page is monospace
/// already, so `mono` keeps the same face; it remains in the style so a
/// future body face can make inline code distinct without changing the
/// layout callers.
fn font_for(style: MeasureStyle) -> Font {
    let mut face = font(BODY_FONT);
    face.weight = if style.bold {
        FontWeight::SEMIBOLD
    } else {
        FontWeight::NORMAL
    };
    face.style = if style.italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    face
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct MeasureKey {
    text: String,
    size_bits: u32,
    bold: bool,
    italic: bool,
    mono: bool,
}

/// The production [`Measurer`]: every width comes from the same text system
/// that paints the glyphs, never from a per-character approximation. Prefix
/// widths are cached for the duration of one layout pass because wrapping asks
/// for the same ranges repeatedly while it grows its bracket.
struct WindowMeasurer<'a> {
    text_system: &'a WindowTextSystem,
    cache: RefCell<HashMap<MeasureKey, f32>>,
}

impl<'a> WindowMeasurer<'a> {
    fn new(text_system: &'a WindowTextSystem) -> Self {
        Self {
            text_system,
            cache: RefCell::new(HashMap::new()),
        }
    }
}

impl Measurer for WindowMeasurer<'_> {
    fn width(&self, text: &str, style: MeasureStyle) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        // `shape_line` panics on a newline. Wrapping already turns a soft break
        // into a measured space; do the same here as the last line of defence.
        let shown = if text.contains('\n') {
            text.replace('\n', " ")
        } else {
            text.to_owned()
        };
        let key = MeasureKey {
            size_bits: style.size.to_bits(),
            bold: style.bold,
            italic: style.italic,
            mono: style.mono,
            text: shown.clone(),
        };
        if let Some(width) = self.cache.borrow().get(&key) {
            return *width;
        }
        let run = TextRun {
            len: shown.len(),
            font: font_for(style),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let width = self
            .text_system
            .shape_line(
                SharedString::new(shown),
                px(style.size),
                std::slice::from_ref(&run),
                None,
            )
            .width()
            / px(1.0);
        self.cache.borrow_mut().insert(key, width);
        width
    }
}

/// The complete face, colour and decoration of one run of painted glyphs.
/// Equal styles merge into one `TextRun`, which keeps `shape_line` from
/// resolving the same face once per character.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RunStyle {
    font: Font,
    color: Hsla,
    background_color: Option<Hsla>,
    underline: Option<UnderlineStyle>,
    strikethrough: Option<StrikethroughStyle>,
}

impl RunStyle {
    fn run(&self, len: usize) -> TextRun {
        TextRun {
            len,
            font: self.font.clone(),
            color: self.color,
            background_color: self.background_color,
            underline: self.underline,
            strikethrough: self.strikethrough,
        }
    }
}

/// The words inside `[text](url)` links, as byte ranges in the block. Only the
/// words are an affordance; brackets and the address remain syntax.
fn link_text_ranges(text: &str) -> Vec<Range<usize>> {
    if !text.as_bytes().contains(&b'[') {
        return Vec::new();
    }
    inline_links(text)
        .into_iter()
        .filter(|link| link.text_start < link.text_end)
        .map(|link| link.text_start..link.text_end)
        .collect()
}

/// The marked composition range expressed in the block's own bytes.
fn block_marked_range(marked: Option<&Range<usize>>, block: Block) -> Option<Range<usize>> {
    let marked = marked?;
    let start = marked.start.max(block.start);
    let end = marked.end.min(block.end);
    (start < end).then(|| start - block.start..end - block.start)
}

/// The length of a quote's syntax at the start of one source line inside a
/// merged quote block. The block only records its first line's marker; the
/// rest are syntax on their own lines and should read just as quietly.
fn quote_marker_len(line: &[u8]) -> usize {
    let mut indent = 0;
    while indent < line.len() && indent < 3 && matches!(line[indent], b' ' | b'\t') {
        indent += 1;
    }
    if line.get(indent) != Some(&b'>') {
        return 0;
    }
    indent
        + 1
        + usize::from(
            line.get(indent + 1)
                .is_some_and(|byte| matches!(byte, b' ' | b'\t')),
        )
}

/// The task box at the start of a list item's content, if this block is one.
#[derive(Clone, Copy, Debug)]
struct TaskVisual {
    /// Byte offsets in the block's own text.
    box_start: usize,
    box_end: usize,
    text_start: usize,
    checked: bool,
}

fn task_visual(block: Block, full: &str) -> Option<TaskVisual> {
    if block.kind != BlockKind::ListItem {
        return None;
    }
    let rest = full.get(block.marker..)?;
    let len = pilcrow_core::blocks::task_box_len(rest)?;
    let box_end = block.marker + 3;
    Some(TaskVisual {
        box_start: block.marker,
        box_end,
        text_start: block.marker + len,
        checked: matches!(rest.as_bytes().get(1), Some(b'x' | b'X')),
    })
}

/// The checkbox's click target and document position.
#[derive(Clone, Copy, Debug)]
struct TaskBox {
    bounds: Bounds<Pixels>,
    start: usize,
    end: usize,
}

/// Paint a block's syntax as marker bytes. Inline markers already arrive this
/// way from the scanner; this adds the block prefix, every line of a merged
/// quote, and code fences.
fn apply_block_markers(block: Block, full: &str, kinds: &mut [InlineKind]) {
    let end = block.marker.min(kinds.len());
    kinds[..end].fill(InlineKind::Marker);
    if block.kind != BlockKind::Quote {
        return;
    }
    let bytes = full.as_bytes();
    let mut line_start = 0;
    loop {
        let line_end = bytes[line_start..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(bytes.len(), |offset| line_start + offset);
        let marker = quote_marker_len(&bytes[line_start..line_end]);
        let marker_end = (line_start + marker).min(kinds.len());
        if line_start < marker_end {
            kinds[line_start..marker_end].fill(InlineKind::Marker);
        }
        if line_end == bytes.len() {
            break;
        }
        line_start = line_end + 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn run_style(
    block: Block,
    look: Look,
    task: Option<TaskVisual>,
    focus: Option<FocusView>,
    kinds: Option<&[InlineKind]>,
    links: &[Range<usize>],
    marked: Option<&Range<usize>>,
    at: usize,
    tokens: &Tokens,
) -> RunStyle {
    let kind = kinds
        .and_then(|kinds| kinds.get(at))
        .copied()
        .unwrap_or(InlineKind::Plain);
    // A bracket shape inside a code span is code, not a link.
    let in_link = kind != InlineKind::Code && links.iter().any(|range| range.contains(&at));
    let face = MeasureStyle {
        size: look.size,
        bold: look.bold || kind == InlineKind::Bold,
        italic: kind == InlineKind::Italic,
        mono: block.kind == BlockKind::Code || kind == InlineKind::Code,
    };
    let base = match block.kind {
        BlockKind::Quote | BlockKind::Code | BlockKind::Frontmatter => tokens.mid,
        _ => tokens.ink,
    };
    let done = task.is_some_and(|task| task.checked && at >= task.text_start);
    let in_focus =
        focus.is_none_or(|focus| focus.start <= block.start + at && block.start + at < focus.end);
    let color = if kind == InlineKind::Marker {
        tokens.dim
    } else if !in_focus {
        tokens.mid
    } else if in_link {
        tokens.accent
    } else if done {
        tokens.mid
    } else {
        base
    };
    let underline = if in_link && kind != InlineKind::Marker {
        Some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(tokens.accent),
            wavy: false,
        })
    } else if marked.is_some_and(|marked| marked.contains(&at)) {
        Some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(tokens.ink),
            wavy: false,
        })
    } else {
        None
    };
    let strikethrough = (kind == InlineKind::Strike || done).then_some(StrikethroughStyle {
        thickness: px(1.0),
        color: Some(color),
    });
    RunStyle {
        font: font_for(face),
        color,
        background_color: None,
        underline,
        strikethrough,
    }
}

/// Shapes one placed line. Soft newlines inside a merged paragraph become
/// spaces, but the byte offsets stay identical, so the caret, selection and
/// IME ranges still address the file.
#[allow(clippy::too_many_arguments)]
fn text_runs_for_line(
    text: &str,
    block: Block,
    placed: Placed,
    kinds: Option<&[InlineKind]>,
    links: &[Range<usize>],
    marked: Option<&Range<usize>>,
    focus: Option<FocusView>,
    tokens: &Tokens,
    scale: f32,
) -> (SharedString, Vec<TextRun>) {
    let look = style::look_scaled(block, scale);
    let full = &text[block.start..block.end];
    let start = (placed.start as usize).min(full.len());
    let end = (placed.end as usize).min(full.len()).max(start);
    let segment = &full[start..end];
    let shown = if segment.contains('\n') {
        segment.replace('\n', " ")
    } else {
        segment.to_owned()
    };
    let marked = block_marked_range(marked, block);
    let task = task_visual(block, full);

    let mut runs = Vec::new();
    let mut at = start;
    while at < end {
        let style = run_style(
            block,
            look,
            task,
            focus,
            kinds,
            links,
            marked.as_ref(),
            at,
            tokens,
        );
        let run_start = at;
        at += 1;
        while at < end
            && run_style(
                block,
                look,
                task,
                focus,
                kinds,
                links,
                marked.as_ref(),
                at,
                tokens,
            ) == style
        {
            at += 1;
        }
        // A run may only end between scalars; the scanner paints whole
        // scalars, but a malformed external range must not split one.
        while at < end && !shown.is_char_boundary(at - start) {
            at += 1;
        }
        runs.push(style.run(at - run_start));
    }
    (SharedString::new(shown), runs)
}

/// A visual line, placed at the page origin. The shaped line is the geometry
/// source for painting, clicking and the caret alike.
struct LineGeometry {
    /// Index into [`Layout::lines`].
    index: usize,
    block: usize,
    /// Absolute byte range in the document.
    start: usize,
    end: usize,
    /// Document y, before scrolling and the page head.
    doc_y: f32,
    shaped: ShapedLine,
    bounds: Bounds<Pixels>,
    h: f32,
    rule: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct LineKey {
    block: u32,
    start: u32,
    end: u32,
}

struct ShapeCache {
    revision: u64,
    width: f32,
    scale: f32,
    ink: Option<Hsla>,
    marked: Option<Range<usize>>,
    focus: Option<FocusView>,
    lines: HashMap<LineKey, ShapedLine>,
}

impl ShapeCache {
    fn new() -> Self {
        Self {
            revision: u64::MAX,
            width: f32::NAN,
            scale: f32::NAN,
            ink: None,
            marked: None,
            focus: None,
            lines: HashMap::new(),
        }
    }
}

struct PrepaintState {
    lines: Vec<LineGeometry>,
    search: Vec<PaintQuad>,
    spell: Vec<PaintQuad>,
    selection: Vec<PaintQuad>,
    caret: Option<PaintQuad>,
    code_backgrounds: Vec<PaintQuad>,
    rules: Vec<PaintQuad>,
    task_hits: Vec<TaskBox>,
    scroll: f32,
    page: Page,
    /// The caret's block has been placed, so its geometry is final.
    caret_ready: bool,
    /// The scroll/run-up coverage target was reached this frame.
    covered: bool,
}

/// The block whose range contains `offset`. Blocks tile the document, so the
/// last block with a start at or before the offset is the one that owns it.
fn clamp_pixels(value: Pixels, min: Pixels, max: Pixels) -> Pixels {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// A local byte window around `at` bounded by `\n` line terminators, which are
/// unconditional UTF-8, grapheme (UAX #29 GB4), and word boundaries.
fn line_window(text: &str, at: usize) -> Range<usize> {
    let bytes = text.as_bytes();
    let at = at.min(bytes.len());
    let from = bytes[..at.saturating_sub(2)]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |index| index + 1);
    let to = bytes[at..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |offset| at + offset + 1);
    from..to
}

/// The word around `offset` for a double-click, or the character under it
/// when the click lands on whitespace or punctuation.
fn word_range(text: &str, offset: usize) -> Range<usize> {
    let offset = boundary_at_or_before(text, offset);
    let win = line_window(text, offset);
    for (rel_start, word) in text[win.clone()].unicode_word_indices() {
        let start = win.start + rel_start;
        let end = start + word.len();
        if offset >= start && offset < end {
            return start..end;
        }
    }
    let start = offset.min(text.len());
    let end = text[start..win.end]
        .graphemes(true)
        .next()
        .map_or(start, |grapheme| start + grapheme.len());
    start..end
}

fn is_sentence_end(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?' | '\n' | '。' | '！' | '？' | '…')
}

fn is_closing(ch: char) -> bool {
    matches!(ch, '"' | '\'' | '”' | '’' | ')' | ']' | '}' | '»')
}

fn is_text_block(block: Block) -> bool {
    !matches!(block.kind, BlockKind::Blank | BlockKind::Rule)
}

fn block_index_for_offset(blocks: &[Block], offset: usize) -> usize {
    if blocks.is_empty() {
        return 0;
    }
    blocks
        .partition_point(|block| block.start <= offset)
        .saturating_sub(1)
        .min(blocks.len() - 1)
}

/// The painted geometry for `offset`: the line whose range reaches it, or the
/// block's last line when the caret sits past its trailing whitespace.
fn line_geometry_for_offset<'a>(
    blocks: &[Block],
    lines: &'a [LineGeometry],
    offset: usize,
) -> Option<&'a LineGeometry> {
    let block_index = block_index_for_offset(blocks, offset);
    let block = blocks.get(block_index)?;
    let local = offset
        .saturating_sub(block.start)
        .min(block.end - block.start);
    let mut last = None;
    for line in lines
        .iter()
        .filter(|line| line.block == block_index && !line.rule)
    {
        last = Some(line);
        if local <= line.end - block.start {
            return Some(line);
        }
    }
    last
}

/// One stop on the vertical caret walk: a placed text line, or a blank block
/// that has height but no glyphs. Rules are furniture, not stops.
#[derive(Clone, Copy)]
struct CaretStop {
    block: usize,
    line: Option<usize>,
    y: f32,
}

fn prev_grapheme(text: &str, at: usize) -> usize {
    let at = boundary_at_or_before(text, at);
    let win = line_window(text, at);
    let mut previous = win.start;
    for (rel_index, _) in text[win.clone()].grapheme_indices(true) {
        let index = win.start + rel_index;
        if index >= at {
            break;
        }
        previous = index;
    }
    previous
}

fn prev_word(text: &str, at: usize) -> usize {
    let mut at = boundary_at_or_before(text, at);
    while at > 0 {
        let previous = prev_grapheme(text, at);
        if !text[previous..at].chars().all(char::is_whitespace) {
            break;
        }
        at = previous;
    }
    while at > 0 {
        let previous = prev_grapheme(text, at);
        if text[previous..at].chars().all(char::is_whitespace) {
            break;
        }
        at = previous;
    }
    at
}

fn next_word(text: &str, at: usize) -> usize {
    let mut at = boundary_at_or_before(text, at);
    while at < text.len() {
        let next = next_grapheme(text, at);
        if text[at..next].chars().all(char::is_whitespace) {
            break;
        }
        at = next;
    }
    while at < text.len() {
        let next = next_grapheme(text, at);
        if !text[at..next].chars().all(char::is_whitespace) {
            break;
        }
        at = next;
    }
    at
}

fn next_grapheme(text: &str, at: usize) -> usize {
    let at = boundary_at_or_before(text, at);
    let win = line_window(text, at);
    for (rel_index, grapheme) in text[win.clone()].grapheme_indices(true) {
        let index = win.start + rel_index;
        if index > at {
            return index;
        }
        if index == at {
            return index + grapheme.len();
        }
    }
    text.len()
}

fn grapheme_at_or_before(text: &str, at: usize) -> usize {
    let at = boundary_at_or_before(text, at);
    if at == text.len() {
        return at;
    }
    let win = line_window(text, at);
    let mut previous = win.start;
    for (rel_index, _) in text[win.clone()].grapheme_indices(true) {
        let index = win.start + rel_index;
        if index > at {
            break;
        }
        if index == at {
            return at;
        }
        previous = index;
    }
    previous
}

fn grapheme_at_or_after(text: &str, at: usize) -> usize {
    let at = boundary_at_or_before(text, at);
    let win = line_window(text, at);
    for (rel_index, _) in text[win.clone()].grapheme_indices(true) {
        let index = win.start + rel_index;
        if index >= at {
            return index;
        }
    }
    text.len()
}

/// The grapheme boundary closest to `x` in a shaped line. This is deliberately
/// not `closest_index_for_x`: that returns a UTF-8 byte index, and a single
/// wide glyph can therefore report the end of its bytes rather than the edge
/// of its glyph.
fn closest_grapheme_index(shaped: &ShapedLine, text: &str, x: Pixels) -> usize {
    let raw = shaped.closest_index_for_x(x);
    let previous = grapheme_at_or_before(text, raw);
    let next = grapheme_at_or_after(text, raw);
    if previous == next {
        return previous;
    }
    let previous_distance = ((shaped.x_for_index(previous) / px(1.0)) - (x / px(1.0))).abs();
    let next_distance = ((shaped.x_for_index(next) / px(1.0)) - (x / px(1.0))).abs();
    if previous_distance < next_distance {
        previous
    } else {
        next
    }
}

/// The range focus mode keeps bright, in absolute document bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FocusView {
    start: usize,
    end: usize,
}

type SentenceCache = RefCell<Option<(u64, usize, Option<Range<usize>>)>>;

/// Install the editor's default key bindings for the host platform.
pub fn install_keybindings(cx: &mut App) {
    let platform = if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Other
    };
    install_keybindings_for(cx, platform);
}

/// Install the editor's default key bindings for a specific platform.
pub fn install_keybindings_for(cx: &mut App, platform: Platform) {
    pilcrow_commands::keymap::install_editor_keybindings_for(cx, platform);
}

/// One live IME composition. The preedit lives in the document, so it wraps
/// like real text; its start, the text it replaced and the selection before it
/// are kept so a commit can become one undo step and a cancel can put the page
/// back without one.
struct Composition {
    /// Byte offset where the composition began.
    start: usize,
    /// The text that occupied the composition's range before the first mark.
    original: String,
    /// Length of the current preedit (or committed text) at `start`.
    current_len: usize,
    /// `(anchor, caret)` before the composition, when a selection was replaced.
    selection_before: Option<(usize, usize)>,
    /// The final text has been applied; the next unmark records the step.
    committed: bool,
}

/// The editable page.
pub struct EditorView {
    buffer: Buffer,
    /// The document's blocks, rescanned after every edit.
    blocks: Vec<Block>,
    /// The placed page, valid for `layout_width` and its own revision.
    layout: Option<Layout>,
    /// The measured text column the layout was built for.
    layout_width: f32,
    /// The column width of the last painted frame.
    last_layout_width: f32,
    caret: usize,
    anchor: Option<usize>,
    /// `(caret, anchor)` as last announced through
    /// [`EditorEvent::SelectionChanged`]. Render compares against it, so
    /// every selection path — keys, mouse drag, find landing, undo — is
    /// announced exactly once without each path emitting by hand.
    announced_selection: (usize, Option<usize>),
    /// App-owned undo/redo. It owns the diffs, not the buffer.
    history: History,
    /// Monotonic epoch for the history's grouping window.
    history_clock: Instant,
    /// The one live composition, if an IME is mid-edit.
    composition: Option<Composition>,
    focus_handle: FocusHandle,
    last_bounds: Option<Bounds<Pixels>>,
    last_page: Option<Page>,
    last_lines: Vec<LineGeometry>,
    last_code_boxes: Vec<Bounds<Pixels>>,
    last_rule_boxes: Vec<Bounds<Pixels>>,
    last_task_boxes: Vec<TaskBox>,
    last_revision: u64,
    last_scroll: f32,
    scroll: f32,
    /// The layout's estimated document height, refreshed by every
    /// `ensure_layout`. Scroll clamps use it so the scrollbar reaches the
    /// end of a long note before the frontier does.
    estimated_total: f32,
    frame_seq: u64,
    /// Frame trace ring for the test driver. Inert (one bool check per paint)
    /// until a trace is started; records are bounded and never persisted.
    trace_active: bool,
    trace_input_seq: u64,
    trace_records: VecDeque<FrameRecord>,
    trace_frame_started: Option<Instant>,
    desired_x: Option<Pixels>,
    marked: Option<Range<usize>>,
    tokens: Tokens,
    /// Reader-selected body scale. Geometry is derived; the bytes never change.
    scale: f32,
    /// Reader-selected measure, in `x` characters.
    measure_chars: usize,
    focus: bool,
    focus_scope: FocusScope,
    typewriter: bool,
    shape_cache: Rc<RefCell<ShapeCache>>,
    measured_column: Cell<f32>,
    stats: Stats,
    stats_revision: u64,
    stats_task: Option<Task<()>>,
    /// Source-byte ranges the system checker considers misspelled. Cleared on
    /// every edit and recomputed only after an idle pause.
    spell: Vec<Range<usize>>,
    spell_revision: u64,
    spell_task: Option<Task<()>>,
    /// `(revision, caret, sentence)` so a still frame does not walk the block
    /// characters again on every paint.
    sentence_cache: SentenceCache,
    /// `(block, byte within block)` to re-wrap from after an edit in a long
    /// block. Cleared when the layout is rebuilt.
    rewrap_block: Option<(usize, usize)>,
    /// A movement asked for the caret to be visible. The flag survives until a
    /// paint has both placed the caret's block and had the chance to scroll.
    reveal_pending: bool,
    mouse_selecting: bool,
    mouse_down_offset: Option<usize>,
    /// Registered lazily at the first drag: losing focus mid-drag ends the
    /// drag instead of leaving `mouse_selecting` stuck.
    blur_subscription: Option<Subscription>,
    find: Option<FindState>,
    /// The last query, replacement and case setting. Closing the bar does not
    /// forget them; the next Find starts from the same search.
    last_find: Option<(String, String, bool)>,
    /// Reading view draws the syntax-free display copy instead of the page.
    reading: bool,
    reading_scroll: ScrollHandle,
}

impl EventEmitter<EditorEvent> for EditorView {}

impl EditorView {
    pub fn new(cx: &mut Context<Self>, text: &str, tokens: Tokens) -> Self {
        let buffer = Buffer::new(text);
        let mut blocks = Vec::new();
        scan_blocks(text, &mut blocks);
        let stats = compute_stats(text, &blocks);
        let mut editor = Self {
            buffer,
            blocks,
            layout: None,
            layout_width: f32::NAN,
            last_layout_width: f32::NAN,
            caret: 0,
            anchor: None,
            announced_selection: (0, None),
            history: History::new(),
            history_clock: Instant::now(),
            composition: None,
            focus_handle: cx.focus_handle(),
            last_bounds: None,
            last_page: None,
            last_lines: Vec::new(),
            last_code_boxes: Vec::new(),
            last_rule_boxes: Vec::new(),
            last_task_boxes: Vec::new(),
            last_revision: 0,
            last_scroll: 0.0,
            scroll: 0.0,
            estimated_total: 0.0,
            frame_seq: 0,
            trace_active: false,
            trace_input_seq: 0,
            trace_records: VecDeque::new(),
            trace_frame_started: None,
            desired_x: None,
            marked: None,
            tokens,
            scale: 1.0,
            measure_chars: MEASURE_CHARS,
            focus: false,
            focus_scope: FocusScope::Paragraph,
            typewriter: false,
            shape_cache: Rc::new(RefCell::new(ShapeCache::new())),
            measured_column: Cell::new(f32::NAN),
            stats,
            stats_revision: 0,
            stats_task: None,
            spell: Vec::new(),
            spell_revision: 0,
            spell_task: None,
            sentence_cache: RefCell::new(None),
            rewrap_block: None,
            reveal_pending: false,
            mouse_selecting: false,
            mouse_down_offset: None,
            blur_subscription: None,
            find: None,
            last_find: None,
            reading: false,
            reading_scroll: ScrollHandle::new(),
        };
        editor.schedule_spellcheck(cx);
        editor
    }

    /// The light page. Convenience for tests and the first app slice.
    pub fn new_light(cx: &mut Context<Self>, text: &str) -> Self {
        Self::new(cx, text, pilcrow_ui::theme::light())
    }

    pub fn tokens(&self) -> Tokens {
        self.tokens
    }

    /// Replace the page palette after a system or user appearance change.
    /// The geometry does not change, but every shaped line carries ink and
    /// background colours, so the visible shape cache is discarded.
    /// Whether an IME composition or preedit is live. Autosave waits for it.
    pub fn has_composition(&self) -> bool {
        self.composition.is_some() || self.marked.is_some()
    }

    pub fn set_tokens(&mut self, tokens: Tokens, cx: &mut Context<Self>) {
        if self.tokens == tokens {
            return;
        }
        self.tokens = tokens;
        self.shape_cache.borrow_mut().lines.clear();
        cx.notify();
    }

    /// The reader-selected text scale.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn measure_chars(&self) -> usize {
        self.measure_chars
    }

    /// The text column width from the last completed layout.
    pub fn laid_out_width(&self) -> f32 {
        self.layout_width
    }

    /// Pick a measure from the supported stops. The page re-measures on the
    /// next paint; the bytes never change.
    pub fn set_measure(&mut self, measure_chars: usize, cx: &mut Context<Self>) {
        let measure_chars = measure_chars.clamp(48, 90);
        if self.measure_chars == measure_chars {
            return;
        }
        self.measure_chars = measure_chars;
        self.layout = None;
        self.measured_column.set(f32::NAN);
        self.shape_cache.borrow_mut().lines.clear();
        self.reveal_pending = true;
        cx.notify();
    }

    /// Change the body scale. Layout, cached shapes and the measuring column
    /// are all derived from it, so they are rebuilt at the next paint.
    pub fn set_scale(&mut self, scale: f32, cx: &mut Context<Self>) {
        let scale = scale.clamp(0.75, 1.75);
        if (self.scale - scale).abs() < f32::EPSILON {
            return;
        }
        self.scale = scale;
        self.layout = None;
        self.measured_column.set(f32::NAN);
        self.shape_cache.borrow_mut().lines.clear();
        self.reveal_pending = true;
        cx.notify();
    }

    pub fn text(&self) -> &str {
        self.buffer.text()
    }

    /// Capture the committed document and its view state as one snapshot.
    pub fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: std::sync::Arc::from(self.buffer.text()),
            revision: self.buffer.revision(),
            caret: self.caret,
            anchor: self.anchor,
            reading: self.reading,
            focus: self.focus,
            typewriter: self.typewriter,
            scale: self.scale,
            measure_chars: self.measure_chars,
        }
    }

    /// The first meaningful source line, without rescanning the document.
    /// This is the pathless title; the blocks are already current.
    pub fn untitled_title(&self) -> String {
        let text = self.buffer.text();
        for block in &self.blocks {
            if matches!(
                block.kind,
                BlockKind::Frontmatter | BlockKind::Blank | BlockKind::Rule
            ) {
                continue;
            }
            let mut line = &text[block.start..block.end];
            if block.kind == BlockKind::Heading {
                line = &line[block.marker.min(line.len())..];
            }
            let line = line.trim();
            let line = line.lines().next().unwrap_or(line).trim();
            if !line.is_empty() {
                return line.to_owned();
            }
        }
        "Untitled".to_owned()
    }

    /// Replace the whole document after an explicit Reload. This is a new
    /// document state, so the old history, composition and layout are dropped.
    pub fn replace_document(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer = Buffer::new(text);
        self.blocks.clear();
        scan_blocks(text, &mut self.blocks);
        self.layout = None;
        self.rewrap_block = None;
        self.history.clear();
        self.composition = None;
        self.marked = None;
        self.caret = 0;
        self.anchor = None;
        self.desired_x = None;
        self.scroll = 0.0;
        self.reveal_pending = true;
        self.stats = compute_stats(text, &self.blocks);
        self.stats_revision = 0;
        self.stats_task = None;
        self.spell.clear();
        self.spell_revision = 0;
        self.spell_task = None;
        self.sentence_cache.borrow_mut().take();
        self.schedule_spellcheck(cx);
        self.shape_cache.borrow_mut().lines.clear();
        // A wholesale replace does not pass through invalidate_after_edit;
        // announce it with the fresh buffer's revision.
        cx.emit(EditorEvent::Edited {
            revision: self.buffer.revision(),
        });
        cx.notify();
    }

    /// Whether the find bar is open.
    pub fn is_finding(&self) -> bool {
        self.find.is_some()
    }

    fn find_editing(&self) -> bool {
        self.find.as_ref().is_some_and(|find| find.editing)
    }

    /// The current query, empty when find is closed.
    pub fn find_query(&self) -> &str {
        self.find.as_ref().map_or("", |find| find.query.as_str())
    }

    /// All current matches, for tests and the paint path.
    pub fn find_matches(&self) -> &[Range<usize>] {
        self.find
            .as_ref()
            .map_or(&[], |find| find.matches.as_slice())
    }

    /// The match the bar is pointing at.
    pub fn active_find_match(&self) -> Option<Range<usize>> {
        let find = self.find.as_ref()?;
        find.matches.get(find.active?).cloned()
    }

    fn refresh_find(&mut self, land: bool, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let text = self.buffer.text();
        find.matches = text_search::find_all(text, &find.query, find.case_sensitive);
        if find.matches.is_empty() {
            find.active = None;
        } else if find
            .active
            .is_none_or(|active| active >= find.matches.len())
        {
            find.active = Some(
                text_search::next_index(&find.matches, self.caret.min(text.len()), true)
                    .unwrap_or(0),
            );
        }
        if land && let Some(window) = window {
            self.land_on_find_match(window, cx);
        }
        cx.notify();
    }

    fn land_on_find_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_ref() else {
            return;
        };
        let Some(range) = find.matches.get(find.active.unwrap_or(0)).cloned() else {
            return;
        };
        self.history.break_group();
        self.anchor = (range.start < range.end).then_some(range.start);
        self.caret = range.end;
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn open_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        // Find points at the source page. Leaving reading mode makes the
        // highlighted match and caret visible instead of searching an image.
        if self.reading {
            self.reading = false;
            cx.emit(EditorEvent::ModeChanged);
        }
        if let Some(find) = self.find.as_mut() {
            find.editing = true;
            find.field = FindField::Query;
            self.refresh_find(true, Some(window), cx);
            return;
        }
        let selected = self
            .selection()
            .map(|range| self.buffer.text()[range].to_owned())
            .filter(|text| !text.is_empty() && !text.contains('\n'));
        let remembered = self.last_find.clone().unwrap_or_default();
        let (query, replacement, case_sensitive) =
            selected.map_or(remembered, |selected| (selected, String::new(), false));
        self.find = Some(FindState::new(
            query,
            replacement,
            FindField::Query,
            case_sensitive,
        ));
        self.refresh_find(true, Some(window), cx);
    }

    fn close_find(&mut self, _: &CloseFind, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.take() else {
            return;
        };
        self.last_find = Some((find.query, find.replacement, find.case_sensitive));
        cx.notify();
    }

    fn find_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let from = if forward {
            self.selection_end()
        } else {
            self.selection_start()
        };
        let Some(find) = self.find.as_mut() else {
            return;
        };
        find.active = text_search::next_index(&find.matches, from, forward);
        self.land_on_find_match(window, cx);
    }

    fn find_next(&mut self, _: &FindNext, window: &mut Window, cx: &mut Context<Self>) {
        self.find_step(true, window, cx);
    }

    fn find_previous(&mut self, _: &FindPrevious, window: &mut Window, cx: &mut Context<Self>) {
        self.find_step(false, window, cx);
    }

    fn find_next_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.find_step(true, window, cx);
    }

    fn find_previous_clicked(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.find_step(false, window, cx);
    }

    fn focus_find_query(
        &mut self,
        _: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(find) = self.find.as_mut() {
            find.editing = true;
            find.field = FindField::Query;
            cx.notify();
        }
    }

    fn focus_find_replace(
        &mut self,
        _: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(find) = self.find.as_mut() {
            find.editing = true;
            find.field = FindField::Replacement;
            cx.notify();
        }
    }

    fn toggle_find_case(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = self.find.as_mut() {
            find.case_sensitive = !find.case_sensitive;
        }
        self.refresh_find(true, Some(window), cx);
    }

    fn close_find_clicked(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.close_find(&CloseFind, window, cx);
    }

    fn insert_find_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let text = match find.field {
            FindField::Query => text.replace(['\n', '\r'], " "),
            FindField::Replacement => text.replace("\r\n", "\n"),
        };
        if text.is_empty() {
            return;
        }
        let cursor = find.cursor();
        find.value_mut().insert_str(cursor, &text);
        *find.cursor_mut() = cursor + text.len();
        let query_changed = find.field == FindField::Query;
        if query_changed {
            self.refresh_find(true, Some(window), cx);
        } else {
            cx.notify();
        }
    }

    fn backspace_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(find) = self.find.as_mut() else {
            return false;
        };
        let cursor = find.cursor();
        if cursor == 0 {
            return true;
        }
        let previous = {
            let value = find.value();
            let mut previous = 0;
            for (index, _) in value.grapheme_indices(true) {
                if index >= cursor {
                    break;
                }
                previous = index;
            }
            previous
        };
        find.value_mut().replace_range(previous..cursor, "");
        *find.cursor_mut() = previous;
        let query_changed = find.field == FindField::Query;
        if query_changed {
            self.refresh_find(true, Some(window), cx);
        } else {
            cx.notify();
        }
        true
    }

    fn delete_find_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(find) = self.find.as_mut() else {
            return false;
        };
        let cursor = find.cursor();
        let value = find.value();
        if cursor >= value.len() {
            return true;
        }
        let end = value[cursor..]
            .graphemes(true)
            .next()
            .map_or(value.len(), |grapheme| cursor + grapheme.len());
        find.value_mut().replace_range(cursor..end, "");
        let query_changed = find.field == FindField::Query;
        if query_changed {
            self.refresh_find(true, Some(window), cx);
        } else {
            cx.notify();
        }
        true
    }

    fn move_find_cursor(
        &mut self,
        delta: isize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(find) = self.find.as_mut() else {
            return false;
        };
        let cursor = find.cursor();
        let value = find.value();
        let target = if delta < 0 {
            let mut previous = 0;
            for (index, _) in value.grapheme_indices(true) {
                if index >= cursor {
                    break;
                }
                previous = index;
            }
            previous
        } else {
            value[cursor..]
                .graphemes(true)
                .next()
                .map_or(value.len(), |grapheme| cursor + grapheme.len())
        };
        *find.cursor_mut() = target;
        cx.notify();
        true
    }

    fn switch_find_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(find) = self.find.as_mut() else {
            return false;
        };
        find.editing = true;
        find.field = match find.field {
            FindField::Query => FindField::Replacement,
            FindField::Replacement => FindField::Query,
        };
        // Keep the point in the document on the active match when returning to
        // the query field.
        let land = find.field == FindField::Query;
        self.refresh_find(land, Some(window), cx);
        true
    }

    fn replace_current_match(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.find.as_ref() else {
            return;
        };
        if find.matches.is_empty() {
            return;
        }
        let replacement = find.replacement.clone();
        if all {
            let ranges = find.matches.clone();
            let text = self.buffer.text();
            let replaced = text_search::replace_ranges(text, &ranges, &replacement);
            if replaced != text {
                let old = text.to_owned();
                self.replace_range(0..old.len(), &replaced, EditGroup::Other, window, cx);
                if let Some(find) = self.find.as_mut() {
                    find.active = Some(0);
                }
            }
            self.refresh_find(true, Some(window), cx);
            return;
        }
        let Some(range) = self.active_find_match() else {
            return;
        };
        let start = range.start;
        self.replace_range(range, &replacement, EditGroup::Other, window, cx);
        let Some(find) = self.find.as_mut() else {
            return;
        };
        find.matches = text_search::find_all(self.buffer.text(), &find.query, find.case_sensitive);
        find.active =
            text_search::next_index(&find.matches, start.saturating_add(replacement.len()), true);
        self.refresh_find(true, Some(window), cx);
    }

    fn replace_one(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_current_match(false, window, cx);
    }

    fn replace_all(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_current_match(true, window, cx);
    }

    fn set_accessibility_selection(
        &mut self,
        anchor_character: usize,
        focus_character: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.buffer.text();
        let anchor = boundary_at_or_before(text, a11y::byte_for_character(text, anchor_character));
        let focus = boundary_at_or_before(text, a11y::byte_for_character(text, focus_character));
        self.commit_composition(window);
        self.history.break_group();
        self.anchor = (anchor != focus).then_some(anchor);
        self.caret = focus;
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_accessibility_selection(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reading {
            return;
        }
        self.commit_composition(window);
        let range = self.selection_start()..self.selection_end();
        self.replace_range(range, text, EditGroup::Typing, window, cx);
    }

    fn set_accessibility_value(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.reading {
            return;
        }
        self.commit_composition(window);
        let len = self.buffer.len();
        self.replace_range(0..len, text, EditGroup::Other, window, cx);
        self.caret = self.buffer.len();
        self.anchor = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn toggle_reading(&mut self, _: &ToggleReading, _window: &mut Window, cx: &mut Context<Self>) {
        self.reading = !self.reading;
        if let Some(find) = self.find.take() {
            self.last_find = Some((find.query, find.replacement, find.case_sensitive));
        }
        self.reading_scroll.set_offset(point(px(0.0), px(0.0)));
        self.reveal_pending = true;
        cx.emit(EditorEvent::ModeChanged);
        cx.notify();
    }

    /// The text an explicit save may write.
    ///
    /// While a composition is live, its preedit bytes are already in the
    /// buffer so the page can paint them, but they are not committed text. This
    /// restores the composition's original bytes for the snapshot; a queued
    /// save can then never write a preedit. A committed composition is final
    /// text and is included.
    pub fn save_text(&self) -> String {
        let Some(composition) = self.composition.as_ref() else {
            return self.buffer.text().to_owned();
        };
        if composition.committed {
            return self.buffer.text().to_owned();
        }
        let start = composition.start.min(self.buffer.len());
        let end = start
            .saturating_add(composition.current_len)
            .min(self.buffer.len());
        let end = boundary_at_or_before(self.buffer.text(), end).max(start);
        let mut text = self.buffer.text().to_owned();
        text.replace_range(start..end, &composition.original);
        text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        self.anchor
            .map(|anchor| self.selection_range(anchor))
            .filter(|range| range.start < range.end)
    }

    /// Whether the editor has an undo step.
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    /// Whether the editor has a redo step.
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    fn selection_range(&self, anchor: usize) -> Range<usize> {
        if anchor <= self.caret {
            anchor..self.caret
        } else {
            self.caret..anchor
        }
    }

    fn selection_start(&self) -> usize {
        self.anchor.map_or(self.caret, |a| a.min(self.caret))
    }

    fn selection_end(&self) -> usize {
        self.anchor.map_or(self.caret, |a| a.max(self.caret))
    }

    fn now_ms(&self) -> u64 {
        self.history_clock
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64
    }

    /// `(anchor, caret)` when the selection is non-empty; `None` for a caret.
    fn selection_snapshot(&self) -> Option<(usize, usize)> {
        self.anchor
            .filter(|anchor| *anchor != self.caret)
            .map(|anchor| (anchor, self.caret))
    }

    /// Whether a live (uncommitted) composition owns the movement keys.
    ///
    /// Native IMEs use arrows to walk the candidate window. Committing the
    /// preedit out from under that interaction loses the composition and makes
    /// the arrow edit the page instead of the candidate list.
    fn ime_owns_movement(&self) -> bool {
        self.composition
            .as_ref()
            .is_some_and(|composition| !composition.committed)
    }

    /// Records the net result of the live composition as one IME step and
    /// clears it. Returns whether the document changed.
    ///
    /// The live preedit is never recorded; only this commit is. One commit is
    /// one undo step, whether the composition replaced a selection or grew
    /// character by character.
    fn finish_composition(&mut self) -> bool {
        self.marked = None;
        let Some(composition) = self.composition.take() else {
            return false;
        };
        let start = composition.start.min(self.buffer.len());
        let end = start
            .saturating_add(composition.current_len)
            .min(self.buffer.len());
        let end = boundary_at_or_before(self.buffer.text(), end).max(start);
        let current = self.buffer.text()[start..end].to_owned();
        if current == composition.original {
            return false;
        }
        self.history.record(
            Edit {
                start,
                removed: composition.original,
                inserted: current,
                selection_before: composition.selection_before,
                selection_after: self.selection_snapshot(),
                group: EditGroup::ImeCommit,
            },
            self.now_ms(),
        );
        true
    }

    /// Puts the composition's original text and selection back. Cancel is not
    /// an edit, so nothing is recorded.
    fn cancel_composition(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        let Some(composition) = self.composition.take() else {
            return;
        };
        let start = composition.start.min(self.buffer.len());
        let end = start
            .saturating_add(composition.current_len)
            .min(self.buffer.len());
        let end = boundary_at_or_before(self.buffer.text(), end).max(start);
        let old_revision = self.buffer.revision();
        self.buffer.replace(start..end, &composition.original);
        let edit_end = start + composition.original.len();
        match composition.selection_before {
            Some((anchor, caret)) => {
                self.anchor = Some(anchor);
                self.caret = caret;
            }
            None => {
                self.anchor = None;
                self.caret = start;
            }
        }
        self.invalidate_after_edit(old_revision, start, edit_end, cx);
    }

    /// Ends a live composition before a normal edit or command. Leaving a
    /// preedit to be overwritten by the next keystroke would lose the one undo
    /// step the commit is owed.
    pub fn commit_composition(&mut self, window: &mut Window) {
        let had_composition = self.composition.is_some() || self.marked.is_some();
        let changed = self.finish_composition();
        self.marked = None;
        if had_composition {
            window.notify_text_input_changed(&self.focus_handle);
        }
        if changed {
            window.notify_text_selection_changed(&self.focus_handle);
        }
    }

    /// Applies one text edit and records its diff exactly once. `group` says
    /// which gesture produced it; history decides whether it can join the
    /// previous step.
    fn replace_range(
        &mut self,
        range: Range<usize>,
        text: &str,
        group: EditGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Reading view is a copy, not a second editor. Every mutation path
        // funnels through here or the composition handlers below.
        if self.reading {
            return;
        }
        let selection_before = self.selection_snapshot();
        let (edit_start, edit_end, removed) = {
            let source = self.buffer.text();
            let mut start = boundary_at_or_before(source, range.start);
            let mut end = boundary_at_or_before(source, range.end);
            if start > end {
                std::mem::swap(&mut start, &mut end);
            }
            (start, end, source[start..end].to_owned())
        };
        let old_revision = self.buffer.revision();
        self.caret = self.buffer.replace(edit_start..edit_end, text);
        if self.buffer.revision() != old_revision {
            self.history.record(
                Edit {
                    start: edit_start,
                    removed,
                    inserted: text.to_owned(),
                    selection_before,
                    selection_after: None,
                    group,
                },
                self.now_ms(),
            );
        }
        self.invalidate_after_edit(old_revision, edit_start, edit_start + text.len(), cx);
        self.anchor = None;
        self.marked = None;
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_input_changed(&self.focus_handle);
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    /// Rescan the blocks from the block before the edit and resume the layout
    /// at the earliest block whose geometry can have changed. `edit_end` is
    /// the byte offset in the new text just past the inserted replacement.
    /// The rescan stops at the first blank line past the edit where it
    /// resynchronises with the old block table (see `rescan_from`), so an
    /// edit in a long note costs about a paragraph of scanning, not the whole
    /// suffix; an unclosed fence still runs to the end. The block before the
    /// edit catches merges and structural edits that start on the previous
    /// line, and the edited block catches same-length content replacements.
    fn invalidate_after_edit(
        &mut self,
        old_revision: u64,
        edit_start: usize,
        edit_end: usize,
        cx: &mut Context<Self>,
    ) {
        if self.buffer.revision() == old_revision {
            return;
        }
        // Every buffer mutation funnels through here, so this is the one
        // place Edited is announced. Subscribers run after this update;
        // nothing on the typing path waits for them.
        cx.emit(EditorEvent::Edited {
            revision: self.buffer.revision(),
        });
        let edit_block = block_index_for_offset(&self.blocks, edit_start);
        let mut scan_from = edit_block.saturating_sub(1);
        // Starting inside a fence or frontmatter would parse its content as
        // prose, and starting at a closing fence would reopen it. Walk back
        // to the first block of the run, whose opening line re-establishes
        // the scanner state.
        while scan_from > 0 {
            let Some(current) = self.blocks.get(scan_from) else {
                break;
            };
            if !matches!(current.kind, BlockKind::Code | BlockKind::Frontmatter) {
                break;
            }
            let Some(previous) = self.blocks.get(scan_from - 1) else {
                break;
            };
            if previous.kind != current.kind {
                break;
            }
            scan_from -= 1;
        }
        let mut next_blocks = Vec::with_capacity(self.blocks.len());
        rescan_from(
            self.buffer.text(),
            &self.blocks,
            scan_from,
            edit_end,
            &mut next_blocks,
        );

        let revision = self.buffer.revision();
        let old_block = self.blocks.get(edit_block).copied();
        let structural_same = old_block.is_some_and(|old| {
            next_blocks.get(edit_block).is_some_and(|new| {
                old.kind == new.kind && old.level == new.level && old.marker == new.marker
            })
        });
        let previous_same =
            scan_from == edit_block || self.blocks.get(scan_from) == next_blocks.get(scan_from);
        let resume = if previous_same { edit_block } else { scan_from };
        if self.layout.as_ref().is_some_and(|layout| {
            layout.matches_scaled(old_revision, self.layout_width, self.scale)
        }) {
            if let Some(layout) = self.layout.as_mut() {
                if previous_same
                    && structural_same
                    && let Some(old) = old_block
                    && let Some(from) = layout.truncate_block_from_offset(
                        edit_block,
                        edit_start.saturating_sub(old.start),
                    )
                {
                    self.rewrap_block = Some((edit_block, from));
                } else {
                    layout.truncate_from(resume);
                    self.rewrap_block = None;
                }
                layout.set_revision(revision);
            }
        } else {
            // There is no usable layout at this width/revision. Install a fresh
            // one at the next prepaint rather than trusting a stale frontier.
            self.layout = None;
            self.rewrap_block = None;
        }
        self.blocks = next_blocks;
        self.refresh_find(false, None, cx);
        self.schedule_stats(cx);
        self.spell.clear();
        self.schedule_spellcheck(cx);
    }

    /// Recompute counts after a typing pause. Replacing the task on every edit
    /// cancels the previous timer, so the count never runs mid-keystroke.
    fn schedule_stats(&mut self, cx: &mut Context<Self>) {
        let revision = self.buffer.revision();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let Some((text, blocks)) = this
                .update(cx, |editor, _| {
                    (editor.buffer.revision() == revision)
                        .then(|| (editor.buffer.text().to_owned(), editor.blocks.clone()))
                })
                .ok()
                .flatten()
            else {
                return;
            };
            // Count off the UI thread. A 1M-word note is measurable work, and
            // the page must not hitch after a pause either.
            let stats = cx
                .background_executor()
                .spawn(async move { compute_stats(&text, &blocks) })
                .await;
            this.update(cx, |editor, cx| {
                if editor.buffer.revision() == revision {
                    editor.stats = stats;
                    editor.stats_revision = revision;
                    cx.emit(EditorEvent::StatsChanged);
                    cx.notify();
                }
            })
            .ok();
        });
        self.stats_task = Some(task);
    }

    fn schedule_spellcheck(&mut self, cx: &mut Context<Self>) {
        let revision = self.buffer.revision();
        if self.spell_task.is_some() && self.spell_revision == revision {
            return;
        }
        self.spell_revision = revision;
        self.spell_task = None;
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(650))
                .await;
            // One chunk per frame. The platform checker only runs on this
            // thread, so a single whole-document pass over a long note is a
            // hitch arriving half a second after the typing pause; spreading
            // it keeps every frame inside budget while the squiggles fill in.
            let mut from = 0;
            loop {
                let next = this
                    .update(cx, |editor, cx| {
                        (editor.buffer.revision() == revision)
                            .then(|| editor.spell_pass_chunk(from, cx))
                    })
                    .ok()
                    .flatten();
                let Some(next) = next.flatten() else {
                    return;
                };
                from = next;
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
            }
        });
        self.spell_task = Some(task);
    }

    /// Check one chunk of the document starting at byte `from`, appending its
    /// ranges to the current marks. Returns the next chunk's start, or `None`
    /// when the pass reached the end of the text. A chunk starting at zero
    /// begins a fresh pass and clears the old marks.
    fn spell_pass_chunk(&mut self, from: usize, cx: &mut Context<Self>) -> Option<usize> {
        let revision = self.buffer.revision();
        let (end, ranges, done) = {
            let text = self.buffer.text();
            let end = spell::chunk_end(text, from, spell::SPELL_CHUNK_BYTES);
            let ranges = spell::prose_ranges_in(text, &self.blocks, from..end);
            (end, ranges, end >= text.len())
        };
        if from == 0 {
            self.spell.clear();
        }
        self.spell.extend(ranges);
        self.spell_revision = revision;
        cx.notify();
        if done { None } else { Some(end) }
    }

    /// The whole spell pass in one call. Production spreads the same chunks
    /// across frames from `schedule_spellcheck`; tests use this to assert the
    /// final marks without walking the simulated clock.
    #[cfg(all(test, target_os = "macos"))]
    fn refresh_spellcheck(&mut self, cx: &mut Context<Self>) {
        let mut from = 0;
        while let Some(next) = self.spell_pass_chunk(from, cx) {
            from = next;
        }
    }

    fn edit_selection_or(
        &mut self,
        text: &str,
        group: EditGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.selection_start()..self.selection_end();
        self.replace_range(range, text, group, window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            self.backspace_find(window, cx);
            return;
        }
        self.commit_composition(window);
        if self.selection_start() < self.selection_end() {
            self.edit_selection_or("", EditGroup::Backspace, window, cx);
            return;
        }
        if self.caret == 0 {
            return;
        }
        let from = prev_grapheme(self.buffer.text(), self.caret);
        self.replace_range(from..self.caret, "", EditGroup::Backspace, window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            self.delete_find_forward(window, cx);
            return;
        }
        self.commit_composition(window);
        if self.selection_start() < self.selection_end() {
            self.edit_selection_or("", EditGroup::Delete, window, cx);
            return;
        }
        if self.caret >= self.buffer.len() {
            return;
        }
        let to = next_grapheme(self.buffer.text(), self.caret);
        self.replace_range(self.caret..to, "", EditGroup::Delete, window, cx);
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            self.move_find_cursor(-1, window, cx);
            return;
        }
        if self.ime_owns_movement() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        if self.anchor.is_some() && self.selection_start() == self.selection_end() {
            self.anchor = None;
        }
        if self.selection_start() < self.selection_end() {
            self.caret = self.selection_start();
            self.anchor = None;
        } else {
            self.caret = prev_grapheme(self.buffer.text(), self.caret);
        }
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            self.move_find_cursor(1, window, cx);
            return;
        }
        if self.ime_owns_movement() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        if self.anchor.is_some() && self.selection_start() == self.selection_end() {
            self.anchor = None;
        }
        if self.selection_start() < self.selection_end() {
            self.caret = self.selection_end();
            self.anchor = None;
        } else {
            self.caret = next_grapheme(self.buffer.text(), self.caret);
        }
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.ime_owns_movement() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        if self.anchor.is_none() {
            self.anchor = Some(self.caret);
        }
        self.caret = prev_grapheme(self.buffer.text(), self.caret);
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.ime_owns_movement() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        if self.anchor.is_none() {
            self.anchor = Some(self.caret);
        }
        self.caret = next_grapheme(self.buffer.text(), self.caret);
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn up(&mut self, _: &Up, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            self.find_step(false, window, cx);
            return;
        }
        if self.ime_owns_movement() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        self.move_line(-1, false, window, cx);
    }

    fn down(&mut self, _: &Down, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            self.find_step(true, window, cx);
            return;
        }
        if self.ime_owns_movement() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        self.move_line(1, false, window, cx);
    }

    /// Shape one placed line on demand. Prepaint has its own cache and block
    /// kinds; this path is for a movement target that has not been painted.
    fn shape_placed_line(&self, window: &mut Window, index: usize) -> (SharedString, ShapedLine) {
        let Some(layout) = self.layout.as_ref() else {
            return (SharedString::new(""), ShapedLine::default());
        };
        let Some(placed) = layout.lines().get(index).copied() else {
            return (SharedString::new(""), ShapedLine::default());
        };
        let Some(block) = self.blocks.get(placed.block as usize).copied() else {
            return (SharedString::new(""), ShapedLine::default());
        };
        let full = &self.buffer.text()[block.start..block.end];
        let mut kinds = vec![InlineKind::Plain; full.len()];
        if block.kind != BlockKind::Code {
            inline_kinds(full, &mut kinds);
        }
        apply_block_markers(block, full, &mut kinds);
        let links = if block.kind == BlockKind::Code {
            Vec::new()
        } else {
            link_text_ranges(full)
        };
        let (text, runs) = text_runs_for_line(
            self.buffer.text(),
            block,
            placed,
            Some(&kinds),
            &links,
            self.marked.as_ref(),
            self.focus_view(),
            &self.tokens,
            self.scale,
        );
        let shaped = window.text_system().shape_line(
            text.clone(),
            px(style::look_scaled(block, self.scale).size),
            &runs,
            None,
        );
        (text, shaped)
    }

    /// The page's measure: the selected number of `x` characters in the face
    /// that paints the page. Scale and measure changes invalidate the cache.
    fn measure_column(&self, window: &Window) -> f32 {
        let cached = self.measured_column.get();
        if cached.is_finite() {
            return cached;
        }
        let ruler = SharedString::new("x".repeat(self.measure_chars));
        let run = TextRun {
            len: ruler.len(),
            font: font_for(MeasureStyle {
                size: BODY_SIZE * self.scale,
                bold: false,
                italic: false,
                mono: false,
            }),
            color: self.tokens.ink,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let width = window
            .text_system()
            .shape_line(ruler, FONT_SIZE * self.scale, &[run], None)
            .width()
            / px(1.0);
        self.measured_column.set(width);
        width
    }

    /// Make sure the placed page covers the caret's block and the viewport
    /// plus one page of run-up. The work is bounded: if the caret is at the far
    /// end of a very long note, the next frame continues from the frontier.
    fn ensure_layout(&mut self, window: &mut Window, viewport: f32) -> f32 {
        let column = self.measure_column(window);
        self.layout_width = column;
        let revision = self.buffer.revision();
        if self
            .layout
            .as_ref()
            .is_none_or(|layout| !layout.matches_scaled(revision, column, self.scale))
        {
            self.layout = Some(Layout::begin_at_scaled(
                &self.blocks,
                column,
                revision,
                self.scale,
            ));
        }
        let caret_block = block_index_for_offset(&self.blocks, self.caret);
        let target_y = self.scroll + viewport * 2.0;
        let text = self.buffer.text();
        let blocks = &self.blocks;
        let Some(layout) = self.layout.as_mut() else {
            return column;
        };
        let measurer = WindowMeasurer::new(window.text_system());
        if let Some((block, from)) = self.rewrap_block.take()
            && !layout.rewrap_block_from(text, blocks, &measurer, block, from)
        {
            layout.truncate_from(block);
        }
        // a caret or scroll far outside the placed window reseeds
        // placement near its target from the estimated prefix instead of
        // wrapping every block in between over many frames. The caret
        // wins over the scroll: it is where the next keystroke lands.
        let window_start = layout.window_start();
        let caret_beyond =
            caret_block >= layout.next_block() && caret_block - layout.next_block() > LAYOUT_BUDGET;
        let caret_above = caret_block < window_start
            || (window_start > 0 && caret_block < window_start + LAYOUT_LOOKBACK / 2);
        if caret_beyond || caret_above {
            layout.jump_to_block(
                blocks,
                &measurer,
                caret_block.saturating_sub(LAYOUT_LOOKBACK),
            );
        } else {
            let scroll_block = layout.estimated_block_at(blocks, &measurer, self.scroll);
            let scroll_beyond = scroll_block >= layout.next_block()
                && scroll_block - layout.next_block() > LAYOUT_BUDGET;
            if scroll_beyond || scroll_block < layout.window_start() {
                layout.jump_to_block(
                    blocks,
                    &measurer,
                    scroll_block.saturating_sub(LAYOUT_LOOKBACK / 2),
                );
            }
        }
        let mut spent = 0usize;
        while !layout.done() && layout.next_block() <= caret_block && spent < LAYOUT_BUDGET {
            let before = layout.next_block();
            layout.extend(text, blocks, &measurer, LAYOUT_CHUNK);
            spent += layout.next_block().saturating_sub(before);
            if layout.next_block() == before {
                break;
            }
        }
        while !layout.done() && layout.total() < target_y && spent < LAYOUT_BUDGET {
            let before = layout.next_block();
            layout.extend(text, blocks, &measurer, LAYOUT_CHUNK);
            spent += layout.next_block().saturating_sub(before);
            if layout.next_block() == before {
                break;
            }
        }
        // Keep a long document filling in the background after the viewport
        // and caret are covered. The paint requests the next frame.
        if !layout.done() {
            layout.extend(text, blocks, &measurer, LAYOUT_BACKGROUND);
        }
        self.estimated_total = layout.estimated_total(blocks, &measurer);
        column
    }

    /// The document y and height of the caret's line, or of the block's top
    /// when the block has no glyph line (blank, rule, or the empty document).
    fn caret_band(&self, layout: &Layout) -> Option<(f32, f32)> {
        let block_index = block_index_for_offset(&self.blocks, self.caret);
        let block = self.blocks.get(block_index)?;
        let local = self
            .caret
            .saturating_sub(block.start)
            .min(block.end - block.start);
        let mut last = None;
        for placed in layout
            .lines()
            .iter()
            .filter(|placed| placed.block as usize == block_index && !placed.rule)
        {
            last = Some((placed.y, placed.h));
            if local <= placed.end as usize {
                return Some((placed.y, placed.h));
            }
        }
        last.or_else(|| {
            layout.block_top(block_index).map(|y| {
                (
                    y,
                    style::line_height(style::look_scaled(*block, self.scale)),
                )
            })
        })
    }

    /// The visual line that owns `offset`. A caret at a wrap boundary belongs
    /// to the line whose range reaches it; a caret in swallowed break
    /// whitespace belongs to the next line.
    fn placed_line_index(&self, layout: &Layout, offset: usize) -> Option<usize> {
        let block_index = block_index_for_offset(&self.blocks, offset);
        let block = self.blocks.get(block_index)?;
        let local = offset
            .saturating_sub(block.start)
            .min(block.end - block.start);
        let mut last = None;
        for (index, placed) in layout.lines().iter().enumerate() {
            if placed.block as usize != block_index || placed.rule {
                continue;
            }
            last = Some(index);
            if local <= placed.end as usize {
                return Some(index);
            }
        }
        last
    }

    /// The caret's x relative to the column's left edge.
    fn column_x_for_offset(&self, window: &mut Window, offset: usize) -> Option<Pixels> {
        let layout = self.layout.as_ref()?;
        let index = self.placed_line_index(layout, offset)?;
        let placed = *layout.lines().get(index)?;
        let block = self.blocks.get(placed.block as usize)?;
        let local = offset
            .saturating_sub(block.start)
            .clamp(placed.start as usize, placed.end as usize)
            - placed.start as usize;
        if let Some(line) = self.last_lines.iter().find(|line| {
            line.index == index
                && line.block == placed.block as usize
                && line.start == block.start + placed.start as usize
                && line.end == block.start + placed.end as usize
        }) {
            return Some(px(placed.x) + line.shaped.x_for_index(local));
        }
        let (text, shaped) = self.shape_placed_line(window, index);
        let local = local.min(text.len());
        Some(px(placed.x) + shaped.x_for_index(local))
    }

    /// Every place a vertical caret move may land: one per placed text
    /// line. Blank blocks are represented by their gap, not by extra stops;
    /// moving from a blank uses `caret_band`'s block top. Rules are skipped.
    fn caret_stops(&self, layout: &Layout) -> Vec<CaretStop> {
        let mut stops = Vec::with_capacity(layout.lines().len());
        for (index, placed) in layout.lines().iter().enumerate() {
            if placed.rule {
                continue;
            }
            stops.push(CaretStop {
                block: placed.block as usize,
                line: Some(index),
                y: placed.y,
            });
        }
        stops
    }

    fn move_line(
        &mut self,
        delta: isize,
        select: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = self
            .viewport()
            .map(|(_, height)| height)
            .unwrap_or_default();
        self.ensure_layout(window, viewport);
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        // If the caret is beyond the built frontier, a vertical move has no
        // honest target yet. Keep the reveal pending; the next frame will
        // finish placing the caret's block instead of teleporting to y=0.
        let caret_block = block_index_for_offset(&self.blocks, self.caret);
        if !layout.done() && layout.next_block() <= caret_block {
            return;
        }
        let stops = self.caret_stops(layout);
        if stops.is_empty() {
            return;
        }
        // Pick by document y rather than by offset, so a caret parked on a
        // rule or in a block gap still moves to the text line after it.
        let current_y = self.caret_band(layout).map_or(0.0, |(y, _)| y);
        let target = if delta >= 0 {
            stops.partition_point(|stop| stop.y <= current_y)
        } else {
            stops
                .partition_point(|stop| stop.y < current_y)
                .saturating_sub(1)
        }
        .min(stops.len() - 1);
        let stop = stops[target];
        let desired_x = self.desired_x.unwrap_or_else(|| {
            self.column_x_for_offset(window, self.caret)
                .unwrap_or(px(0.0))
        });
        let Some(block) = self.blocks.get(stop.block).copied() else {
            return;
        };
        let old_caret = self.caret;
        self.caret = match stop.line {
            Some(index) => {
                let Some(placed) = self
                    .layout
                    .as_ref()
                    .and_then(|l| l.lines().get(index))
                    .copied()
                else {
                    return;
                };
                let cached = self.last_lines.iter().find(|line| {
                    line.index == index
                        && line.block == stop.block
                        && line.start == block.start + placed.start as usize
                        && line.end == block.start + placed.end as usize
                });
                let (_text, shaped) = match cached {
                    Some(line) => (line.shaped.text.clone(), line.shaped.clone()),
                    None => self.shape_placed_line(window, index),
                };
                let segment_start = block.start + placed.start as usize;
                let segment_end = block.start + placed.end as usize;
                let segment = &self.buffer.text()[segment_start..segment_end];
                let local = closest_grapheme_index(&shaped, segment, desired_x - px(placed.x));
                boundary_at_or_before(self.buffer.text(), segment_start + local)
            }
            None => block.start,
        };
        if select {
            if self.anchor.is_none() {
                self.anchor = Some(old_caret);
            }
        } else {
            self.anchor = None;
        }
        self.desired_x = Some(desired_x);
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = self.find.as_mut().filter(|find| find.editing) {
            *find.cursor_mut() = 0;
            cx.notify();
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        self.caret = self.buffer.line_start(self.buffer.line_of(self.caret));
        self.anchor = None;
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            let Some(find) = self.find.as_mut() else {
                return;
            };
            let len = find.value().len();
            *find.cursor_mut() = len;
            cx.notify();
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        self.caret = self.buffer.line_end(self.buffer.line_of(self.caret));
        self.anchor = None;
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_editing() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        self.anchor = Some(0);
        self.caret = self.buffer.len();
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn doc_start(&mut self, _: &DocStart, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_composition(window);
        self.history.break_group();
        self.caret = 0;
        self.anchor = None;
        self.desired_x = None;
        self.scroll = 0.0;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn doc_end(&mut self, _: &DocEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_composition(window);
        self.history.break_group();
        self.caret = self.buffer.len();
        self.anchor = None;
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn insert_newline(&mut self, _: &InsertNewline, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = self.find.as_ref().filter(|find| find.editing) {
            if find.field == FindField::Replacement {
                self.insert_find_text("\n", window, cx);
            } else {
                self.find_step(true, window, cx);
            }
            return;
        }
        self.commit_composition(window);
        if self.selection_start() != self.selection_end() {
            self.edit_selection_or("\n", EditGroup::Other, window, cx);
            return;
        }
        let text = self.buffer.text();
        let line_start = text[..self.caret].rfind('\n').map_or(0, |index| index + 1);
        let line_end = text[self.caret..]
            .find('\n')
            .map_or(text.len(), |offset| self.caret + offset);
        if self.caret != line_end {
            self.replace_range(self.caret..self.caret, "\n", EditGroup::Other, window, cx);
            return;
        }
        let block_index = block_index_for_offset(&self.blocks, line_start);
        if self
            .blocks
            .get(block_index)
            .is_none_or(|block| block.kind != BlockKind::ListItem)
        {
            self.replace_range(self.caret..self.caret, "\n", EditGroup::Other, window, cx);
            return;
        }
        let body = &text[line_start..line_end];
        let Some(item) = lists::parse(body.as_bytes()) else {
            self.replace_range(self.caret..self.caret, "\n", EditGroup::Other, window, cx);
            return;
        };
        if body[item.marker_end..].trim_matches([' ', '\t']).is_empty() {
            // Enter on an empty item closes the list instead of leaving an
            // empty marker behind.
            self.replace_range(line_start..line_end, "", EditGroup::Other, window, cx);
            return;
        }
        let marker = lists::next_marker(body, item);
        self.replace_range(
            self.caret..self.caret,
            &format!("\n{marker}"),
            EditGroup::Other,
            window,
            cx,
        );
    }

    fn insert_tab(&mut self, _: &InsertTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.as_ref().is_some_and(|find| find.editing) {
            self.switch_find_field(window, cx);
            return;
        }
        self.commit_composition(window);
        self.edit_selection_or("\t", EditGroup::Other, window, cx);
    }

    fn move_word(
        &mut self,
        delta: isize,
        select: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find_editing() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        if select && self.anchor.is_none() {
            self.anchor = Some(self.caret);
        } else if !select {
            self.anchor = None;
        }
        self.caret = if delta < 0 {
            prev_word(self.buffer.text(), self.caret)
        } else {
            next_word(self.buffer.text(), self.caret)
        };
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn delete_word(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_editing() {
            if forward {
                self.delete_find_forward(window, cx);
            } else {
                self.backspace_find(window, cx);
            }
            return;
        }
        self.commit_composition(window);
        if self.selection_start() < self.selection_end() {
            self.edit_selection_or("", EditGroup::Delete, window, cx);
            return;
        }
        let (from, to) = if forward {
            (self.caret, next_word(self.buffer.text(), self.caret))
        } else {
            (prev_word(self.buffer.text(), self.caret), self.caret)
        };
        if from != to {
            self.replace_range(from..to, "", EditGroup::Delete, window, cx);
        }
    }

    fn delete_to_line(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_editing() {
            if let Some(find) = self.find.as_mut() {
                let cursor = find.cursor().min(find.value().len());
                if forward {
                    find.value_mut().truncate(cursor);
                } else {
                    find.value_mut().replace_range(..cursor, "");
                    *find.cursor_mut() = 0;
                }
                let query_changed = find.field == FindField::Query;
                if query_changed {
                    self.refresh_find(true, Some(window), cx);
                } else {
                    cx.notify();
                }
            }
            return;
        }
        self.commit_composition(window);
        if self.selection_start() < self.selection_end() {
            self.edit_selection_or("", EditGroup::Delete, window, cx);
            return;
        }
        let line = self.buffer.line_of(self.caret);
        let target = if forward {
            self.buffer.line_end(line)
        } else {
            self.buffer.line_start(line)
        };
        let (from, to) = (self.caret.min(target), self.caret.max(target));
        if from != to {
            self.replace_range(from..to, "", EditGroup::Delete, window, cx);
        }
    }

    fn move_page(
        &mut self,
        delta: isize,
        select: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find_editing() || self.ime_owns_movement() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        let step = self
            .last_bounds
            .map(|bounds| {
                ((bounds.size.height - HEAD_PX) / (LINE_HEIGHT * self.scale))
                    .floor()
                    .clamp(1.0, 40.0) as usize
            })
            .unwrap_or(20);
        for _ in 0..step {
            self.move_line(delta, select, window, cx);
        }
    }

    fn select_line_edge(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_editing() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        if self.anchor.is_none() {
            self.anchor = Some(self.caret);
        }
        let line = self.buffer.line_of(self.caret);
        self.caret = if forward {
            self.buffer.line_end(line)
        } else {
            self.buffer.line_start(line)
        };
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn select_doc_edge(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_editing() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        if self.anchor.is_none() {
            self.anchor = Some(self.caret);
        }
        self.caret = if forward { self.buffer.len() } else { 0 };
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn apply_format(
        &mut self,
        command: FormatCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find_editing() {
            return;
        }
        self.commit_composition(window);
        let from = self.selection_start();
        let to = self.selection_end();
        let Some(edit) = format_command(self.buffer.text(), from, to, self.caret, command) else {
            return;
        };
        self.replace_range(edit.range, &edit.replacement, EditGroup::Format, window, cx);
        match edit.after {
            pilcrow_core::FormatPlacement::Caret(at) => {
                self.caret = boundary_at_or_before(self.buffer.text(), at);
                self.anchor = None;
            }
            pilcrow_core::FormatPlacement::Selection { anchor, caret } => {
                self.anchor = Some(boundary_at_or_before(self.buffer.text(), anchor));
                self.caret = boundary_at_or_before(self.buffer.text(), caret);
            }
        }
        self.history
            .set_last_selection_after(Some((self.anchor.unwrap_or(self.caret), self.caret)));
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn format_bold(&mut self, _: &FormatBold, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Bold, window, cx);
    }

    fn format_italic(&mut self, _: &FormatItalic, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Italic, window, cx);
    }

    fn format_strike(&mut self, _: &FormatStrike, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Strike, window, cx);
    }

    fn format_code(&mut self, _: &FormatCode, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Code, window, cx);
    }

    fn format_link(&mut self, _: &FormatLink, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Link, window, cx);
    }

    fn heading_1(&mut self, _: &Heading1, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Heading(1), window, cx);
    }

    fn heading_2(&mut self, _: &Heading2, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Heading(2), window, cx);
    }

    fn heading_3(&mut self, _: &Heading3, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Heading(3), window, cx);
    }

    fn heading_4(&mut self, _: &Heading4, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Heading(4), window, cx);
    }

    fn heading_5(&mut self, _: &Heading5, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Heading(5), window, cx);
    }

    fn heading_6(&mut self, _: &Heading6, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Heading(6), window, cx);
    }

    fn bullet_list(&mut self, _: &BulletList, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Bullet, window, cx);
    }

    fn ordered_list(&mut self, _: &OrderedList, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Ordered, window, cx);
    }

    fn task_list(&mut self, _: &TaskList, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Task, window, cx);
    }

    fn quote(&mut self, _: &Quote, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_format(FormatCommand::Quote, window, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = self.find.as_ref().filter(|find| find.editing) {
            cx.write_to_clipboard(ClipboardItem::new_string(find.value().to_owned()));
            return;
        }
        let text = self.buffer.text()[self.selection_start()..self.selection_end()].to_owned();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_editing() {
            let Some(find) = self.find.as_mut() else {
                return;
            };
            cx.write_to_clipboard(ClipboardItem::new_string(find.value().to_owned()));
            find.value_mut().clear();
            *find.cursor_mut() = 0;
            let query_changed = find.field == FindField::Query;
            if query_changed {
                self.refresh_find(true, Some(window), cx);
            } else {
                cx.notify();
            }
            return;
        }
        self.commit_composition(window);
        let text = self.buffer.text()[self.selection_start()..self.selection_end()].to_owned();
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.edit_selection_or("", EditGroup::Cut, window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if self.reading {
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if self.find.as_ref().is_some_and(|find| find.editing) {
            self.insert_find_text(&text, window, cx);
            return;
        }
        self.paste_text(&text, window, cx);
    }

    fn paste_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        self.commit_composition(window);
        // The buffer is LF; a paste from another app may carry CRLF.
        let text = text.replace("\r\n", "\n");
        self.edit_selection_or(&text, EditGroup::Paste, window, cx);
    }

    fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        if self.reading {
            return;
        }
        self.commit_composition(window);
        let old_revision = self.buffer.revision();
        let Some(applied) = self.history.undo(&mut self.buffer) else {
            return;
        };
        self.after_history(applied, old_revision, window, cx);
    }

    fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        if self.reading {
            return;
        }
        self.commit_composition(window);
        let old_revision = self.buffer.revision();
        let Some(applied) = self.history.redo(&mut self.buffer) else {
            return;
        };
        self.after_history(applied, old_revision, window, cx);
    }

    fn after_history(
        &mut self,
        applied: Applied,
        old_revision: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.caret = applied.caret;
        self.anchor = applied.anchor;
        self.marked = None;
        self.composition = None;
        self.desired_x = None;
        self.invalidate_after_edit(old_revision, applied.start, applied.end, cx);
        self.reveal_caret();
        window.notify_text_input_changed(&self.focus_handle);
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    /// The page's left text edge. A heading marker may hang left of it; when
    /// the margin is narrower than the widest hang the column shifts right so
    /// the marker still begins inside the window.
    fn page_left(&self, bounds: Bounds<Pixels>) -> Pixels {
        let hang = self.layout.as_ref().map_or(0.0, Layout::max_hang);
        let page = self
            .last_page
            .unwrap_or_else(|| page_for(bounds.size.width / px(1.0), self.layout_width.max(0.0)));
        bounds.left() + px(page.margin.max(hang))
    }

    fn offset_in_geometry(&self, line: &LineGeometry, x: Pixels) -> usize {
        // Grapheme boundaries come from the file, not the painted string: a
        // soft newline is painted as a space, but a CRLF is still one cluster.
        let text = &self.buffer.text()[line.start..line.end];
        let local = closest_grapheme_index(&line.shaped, text, x - line.bounds.left());
        boundary_at_or_before(
            self.buffer.text(),
            line.start.saturating_add(local).min(line.end),
        )
    }

    /// Map a window point to a document byte. The y lands on a placed line
    /// when one covers it; otherwise the block table supplies the blank line
    /// or rule the click fell on.
    fn hit_test_offset(&self, position: gpui::Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let page = self.last_page?;
        let layout = self.layout.as_ref()?;
        let page_left = self.page_left(bounds);
        let column_left = page_left - px(layout.max_hang());
        let column_right = page_left + px(page.column);
        if !self.layout_is_current()
            || position.y < bounds.top() + HEAD_PX
            || position.y > bounds.bottom()
            || position.x < column_left
            || position.x > column_right
        {
            return None;
        }
        self.offset_for_point(position)
    }

    /// A drag keeps selecting even when the pointer leaves the measured
    /// column or the page: clamp to the nearest text edge instead of freezing.
    fn offset_for_drag(&self, position: gpui::Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let page = self.last_page?;
        let layout = self.layout.as_ref()?;
        let page_left = self.page_left(bounds);
        let column_left = page_left - px(layout.max_hang());
        let column_right = page_left + px(page.column);
        let x = clamp_pixels(position.x, column_left, column_right - px(1.0));
        let y = clamp_pixels(
            position.y,
            bounds.top() + HEAD_PX,
            bounds.bottom() - px(1.0),
        );
        self.offset_for_point(point(x, y))
    }

    fn offset_for_point(&self, position: gpui::Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let layout = self.layout.as_ref()?;
        let y = (position.y - bounds.top() - HEAD_PX + px(self.scroll)) / px(1.0);
        if let Some(line) = self
            .last_lines
            .iter()
            .find(|line| !line.rule && y >= line.doc_y && y < line.doc_y + line.h)
        {
            return Some(self.offset_in_geometry(line, position.x));
        }

        let block_index = layout.block_at(y)?;
        let block = self.blocks.get(block_index).copied()?;
        let line = self
            .last_lines
            .iter()
            .filter(|line| line.block == block_index && !line.rule)
            .min_by(|a, b| {
                let distance = |line: &LineGeometry| {
                    if y < line.doc_y {
                        line.doc_y - y
                    } else if y > line.doc_y + line.h {
                        y - (line.doc_y + line.h)
                    } else {
                        0.0
                    }
                };
                distance(a).total_cmp(&distance(b))
            });
        match line {
            Some(line) => Some(self.offset_in_geometry(line, position.x)),
            None => Some(block.start),
        }
    }

    fn toggle_focus(&mut self, _: &ToggleFocus, _window: &mut Window, cx: &mut Context<Self>) {
        let on = !self.focus;
        self.set_focus_mode(on, cx);
    }

    fn focus_paragraph(
        &mut self,
        _: &FocusParagraph,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_focus_scope(FocusScope::Paragraph, cx);
    }

    fn focus_sentence(&mut self, _: &FocusSentence, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_focus_scope(FocusScope::Sentence, cx);
    }

    fn toggle_typewriter(
        &mut self,
        _: &ToggleTypewriter,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on = !self.typewriter;
        self.set_typewriter(on, cx);
    }

    fn word_left(&mut self, _: &WordLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.move_word(-1, false, window, cx);
    }

    fn word_right(&mut self, _: &WordRight, window: &mut Window, cx: &mut Context<Self>) {
        self.move_word(1, false, window, cx);
    }

    fn select_word_left(
        &mut self,
        _: &SelectWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_word(-1, true, window, cx);
    }

    fn select_word_right(
        &mut self,
        _: &SelectWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_word(1, true, window, cx);
    }

    fn backspace_word(&mut self, _: &BackspaceWord, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_word(false, window, cx);
    }

    fn delete_word_forward(&mut self, _: &DeleteWord, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_word(true, window, cx);
    }

    fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_editing() || self.ime_owns_movement() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        self.move_line(-1, true, window, cx);
    }

    fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.find_editing() || self.ime_owns_movement() {
            return;
        }
        self.commit_composition(window);
        self.history.break_group();
        self.move_line(1, true, window, cx);
    }

    fn select_home(&mut self, _: &SelectHome, window: &mut Window, cx: &mut Context<Self>) {
        self.select_line_edge(false, window, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.select_line_edge(true, window, cx);
    }

    fn select_doc_start(
        &mut self,
        _: &SelectDocStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_doc_edge(false, window, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.select_doc_edge(true, window, cx);
    }

    fn page_up(&mut self, _: &PageUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.reading {
            self.reading_page_step(-1.0, cx);
            return;
        }
        self.move_page(-1, false, window, cx);
    }

    fn page_down(&mut self, _: &PageDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.reading {
            self.reading_page_step(1.0, cx);
            return;
        }
        self.move_page(1, false, window, cx);
    }

    fn reading_page_step(&mut self, direction: f32, cx: &mut Context<Self>) {
        let viewport = self
            .last_bounds
            .map_or(600.0, |bounds| bounds.size.height / px(1.0));
        let current = (-self.reading_scroll.offset().y / px(1.0)).max(0.0);
        let max = self.reading_scroll.max_offset().y / px(1.0);
        let target = (current + viewport * direction).clamp(0.0, max.max(0.0));
        self.reading_scroll.set_offset(point(px(0.0), px(-target)));
        cx.notify();
    }

    fn select_page_up(&mut self, _: &SelectPageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.move_page(-1, true, window, cx);
    }

    fn select_page_down(
        &mut self,
        _: &SelectPageDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_page(1, true, window, cx);
    }

    fn delete_line_start(
        &mut self,
        _: &DeleteToLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_to_line(false, window, cx);
    }

    fn delete_line_end(
        &mut self,
        _: &DeleteToLineEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_to_line(true, window, cx);
    }

    /// Toggle `[ ]` / `[x]` for one task item without moving the caret.
    fn toggle_task(&mut self, task: TaskBox, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_task_range(task.start, task.end, window, cx);
    }

    fn toggle_task_range(
        &mut self,
        start: usize,
        end: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let block_index = block_index_for_offset(&self.blocks, start);
        let Some(block) = self.blocks.get(block_index).copied() else {
            return;
        };
        let full = &self.buffer.text()[block.start..block.end];
        let Some(visual) = task_visual(block, full) else {
            return;
        };
        if block.start + visual.box_start != start || block.start + visual.box_end != end {
            return;
        }
        let caret = self.caret;
        let anchor = self.anchor;
        let at = start + 1;
        let replacement = if visual.checked { " " } else { "x" };
        self.replace_range(at..at + 1, replacement, EditGroup::Other, window, cx);
        self.caret = boundary_at_or_before(self.buffer.text(), caret);
        self.anchor = anchor.map(|anchor| boundary_at_or_before(self.buffer.text(), anchor));
        self.history
            .set_last_selection_after(Some((self.anchor.unwrap_or(self.caret), self.caret)));
        self.desired_x = None;
        self.reveal_caret();
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    /// Keyboard equivalent of clicking the task box on the caret's line.
    fn toggle_task_action(&mut self, _: &ToggleTask, window: &mut Window, cx: &mut Context<Self>) {
        if self.reading {
            return;
        }
        let index = block_index_for_offset(&self.blocks, self.caret);
        let Some(block) = self.blocks.get(index).copied() else {
            return;
        };
        let full = &self.buffer.text()[block.start..block.end];
        let Some(visual) = task_visual(block, full) else {
            return;
        };
        self.commit_composition(window);
        self.toggle_task_range(
            block.start + visual.box_start,
            block.start + visual.box_end,
            window,
            cx,
        );
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_page = self.last_bounds.is_some_and(|bounds| {
            event.position.y >= bounds.top() + HEAD_PX && event.position.y <= bounds.bottom()
        });
        if on_page && let Some(find) = self.find.take() {
            self.last_find = Some((find.query, find.replacement, find.case_sensitive));
            cx.notify();
        }
        if self.layout_is_current()
            && on_page
            && let Some(task) = self
                .last_task_boxes
                .iter()
                .find(|task| task.bounds.contains(&event.position))
                .copied()
        {
            // A click is an explicit edit boundary. Commit the preedit first
            // so the task toggle cannot replace bytes under a live composition.
            self.commit_composition(window);
            self.focus_handle.focus(window, cx);
            self.toggle_task(task, window, cx);
            return;
        }
        let Some(offset) = self.hit_test_offset(event.position) else {
            self.mouse_selecting = false;
            self.mouse_down_offset = None;
            return;
        };

        self.commit_composition(window);
        self.history.break_group();
        if event.click_count >= 3 {
            let Some(block) = self
                .blocks
                .get(block_index_for_offset(&self.blocks, offset))
                .copied()
            else {
                return;
            };
            self.anchor = Some(block.start);
            self.caret = block.end;
        } else if event.click_count == 2 {
            let range = word_range(self.buffer.text(), offset);
            self.anchor = Some(range.start);
            self.caret = range.end;
        } else if event.modifiers.shift {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
            self.caret = offset;
        } else {
            self.anchor = None;
            self.caret = offset;
        }
        self.mouse_selecting = true;
        self.mouse_down_offset = Some(self.anchor.unwrap_or(offset).min(self.buffer.len()));
        if self.blur_subscription.is_none() {
            let handle = self.focus_handle.clone();
            self.blur_subscription = Some(cx.on_blur(&handle, window, |editor, window, _cx| {
                editor.end_drag(window);
            }));
        }
        self.desired_x = None;
        self.reveal_caret();
        self.focus_handle.focus(window, cx);
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    /// Ends a mouse drag, whatever ended it: the button going up, focus
    /// leaving the page, or a move arriving with no button held because
    /// the up happened somewhere this window never heard about. Without
    /// the last two, `mouse_selecting` stays on and the next plain hover
    /// would keep extending the selection.
    fn end_drag(&mut self, window: &mut Window) {
        if !self.mouse_selecting {
            return;
        }
        self.mouse_selecting = false;
        self.mouse_down_offset = None;
        window.notify_text_selection_changed(&self.focus_handle);
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.mouse_selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag(_window);
            return;
        }
        let Some(offset) = self.offset_for_drag(event.position) else {
            return;
        };
        if self.anchor.is_none() {
            self.anchor = self.mouse_down_offset;
        }
        if self.caret == offset {
            return;
        }
        self.caret = offset;
        self.desired_x = None;
        self.reveal_caret();
        _window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, _cx: &mut Context<Self>) {
        if event.button == MouseButton::Left {
            self.end_drag(window);
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(bounds) = self.last_bounds else {
            return;
        };
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let delta = event.delta.pixel_delta(LINE_HEIGHT * self.scale).y / px(1.0);
        let viewport = ((bounds.size.height - HEAD_PX) / px(1.0)).max(0.0);
        let total = self.estimated_total.max(layout.total());
        self.scroll = (self.scroll + delta).clamp(0.0, (total - viewport).max(0.0));
        // The wheel is an explicit request to leave the caret where it is.
        self.reveal_pending = false;
        cx.notify();
    }

    fn reveal_caret(&mut self) {
        self.reveal_pending = true;
        let Some(bounds) = self.last_bounds else {
            return;
        };
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let Some((y, h)) = self.caret_band(layout) else {
            return;
        };
        let viewport = ((bounds.size.height - HEAD_PX) / px(1.0)).max(0.0);
        let total = layout.total();
        let max_scroll = (total - viewport).max(0.0);
        if y < self.scroll {
            self.scroll = y;
        } else if y + h > self.scroll + viewport {
            self.scroll = y + h - viewport;
        }
        self.scroll = self.scroll.clamp(0.0, max_scroll);
    }

    fn layout_is_current(&self) -> bool {
        self.last_bounds.is_some()
            && self.frame_seq > 0
            && self.last_revision == self.buffer.revision()
            && self.last_layout_width == self.layout_width
            && self.layout.as_ref().is_some_and(|layout| {
                layout.matches_scaled(self.buffer.revision(), self.layout_width, self.scale)
            })
            && (self.last_scroll - self.scroll).abs() < 0.01
    }

    fn in_viewport(&self, line: &LineGeometry) -> bool {
        let Some(bounds) = self.last_bounds else {
            return false;
        };
        line.bounds.bottom() > bounds.top() + HEAD_PX && line.bounds.top() < bounds.bottom()
    }

    /// The caret rectangle from a set of shaped lines. Shared by prepaint
    /// (before the lines are committed) and `caret_rect` (after paint).
    fn caret_bounds_in(
        &self,
        lines: &[LineGeometry],
        bounds: Bounds<Pixels>,
        scroll: f32,
        page_left: Pixels,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let block_index = block_index_for_offset(&self.blocks, self.caret);
        let block = self.blocks.get(block_index).copied()?;
        let local = self
            .caret
            .saturating_sub(block.start)
            .min(block.end - block.start);
        let mut last = None;
        for line in lines
            .iter()
            .filter(|line| line.block == block_index && !line.rule)
        {
            last = Some(line);
            let line_start = line.start - block.start;
            let line_end = line.end - block.start;
            if local <= line_end {
                let in_line = local.saturating_sub(line_start).min(line_end - line_start);
                let x = line.bounds.left() + line.shaped.x_for_index(in_line);
                if line.bounds.bottom() <= bounds.top() + HEAD_PX
                    || line.bounds.top() >= bounds.bottom()
                {
                    return None;
                }
                return Some(Bounds::new(
                    point(x, line.bounds.top()),
                    size(CARET_WIDTH, px(line.h)),
                ));
            }
        }
        if let Some(line) = last {
            let in_line = (line.end - block.start).saturating_sub(line.start - block.start);
            let x = line.bounds.left() + line.shaped.x_for_index(in_line);
            if line.bounds.bottom() <= bounds.top() + HEAD_PX
                || line.bounds.top() >= bounds.bottom()
            {
                return None;
            }
            return Some(Bounds::new(
                point(x, line.bounds.top()),
                size(CARET_WIDTH, px(line.h)),
            ));
        }

        // A blank line, a rule, or the empty document: the caret sits at the
        // top of the block with the body's line height.
        let y = layout.block_top(block_index)?;
        let top = bounds.top() + HEAD_PX + px(y) - px(scroll);
        let h = style::line_height(style::look_scaled(block, self.scale));
        if top + px(h) <= bounds.top() + HEAD_PX || top >= bounds.bottom() {
            return None;
        }
        Some(Bounds::new(point(page_left, top), size(CARET_WIDTH, px(h))))
    }

    pub fn caret_rect(&self) -> Option<Bounds<Pixels>> {
        if !self.layout_is_current() {
            return None;
        }
        let bounds = self.last_bounds?;
        let page_left = self.page_left(bounds);
        self.caret_bounds_in(&self.last_lines, bounds, self.scroll, page_left)
    }

    /// The buffer revision the visible layout was built from.
    pub fn revision(&self) -> u64 {
        self.buffer.revision()
    }

    /// The buffer's incremental content hash; see `Buffer::content_hash`.
    pub fn content_hash(&self) -> u64 {
        self.buffer.content_hash()
    }

    /// The selection anchor, if the selection is non-empty.
    pub fn anchor(&self) -> Option<usize> {
        self.anchor.filter(|anchor| *anchor != self.caret)
    }

    /// The vertical scroll offset in pixels.
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// `(caret, anchor, scroll, reading, focus, typewriter)` for session
    /// restore. This is view state; the document remains the file.
    pub fn view_state(&self) -> (usize, Option<usize>, f32, bool, bool, bool) {
        (
            self.caret,
            self.anchor,
            self.scroll,
            self.reading,
            self.focus,
            self.typewriter,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn restore_view_state(
        &mut self,
        caret: usize,
        anchor: Option<usize>,
        scroll: f32,
        reading: bool,
        focus: bool,
        typewriter: bool,
        cx: &mut Context<Self>,
    ) {
        let text = self.buffer.text();
        self.caret = boundary_at_or_before(text, caret.min(text.len()));
        self.anchor = anchor
            .filter(|anchor| *anchor != self.caret)
            .map(|anchor| boundary_at_or_before(text, anchor.min(text.len())));
        self.scroll = scroll.max(0.0);
        let mode_changed =
            self.reading != reading || self.focus != focus || self.typewriter != typewriter;
        self.reading = reading;
        self.focus = focus;
        self.typewriter = typewriter;
        self.desired_x = None;
        self.reveal_pending = true;
        if mode_changed {
            cx.emit(EditorEvent::ModeChanged);
        }
        cx.notify();
    }

    /// The last idle count of words, characters and reading time.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Counts for the current selection, if it is non-empty.
    pub fn selection_stats(&self) -> Option<Stats> {
        self.selection()
            .map(|range| pilcrow_core::stats::compute_range(self.buffer.text(), range))
    }

    pub fn focus_enabled(&self) -> bool {
        self.focus
    }

    pub fn focus_scope(&self) -> FocusScope {
        self.focus_scope
    }

    pub fn typewriter_enabled(&self) -> bool {
        self.typewriter
    }

    /// The block the caret is writing in, skipping blank lines and rules.
    fn focus_block(&self) -> Option<usize> {
        let n = self.blocks.len();
        if n == 0 {
            return None;
        }
        let mut index = block_index_for_offset(&self.blocks, self.caret).min(n - 1);
        if self
            .blocks
            .get(index)
            .is_some_and(|block| is_text_block(*block))
        {
            return Some(index);
        }
        let mut before = index;
        while before > 0 {
            before -= 1;
            if self
                .blocks
                .get(before)
                .is_some_and(|block| is_text_block(*block))
            {
                return Some(before);
            }
        }
        index += 1;
        while index < n {
            if self
                .blocks
                .get(index)
                .is_some_and(|block| is_text_block(*block))
            {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    /// The sentence holding the caret, clipped to its block.
    fn sentence_range(&self, block_index: usize) -> Option<Range<usize>> {
        let key = (self.buffer.revision(), self.caret);
        if let Some((revision, caret, cached)) = self.sentence_cache.borrow().as_ref()
            && (*revision, *caret) == key
        {
            return cached.clone();
        }
        let range = self.compute_sentence_range(block_index);
        *self.sentence_cache.borrow_mut() = Some((key.0, key.1, range.clone()));
        range
    }

    fn compute_sentence_range(&self, block_index: usize) -> Option<Range<usize>> {
        let block = *self.blocks.get(block_index)?;
        let block_text = &self.buffer.text()[block.start..block.end];
        let at = self.caret.clamp(block.start, block.end) - block.start;
        let chars: Vec<(usize, char)> = block_text.char_indices().collect();
        let pos = chars.partition_point(|(index, _)| *index < at);

        // A caret in the trailing air belongs to the sentence that just ended.
        if chars[pos..].iter().all(|(_, ch)| ch.is_whitespace()) {
            let mut end = chars.len();
            while end > 0 && chars[end - 1].1.is_whitespace() {
                end -= 1;
            }
            if end == 0 {
                return None;
            }
            let mut start = end;
            while start > 0 && is_closing(chars[start - 1].1) {
                start -= 1;
            }
            if start > 0 && is_sentence_end(chars[start - 1].1) && chars[start - 1].1 != '\n' {
                start -= 1;
            }
            while start > 0 && !is_sentence_end(chars[start - 1].1) {
                start -= 1;
            }
            if start > 0 {
                while start < end && is_closing(chars[start].1) {
                    start += 1;
                }
            }
            while start < end && chars[start].1.is_whitespace() {
                start += 1;
            }
            return (start < end).then_some(
                block.start + chars[start].0
                    ..block.start + chars[end - 1].0 + chars[end - 1].1.len_utf8(),
            );
        }

        // A caret on a line break, or on the spaces before one, belongs to
        // the sentence the break ends.
        let mut probe = pos;
        while probe < chars.len() && chars[probe].1.is_whitespace() && chars[probe].1 != '\n' {
            probe += 1;
        }
        if probe < chars.len() && chars[probe].1 == '\n' {
            let mut end = probe;
            while end > 0 && chars[end - 1].1.is_whitespace() {
                end -= 1;
            }
            if end == 0 {
                return None;
            }
            let mut start = end;
            while start > 0 && is_closing(chars[start - 1].1) {
                start -= 1;
            }
            if start > 0 && is_sentence_end(chars[start - 1].1) && chars[start - 1].1 != '\n' {
                start -= 1;
            }
            while start > 0 && !is_sentence_end(chars[start - 1].1) {
                start -= 1;
            }
            if start > 0 {
                while start < end && is_closing(chars[start].1) {
                    start += 1;
                }
            }
            while start < end && chars[start].1.is_whitespace() {
                start += 1;
            }
            return (start < end)
                .then_some(block.start + chars[start].0..block.start + chars[probe].0 + 1);
        }

        let mut seed = pos;
        while seed > 0 && seed < chars.len() && is_closing(chars[seed].1) {
            seed -= 1;
        }
        let mut start = seed;
        while start > 0 && !is_sentence_end(chars[start - 1].1) {
            start -= 1;
        }
        let mut end = seed;
        while end < chars.len() {
            let ch = chars[end].1;
            end += 1;
            if is_sentence_end(ch) {
                while end < chars.len() && is_closing(chars[end].1) {
                    end += 1;
                }
                break;
            }
        }
        if start > 0 {
            while start < end && is_closing(chars[start].1) {
                start += 1;
            }
        }
        while start < end && chars[start].1.is_whitespace() {
            start += 1;
        }
        while end > start && chars[end - 1].1.is_whitespace() {
            end -= 1;
        }
        (start < end).then_some(
            block.start + chars[start].0
                ..block.start + chars[end - 1].0 + chars[end - 1].1.len_utf8(),
        )
    }

    fn focus_view(&self) -> Option<FocusView> {
        if !self.focus {
            return None;
        }
        let index = self.focus_block()?;
        let block = self.blocks.get(index).copied()?;
        if self.focus_scope == FocusScope::Paragraph {
            return Some(FocusView {
                start: block.start,
                end: block.end,
            });
        }
        self.sentence_range(index)
            .map(|range| FocusView {
                start: range.start,
                end: range.end,
            })
            .or(Some(FocusView {
                start: block.start,
                end: block.end,
            }))
    }

    pub fn set_focus_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.focus != on {
            self.focus = on;
            cx.emit(EditorEvent::ModeChanged);
            cx.notify();
        }
    }

    pub fn set_focus_scope(&mut self, scope: FocusScope, cx: &mut Context<Self>) {
        if self.focus_scope != scope || !self.focus {
            self.focus_scope = scope;
            self.focus = true;
            cx.emit(EditorEvent::ModeChanged);
            cx.notify();
        }
    }

    pub fn set_typewriter(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.typewriter != on {
            self.typewriter = on;
            self.reveal_caret();
            cx.emit(EditorEvent::ModeChanged);
            cx.notify();
        }
    }

    /// The editor's viewport size in pixels, once it has been painted.
    pub fn viewport(&self) -> Option<(f32, f32)> {
        self.last_bounds
            .map(|bounds| (bounds.size.width / px(1.0), bounds.size.height / px(1.0)))
    }

    /// Whether the visible layout matches the current buffer and scroll.
    pub fn layout_done(&self) -> bool {
        self.layout_is_current()
    }

    /// A monotonic counter of completed paints.
    pub fn frame_seq(&self) -> u64 {
        self.frame_seq
    }

    /// Starts a frame trace: clears the ring and records a baseline frame.
    /// Painted frames and driver inputs append records until
    /// [`EditorView::trace_stop`]. Used by the test driver only.
    pub fn trace_start(&mut self) {
        self.trace_records.clear();
        self.trace_input_seq = 0;
        self.trace_frame_started = None;
        self.trace_active = true;
        self.push_trace_record(0.0);
    }

    /// Stops the trace and returns its records, oldest first.
    pub fn trace_stop(&mut self) -> Vec<FrameRecord> {
        self.trace_active = false;
        self.trace_frame_started = None;
        std::mem::take(&mut self.trace_records)
            .into_iter()
            .collect()
    }

    /// Whether a frame trace is currently recording.
    pub fn trace_active(&self) -> bool {
        self.trace_active
    }

    /// Records one driver input (type/key/chord/scroll) after it applied.
    /// The record carries the new input sequence number, so a later painted
    /// frame with the same sequence proves the input settled into paint.
    pub fn note_driver_input(&mut self) {
        if !self.trace_active {
            return;
        }
        self.trace_input_seq = self.trace_input_seq.wrapping_add(1);
        self.push_trace_record(0.0);
    }

    /// Records the frame that just painted. Called from the paint path after
    /// `frame_seq` bumps; a no-op while no trace is active.
    fn record_painted_frame(&mut self) {
        if self.trace_active {
            let frame_ms = self
                .trace_frame_started
                .take()
                .map_or(0.0, |started| started.elapsed().as_secs_f64() * 1_000.0);
            self.push_trace_record(frame_ms);
        }
    }

    fn push_trace_record(&mut self, frame_ms: f64) {
        let caret_rect = self.caret_rect().map(|rect| {
            (
                rect.origin.x / px(1.0),
                rect.origin.y / px(1.0),
                rect.size.width / px(1.0),
                rect.size.height / px(1.0),
            )
        });
        let record = FrameRecord {
            seq: self.frame_seq,
            input_seq: self.trace_input_seq,
            revision: self.buffer.revision(),
            caret: self.caret,
            anchor: self.anchor,
            scroll: self.scroll,
            layout_done: self.layout_is_current(),
            caret_rect,
            frame_ms,
            paint_hash: self.paint_hash(),
        };
        self.trace_records.push_back(record);
        while self.trace_records.len() > TRACE_CAPACITY {
            self.trace_records.pop_front();
        }
    }

    fn paint_hash(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |value: u64| {
            hash ^= value;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        mix(self.buffer.content_hash());
        mix(self.buffer.revision());
        mix(self.caret as u64);
        mix(u64::from(self.scroll.to_bits()));
        for line in self.visible_lines() {
            mix(line.start as u64);
            mix(line.end as u64);
            mix(u64::from(line.x.to_bits()));
            mix(u64::from(line.y.to_bits()));
            mix(u64::from(line.h.to_bits()));
        }
        hash
    }

    /// The placed lines that are currently inside the viewport.
    pub fn visible_lines(&self) -> Vec<VisibleLine> {
        if !self.layout_is_current() {
            return Vec::new();
        }
        self.last_lines
            .iter()
            .filter(|line| self.in_viewport(line))
            .map(|line| VisibleLine {
                line: line.index,
                block: line.block,
                start: line.start,
                end: line.end,
                x: line.bounds.left() / px(1.0),
                y: line.bounds.top() / px(1.0),
                h: line.h,
                rule: line.rule,
            })
            .collect()
    }

    /// Apply a vertical scroll delta through the same clamp the wheel uses.
    /// Returns false before the first paint, when there is no viewport yet.
    pub fn scroll_by(&mut self, delta_y: f32) -> bool {
        let Some(bounds) = self.last_bounds else {
            return false;
        };
        let Some(layout) = self.layout.as_ref() else {
            return false;
        };
        let viewport = ((bounds.size.height - HEAD_PX) / px(1.0)).max(0.0);
        let total = self.estimated_total.max(layout.total());
        self.scroll = (self.scroll + delta_y).clamp(0.0, (total - viewport).max(0.0));
        self.reveal_pending = false;
        true
    }

    fn offset_from_utf16(text: &str, offset: usize) -> usize {
        let mut utf16 = 0;
        for (byte, ch) in text.char_indices() {
            if utf16 >= offset {
                return byte;
            }
            utf16 += ch.len_utf16();
        }
        text.len()
    }

    fn offset_to_utf16(text: &str, offset: usize) -> usize {
        let offset = boundary_at_or_before(text, offset);
        text[..offset].encode_utf16().count()
    }

    fn byte_range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        let text = self.buffer.text();
        let len = text.encode_utf16().count();
        let start_byte = Self::offset_from_utf16(text, range.start.min(len));
        let end_byte = Self::offset_from_utf16(text, range.end.min(len));
        let mut start = grapheme_at_or_before(text, start_byte);
        let mut end = if range.start == range.end {
            start
        } else {
            grapheme_at_or_after(text, end_byte)
        };
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        start..end
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests;
