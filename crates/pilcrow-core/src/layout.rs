//! Where every line goes, computed once per (revision, width) instead of once
//! per frame.
//!
//! The editor paints from this: a scroll offset indexes into `lines`, and only
//! the lines inside the viewport are emitted. Without it, a long note would
//! cost the whole document in every frame, and typing would slow down as the
//! note grew — the classic failure of an editor that feels fine until it
//! doesn't.

use crate::blocks::{self, Block, Kind as BlockKind};
use crate::inline::{self, Kind as InlineKind};
use crate::style;
use crate::wrap::{self, Line, MeasureStyle, Measurer};

/// One visual line on the page.
///
/// Coordinates are `u32`: a single block or line range over 4 GiB is clamped
/// and debug builds assert. Real notes are far below that.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// Index into the block table this layout was built from.
    pub block: u32,
    /// Byte range within the block. `end` excludes break whitespace.
    pub start: u32,
    pub end: u32,
    /// Offset from the column's left edge: a hanging heading marker, a list
    /// continuation, or a quote's indent.
    pub x: f32,
    /// Document y of the line's top.
    pub y: f32,
    /// Line height.
    pub h: f32,
    /// A horizontal rule: no text, a hairline and its own space.
    pub rule: bool,
}

/// A block or line range wider than `u32::MAX` bytes — 4 GiB — is outside the
/// layout contract and is clamped; debug builds assert. No real note comes
/// close, and the compact coordinates keep `Placed` small enough for the paint
/// path.
fn to_u32(value: usize) -> u32 {
    debug_assert!(
        value <= u32::MAX as usize,
        "layout coordinates exceed the 4 GiB u32 limit"
    );
    value.min(u32::MAX as usize) as u32
}

/// Marker hangs wider than this are a broken measurer, not a heading. It is
/// far more than the widest `###### ` at 28pt.
const MAX_HANG: f32 = 256.0;

/// Estimates recomputed per budgeted refill. Big enough that a long note
/// converges in a few frames, small enough that no frame pays for the
/// whole suffix after an edit.
const ESTIMATE_REFILL_BUDGET: usize = 8192;

/// A measurer is allowed to be approximate, not nonsensical: a non-finite or
/// negative width is zero, and a marker hang is capped so a bad face cannot
/// push the page off-screen.
fn sanitize_width(width: f32) -> f32 {
    if width.is_finite() && width > 0.0 {
        width.min(MAX_HANG)
    } else {
        0.0
    }
}

/// The laid-out page, built forward in budgeted steps.
pub struct Layout {
    /// Every placed line, sorted by y.
    lines: Vec<Placed>,
    /// Top of each block *before* its leading gap, so truncation can resume at
    /// the point the block began rather than after its space.
    block_y: Vec<f32>,
    total: f32,
    width: f32,
    /// Reader-selected text scale. Geometry is derived from it, never stored
    /// in the document.
    scale: f32,
    /// The document revision this layout was built from. The layout does not
    /// read the document; the caller keeps this in step and uses
    /// [`Layout::matches`] to decide whether a cached layout is still valid.
    revision: u64,
    /// How far the incremental build has got, and whether it has finished.
    next_block: usize,
    done: bool,
    /// The widest heading marker hang seen so far, in points. The page shifts
    /// its column left by this much so a marker can be drawn left of the text
    /// edge at a non-negative offset. Monotonic within a build: a stale larger
    /// value only moves the invisible column edge.
    max_hang: f32,
    /// Kinds for the block being wrapped: reused every block, so laying out a
    /// thousand paragraphs does not allocate a thousand arrays.
    kinds_buf: Vec<InlineKind>,
    /// Line ranges for the block being wrapped, reused the same way.
    scratch: Vec<Line>,
    /// First block of the placed window. Zero until a jump skips placement
    /// of the prefix; see [`Layout::jump_to_block`].
    window_start: usize,
    /// Per-block heights (`gap_before` + content + `gap`): exact once a
    /// block is placed, an estimate before. The estimate prefix is what
    /// lets a jump seed placement deep in a long note without wrapping
    /// everything above it.
    heights: Vec<f32>,
    /// Estimates are stale from this block on; a truncate pushes it back to
    /// the resume point. `blocks.len()` once everything is current.
    est_dirty_from: usize,
    /// High-water mark of blocks ever estimated. Past it, prefix tops
    /// extrapolate the filled blocks' average height, so the total is
    /// plausible from the first frame of a huge note instead of zero
    /// until budgeted refills catch up.
    est_filled_to: usize,
    /// Prefix tops over `heights`, rebuilt lazily when `tops_dirty`.
    est_tops: Vec<f32>,
    tops_dirty: bool,
    /// A truncate landed at or above the placed window, discarding it.
    /// `extend` must seed `total` from the estimated prefix before it
    /// places anything, since `block_y` has no honest value there.
    seed_pending: bool,
    /// Width-per-byte probes keyed by (size bits, bold), for estimates.
    /// One probe per face the page actually uses; a handful at most.
    probe_cache: Vec<((u32, bool), f32)>,
}

impl Layout {
    /// Prepares an empty layout with revision zero.
    pub fn begin(blocks: &[Block], width: f32) -> Self {
        Self::begin_at(blocks, width, 0)
    }

    /// Prepares an empty layout for a document revision. Cache key: this
    /// revision plus [`Layout::width`].
    pub fn begin_at(blocks: &[Block], width: f32, revision: u64) -> Self {
        Self::begin_at_scaled(blocks, width, revision, 1.0)
    }

    /// Prepares an empty layout at a reader-selected text scale.
    pub fn begin_at_scaled(blocks: &[Block], width: f32, revision: u64, scale: f32) -> Self {
        let scale = if scale.is_finite() {
            scale.clamp(0.75, 1.75)
        } else {
            1.0
        };
        Self {
            lines: Vec::with_capacity(blocks.len().saturating_mul(4)),
            block_y: vec![0.0; blocks.len()],
            total: 0.0,
            width,
            scale,
            revision,
            next_block: 0,
            done: blocks.is_empty(),
            max_hang: 0.0,
            kinds_buf: Vec::new(),
            scratch: Vec::new(),
            window_start: 0,
            heights: vec![0.0; blocks.len()],
            est_dirty_from: 0,
            est_filled_to: 0,
            est_tops: Vec::new(),
            tops_dirty: true,
            seed_pending: false,
            probe_cache: Vec::new(),
        }
    }

    /// Lays out up to `budget` more blocks. Returns true when everything is
    /// placed. A budget of 0 means "all of it", which is what tests want.
    pub fn extend(
        &mut self,
        text: &str,
        blocks: &[Block],
        m: &dyn Measurer,
        budget: usize,
    ) -> bool {
        if self.done {
            if self.next_block == blocks.len() {
                return true;
            }
            // The document changed since the build finished: a new block was
            // appended, or blocks were removed without an explicit truncate.
            self.done = false;
        }
        self.grow_to(blocks.len());
        // A caller that shrank the document without truncating first would
        // otherwise leave lines for blocks that no longer exist.
        if self.next_block > blocks.len() {
            self.truncate_from(blocks.len());
        }
        if self.seed_pending {
            // The placed window was discarded above this point; the honest
            // resume height is the estimated prefix, corrected block by
            // block as placement lands exact heights.
            self.seed_pending = false;
            self.total = self.estimated_block_top(blocks, m, self.next_block);
        }
        let stop = if budget == 0 {
            blocks.len()
        } else {
            self.next_block.saturating_add(budget).min(blocks.len())
        };

        let placed_from = self.next_block;
        let mut y = self.total;
        for (bi, block) in blocks.iter().enumerate().take(stop).skip(self.next_block) {
            let b = *block;
            let look = style::look_scaled(b, self.scale);
            let Some(top) = self.block_y.get_mut(bi) else {
                break;
            };
            *top = y;
            y += look.gap_before;

            if look.is_rule {
                // Rules occupy space like a line so the paint loop, which
                // walks lines, cannot skip them.
                self.lines.push(Placed {
                    block: to_u32(bi),
                    start: 0,
                    end: 0,
                    x: 0.0,
                    y,
                    h: 1.0,
                    rule: true,
                });
                y += 1.0 + look.gap;
                continue;
            }
            if b.kind == BlockKind::Blank {
                y += look.gap;
                continue;
            }

            let segment = &text[b.start..b.end];
            let face = MeasureStyle {
                size: look.size,
                bold: look.bold,
                italic: false,
                mono: false,
            };

            // A heading's marker hangs into the left margin, so its words
            // start at the same edge as body text — the reason a page of
            // headings reads as one column instead of a staircase. The hang is
            // the marker measured in the heading's own face, and only the
            // first line moves: continuation lines have no marker to hang.
            let mut hang = 0.0;
            if b.kind == BlockKind::Heading && b.marker > 0 && b.marker <= segment.len() {
                hang = sanitize_width(m.width(&segment[..b.marker], face));
                self.max_hang = self.max_hang.max(hang).min(MAX_HANG);
            }

            // A list item's wrapped lines hang: continuation text starts under
            // the item's words, past its marker, the way every printed list
            // reads. A task box is content to the scanner but marker to a
            // reader, so the hang covers it too.
            let mut list_hang = 0.0;
            if b.kind == BlockKind::ListItem && b.marker > 0 && b.marker <= segment.len() {
                list_hang = sanitize_width(m.width(&segment[..b.marker], face));
                if let Some(box_len) = blocks::task_box_len(&segment[b.marker..]) {
                    let end = b.marker + box_len;
                    list_hang = (list_hang
                        + sanitize_width(m.width(&segment[b.marker..end], face)))
                    .min(MAX_HANG);
                }
            }

            // Inside a code block an asterisk is an asterisk, and one source
            // line is already one visual line: a long code line is not
            // reflowed into prose.
            if b.kind == BlockKind::Code {
                let h = style::line_height(look);
                self.lines.push(Placed {
                    block: to_u32(bi),
                    start: 0,
                    end: to_u32(segment.len()),
                    x: look.indent,
                    y,
                    h,
                    rule: false,
                });
                y += h + look.gap;
                continue;
            }

            self.kinds_buf.resize(segment.len(), InlineKind::Plain);
            inline::kinds(segment, &mut self.kinds_buf);
            wrap::wrap_into(
                m,
                segment,
                Some(&self.kinds_buf),
                face,
                self.width - look.indent,
                list_hang,
                &mut self.scratch,
            );

            let h = style::line_height(look);
            for (nth, line) in self.scratch.iter().enumerate() {
                let x = if nth == 0 {
                    look.indent - hang
                } else if b.kind == BlockKind::ListItem {
                    look.indent + list_hang
                } else {
                    look.indent
                };
                self.lines.push(Placed {
                    block: to_u32(bi),
                    start: to_u32(line.start),
                    end: to_u32(line.end),
                    x,
                    y,
                    h,
                    rule: false,
                });
                y += h;
            }
            y += look.gap;
        }

        self.total = y;
        self.next_block = stop;
        self.done = stop >= blocks.len();
        // Exact heights replace estimates for everything placed here, so
        // the estimated prefix converges to the true page as the frontier
        // advances. Derived from block_y: no second measuring pass.
        if stop > placed_from {
            for bi in placed_from..stop {
                let bottom = if bi + 1 < stop {
                    self.block_y.get(bi + 1).copied().unwrap_or(self.total)
                } else {
                    self.total
                };
                if let (Some(height), Some(top)) = (self.heights.get_mut(bi), self.block_y.get(bi))
                {
                    *height = bottom - top;
                }
            }
            self.tops_dirty = true;
        }
        self.done
    }

    /// Lays out until the page reaches `y`, spending at most `max_blocks`
    /// blocks this call. This is the per-frame budget: enough to cover the
    /// viewport and a page of run-up, bounded so a violent scroll cannot stall
    /// a frame. A budget of 0 makes no progress. Stop early when the document
    /// is done or the requested height is reached.
    pub fn cover_to(
        &mut self,
        text: &str,
        blocks: &[Block],
        m: &dyn Measurer,
        y: f32,
        max_blocks: usize,
    ) {
        let mut left = max_blocks;
        while !self.done && self.total < y && left > 0 {
            let before = self.next_block;
            self.extend(text, blocks, m, left.min(128));
            let advanced = self.next_block.saturating_sub(before);
            if advanced == 0 {
                break;
            }
            left -= advanced;
        }
    }

    /// Drops everything laid out from block `block` onwards, so an edit can be
    /// re-flowed without rebuilding the paragraphs above it. Typing at the end
    /// of a long note then costs one paragraph, not the document.
    ///
    /// After an edit, pass
    /// `min(edit_block, first_changed(old_blocks, new_blocks)).saturating_sub(1)`
    /// so a paragraph merge or split starts one block before the change, then
    /// call [`Layout::extend`]. The caller also owns the revision and width
    /// cache key; see [`Layout::set_revision`] and [`Layout::matches`].
    pub fn truncate_from(&mut self, block: usize) {
        // `block` can name a block past everything laid out (an edit made
        // while scrolled away from an unfinished build). `min` lands the
        // resume point on the first block that actually needs work; blocks
        // beyond `next_block` were never placed, so dropping "from them"
        // drops nothing and the build simply continues.
        let from = block.min(self.next_block);
        let mut keep = self.lines.len();
        while keep > 0 && self.lines[keep - 1].block as usize >= from {
            keep -= 1;
        }
        self.lines.truncate(keep);
        // The resume height is the bottom of what is still laid out. A block
        // at or past `next_block` has a placeholder in `block_y` (zero, or the
        // pre-grow total), not a laid-out position: resuming from it overlaid
        // the rest of the document on the first screen and collapsed the
        // page's height to nothing.
        self.est_dirty_from = self.est_dirty_from.min(from);
        if from <= self.window_start {
            // The whole placed window is at or past the resume point (the
            // line drop above emptied it). There is no honest block_y to
            // resume from: `extend` seeds from the estimated prefix.
            self.window_start = from;
            self.seed_pending = true;
            self.total = 0.0;
        } else if from < self.next_block {
            self.total = self.block_y.get(from).copied().unwrap_or(self.total);
        }
        self.next_block = from;
        self.done = false;
    }

    /// Drop the placed lines from the visual line containing `offset` onward,
    /// keeping earlier lines of the same block. Returns the block-local byte
    /// where re-wrapping should resume. This is the incremental path for a
    /// long paragraph: the prefix before the edited line is unchanged.
    pub fn truncate_block_from_offset(&mut self, block: usize, offset: usize) -> Option<usize> {
        let first = self
            .lines
            .partition_point(|line| (line.block as usize) < block);
        if first >= self.lines.len() || self.lines[first].block as usize != block {
            return None;
        }
        let mut chosen = None;
        for (index, line) in self.lines[first..].iter().enumerate() {
            if line.block as usize != block || line.rule {
                break;
            }
            if offset <= line.end as usize {
                chosen = Some((first + index, line.start as usize));
                break;
            }
        }
        let (keep, from) = chosen.or_else(|| {
            self.lines[first..]
                .iter()
                .rposition(|line| line.block as usize == block)
                .map(|index| (first + index, self.lines[first + index].start as usize))
        })?;
        self.lines.truncate(keep);
        self.total = if keep > 0 {
            let last = self.lines[keep - 1];
            last.y + last.h
        } else {
            self.block_y.get(block).copied().unwrap_or(self.total)
        };
        self.est_dirty_from = self.est_dirty_from.min(block);
        self.next_block = block;
        self.done = false;
        Some(from)
    }

    /// Re-wrap one block from a visual-line boundary, then leave the following
    /// blocks to [`Layout::extend`]. Returns false when the block's shape is
    /// not one the line-level path handles; the caller should truncate the
    /// whole block instead.
    pub fn rewrap_block_from(
        &mut self,
        text: &str,
        blocks: &[Block],
        m: &dyn Measurer,
        block: usize,
        from: usize,
    ) -> bool {
        let Some(b) = blocks.get(block).copied() else {
            return false;
        };
        if matches!(b.kind, BlockKind::Blank | BlockKind::Rule | BlockKind::Code) {
            return false;
        }
        let first = self
            .lines
            .partition_point(|line| (line.block as usize) < block);
        let kept = self.lines[first..]
            .iter()
            .take_while(|line| line.block as usize == block)
            .count();
        let look = style::look_scaled(b, self.scale);
        let segment = &text[b.start..b.end];
        let from = from.min(segment.len());
        let face = MeasureStyle {
            size: look.size,
            bold: look.bold,
            italic: false,
            mono: false,
        };

        let mut hang = 0.0;
        if b.kind == BlockKind::Heading && b.marker > 0 && b.marker <= segment.len() {
            hang = sanitize_width(m.width(&segment[..b.marker], face));
            self.max_hang = self.max_hang.max(hang).min(MAX_HANG);
        }
        let mut list_hang = 0.0;
        if b.kind == BlockKind::ListItem && b.marker > 0 && b.marker <= segment.len() {
            list_hang = sanitize_width(m.width(&segment[..b.marker], face));
            if let Some(box_len) = blocks::task_box_len(&segment[b.marker..]) {
                let end = b.marker + box_len;
                list_hang = (list_hang + sanitize_width(m.width(&segment[b.marker..end], face)))
                    .min(MAX_HANG);
            }
        }

        self.kinds_buf.resize(segment.len(), InlineKind::Plain);
        inline::kinds(segment, &mut self.kinds_buf);
        wrap::wrap_into_from(
            m,
            &segment[from..],
            Some(&self.kinds_buf[from..]),
            face,
            self.width - look.indent,
            list_hang,
            kept == 0,
            &mut self.scratch,
        );

        let mut y = if kept > 0 {
            let last = self.lines[first + kept - 1];
            last.y + last.h
        } else {
            self.block_y.get(block).copied().unwrap_or(self.total) + look.gap_before
        };
        let h = style::line_height(look);
        for (nth, line) in self.scratch.iter().enumerate() {
            let first_visual = kept == 0 && nth == 0;
            let x = if first_visual {
                look.indent - hang
            } else if b.kind == BlockKind::ListItem {
                look.indent + list_hang
            } else {
                look.indent
            };
            self.lines.push(Placed {
                block: to_u32(block),
                start: to_u32(from + line.start),
                end: to_u32(from + line.end),
                x,
                y,
                h,
                rule: false,
            });
            y += h;
        }
        y += look.gap;
        self.total = y;
        self.next_block = block + 1;
        self.done = false;
        if let Some(top) = self.block_y.get(block).copied()
            && let Some(height) = self.heights.get_mut(block)
        {
            *height = y - top;
            self.tops_dirty = true;
        }
        true
    }

    /// The placed lines, sorted by y.
    pub fn lines(&self) -> &[Placed] {
        &self.lines
    }

    /// The laid-out height in points.
    pub fn total(&self) -> f32 {
        self.total
    }

    /// The first block that has not been placed yet.
    pub fn next_block(&self) -> usize {
        self.next_block
    }

    /// Whether every block has been placed.
    pub fn done(&self) -> bool {
        self.done
    }

    /// The widest heading marker hang seen so far, in points. The page shifts
    /// its column left by this much.
    pub fn max_hang(&self) -> f32 {
        self.max_hang
    }

    /// The document revision this layout was built from.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Records the revision after an edit. The caller still decides where to
    /// truncate; this only keeps the cache key honest.
    pub fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
    }

    /// The column width this layout was built for.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// Whether a cached layout is still valid for this revision and width at
    /// the default scale.
    pub fn matches(&self, revision: u64, width: f32) -> bool {
        self.matches_scaled(revision, width, 1.0)
    }

    /// Whether a cached layout is still valid for this revision, width and
    /// reader scale.
    pub fn matches_scaled(&self, revision: u64, width: f32, scale: f32) -> bool {
        self.revision == revision && self.width == width && self.scale == scale
    }

    /// The top of a laid-out block, before its leading gap, or `None` if the
    /// block is outside the placed window (not placed yet, or skipped by a
    /// jump). Rules and blank blocks have a top too; `block_y` is what
    /// makes their space addressable.
    pub fn block_top(&self, block: usize) -> Option<f32> {
        (self.window_start <= block && block < self.next_block)
            .then(|| self.block_y.get(block).copied())
            .flatten()
    }

    /// The first block of the placed window. Zero unless a jump skipped
    /// placement of the prefix.
    pub fn window_start(&self) -> usize {
        self.window_start
    }

    /// The first line that extends past `y`: the first index whose
    /// `line.y + line.h > y`. A line whose bottom edge is exactly `y` lies
    /// above the point and is skipped. Returns `lines().len()` at or past the
    /// page bottom. Binary search: the lines are sorted by construction, and a
    /// linear scan per frame would put the document's length back into the
    /// paint path.
    pub fn first_at(&self, y: f32) -> usize {
        let mut lo = 0;
        let mut hi = self.lines.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.lines[mid].y + self.lines[mid].h <= y {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The placed block whose vertical band contains `y`. Blank blocks have no
    /// line of their own, so the line-only hit test used to skip their space
    /// and snap to the next text line.
    pub fn block_at(&self, y: f32) -> Option<usize> {
        if self.block_y.is_empty() || self.next_block <= self.window_start {
            return None;
        }
        let mut lo = self.window_start;
        let mut hi = self.next_block;
        if y < *self.block_y.get(lo)? {
            return Some(lo);
        }
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.block_y.get(mid).is_some_and(|top| *top <= y) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Some(lo)
    }

    /// Makes room for `n` blocks. The document can gain blocks between frames
    /// — Enter at the end adds one — while `begin` sized the table for the
    /// count it saw.
    fn grow_to(&mut self, n: usize) {
        if self.heights.len() < n {
            self.est_dirty_from = self.est_dirty_from.min(self.heights.len());
            self.heights.resize(n, 0.0);
        }
        if self.block_y.len() >= n {
            return;
        }
        // A block past the laid-out end starts where the laid-out document
        // ends, including the last block's trailing gap.
        self.block_y.resize(n, self.total);
    }

    /// Width of one byte in `look`'s face, from a single probe measurement
    /// cached per face. Estimates count bytes, not scalars: exact for
    /// ASCII, an overestimate for multibyte text (a 3-byte scalar is rarely
    /// 3 cells wide). That bias only moves a jump's seed; placement then
    /// corrects every block it actually lays out.
    fn width_per_byte(&mut self, m: &dyn Measurer, look: style::Look) -> f32 {
        let key = (look.size.to_bits(), look.bold);
        if let Some((_, width)) = self.probe_cache.iter().find(|(k, _)| *k == key) {
            return *width;
        }
        const PROBE: &str = "The quick brown fox jumps over the lazy dog. ";
        let face = MeasureStyle {
            size: look.size,
            bold: look.bold,
            italic: false,
            mono: false,
        };
        let width = m.width(PROBE, face) / PROBE.len() as f32;
        // A measurer reporting no width gets half-em cells rather than
        // zero-height estimates for the whole note.
        let width = if width.is_finite() && width > 0.0 {
            width
        } else {
            look.size.max(1.0) * 0.5
        };
        self.probe_cache.push((key, width));
        width
    }

    /// One block's height without wrapping it. Blank, rule and code blocks
    /// are exact (their placement never wraps); wrapped blocks estimate a
    /// line count from byte length at the probed width per byte. O(1) in
    /// the block's metadata: no text is scanned.
    fn estimate_height(&mut self, block: Block, m: &dyn Measurer) -> f32 {
        let look = style::look_scaled(block, self.scale);
        if look.is_rule {
            return look.gap_before + 1.0 + look.gap;
        }
        if block.kind == BlockKind::Blank {
            return look.gap_before + look.gap;
        }
        let h = style::line_height(look);
        if block.kind == BlockKind::Code {
            return look.gap_before + h + look.gap;
        }
        let bytes = block.end.saturating_sub(block.start) as f32;
        let per_byte = self.width_per_byte(m, look);
        let avail = (self.width - look.indent).max(1.0);
        let lines = (bytes * per_byte / avail).ceil().max(1.0);
        look.gap_before + lines * h + look.gap
    }

    /// Estimates stale heights from `est_dirty_from` up to `upto` blocks,
    /// computing at most `budget` of them (0 = no limit). Placed blocks
    /// keep their exact heights and are skipped for free. The budget is
    /// what keeps an edit at the top of a huge note from re-estimating
    /// the whole suffix in one frame: the scrollbar total converges over
    /// a few frames instead. Seeds (jump targets) demand the exact prefix
    /// through `estimated_block_top`, which fills without a budget.
    fn fill_estimates(&mut self, blocks: &[Block], m: &dyn Measurer, upto: usize, budget: usize) {
        self.grow_to(blocks.len());
        let upto = upto.min(blocks.len());
        let mut filled = 0;
        while self.est_dirty_from < upto {
            let bi = self.est_dirty_from;
            self.est_dirty_from += 1;
            if (self.window_start..self.next_block).contains(&bi) {
                continue;
            }
            let Some(block) = blocks.get(bi).copied() else {
                break;
            };
            let height = self.estimate_height(block, m);
            let Some(slot) = self.heights.get_mut(bi) else {
                break;
            };
            *slot = height;
            self.est_filled_to = self.est_filled_to.max(bi + 1);
            filled += 1;
            if budget > 0 && filled >= budget {
                break;
            }
        }
        if filled > 0 {
            self.tops_dirty = true;
        }
        if self.tops_dirty {
            self.est_tops.clear();
            self.est_tops.reserve(blocks.len() + 1);
            // Blocks never estimated yet contribute the filled blocks'
            // average height; placed blocks always contribute their exact
            // recorded height. The prefix up to any filled block is built
            // only from real entries, so seeds stay honest.
            let filled = self.est_filled_to.min(blocks.len());
            let filled_sum: f32 = self.heights.iter().take(filled).sum();
            let average = if filled > 0 {
                filled_sum / filled as f32
            } else {
                0.0
            };
            let mut y = 0.0;
            self.est_tops.push(0.0);
            for (bi, height) in self.heights.iter().take(blocks.len()).enumerate() {
                let height = if bi < filled || (self.window_start..self.next_block).contains(&bi) {
                    *height
                } else {
                    average
                };
                y += height;
                self.est_tops.push(y);
            }
            self.tops_dirty = false;
        }
    }

    /// The estimated top of `block` (before its leading gap): exact for
    /// every block up to the placed window, estimated beyond it. This is
    /// what seeds a jump; it never feeds caret or paint geometry for
    /// visible lines. Fills the prefix up to `block` without a budget:
    /// a seed built on stale heights would offset the whole window.
    pub fn estimated_block_top(&mut self, blocks: &[Block], m: &dyn Measurer, block: usize) -> f32 {
        self.fill_estimates(blocks, m, block, 0);
        self.est_tops
            .get(block.min(blocks.len()))
            .copied()
            .unwrap_or(0.0)
    }

    /// The estimated height of the whole document: exact once `done`,
    /// converging as the frontier advances and as budgeted estimate
    /// refills catch up after edits.
    pub fn estimated_total(&mut self, blocks: &[Block], m: &dyn Measurer) -> f32 {
        self.fill_estimates(blocks, m, blocks.len(), ESTIMATE_REFILL_BUDGET);
        self.est_tops.last().copied().unwrap_or(0.0)
    }

    /// The block whose estimated band contains `y` — the jump target for a
    /// scroll far outside the placed window.
    pub fn estimated_block_at(&mut self, blocks: &[Block], m: &dyn Measurer, y: f32) -> usize {
        self.fill_estimates(blocks, m, blocks.len(), ESTIMATE_REFILL_BUDGET);
        if blocks.is_empty() || y <= 0.0 {
            return 0;
        }
        let mut lo = 0;
        let mut hi = blocks.len();
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.est_tops.get(mid).is_some_and(|top| *top <= y) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Discards the placed lines and resumes placement at `block`, seeded
    /// at its estimated top. This is how a jump (Cmd+End, a far find hit,
    /// a session restore near the end) paints in one frame instead of
    /// wrapping every block above the target over many frames: the blocks
    /// around the target are then placed exactly by `extend`, so the
    /// caret and the visible page are as exact as ever — only the skipped
    /// prefix's contribution to absolute y is an estimate, corrected if
    /// placement ever restarts from the top.
    ///
    /// No-op when `block` is already inside the placed window.
    pub fn jump_to_block(&mut self, blocks: &[Block], m: &dyn Measurer, block: usize) {
        if blocks.is_empty() {
            return;
        }
        let block = block.min(blocks.len() - 1);
        if (self.window_start..self.next_block).contains(&block) {
            return;
        }
        let seed = self.estimated_block_top(blocks, m, block);
        self.lines.clear();
        self.total = seed;
        self.window_start = block;
        self.next_block = block;
        self.done = false;
        self.seed_pending = false;
        if let Some(top) = self.block_y.get_mut(block) {
            *top = seed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Layout, MAX_HANG, Placed, to_u32};
    use crate::blocks::{self, Block, Kind, first_changed};
    use crate::wrap::{MeasureStyle, Measurer};

    const BODY: MeasureStyle = MeasureStyle {
        size: 16.0,
        bold: false,
        italic: false,
        mono: false,
    };

    /// Fixed advances per scalar, widened by bold and narrowed by code.
    /// Prefix widths are exact sums, so a test can name the expected page.
    struct FakeMeasurer {
        advance: f32,
        wide: f32,
    }

    impl FakeMeasurer {
        fn new(advance: f32) -> Self {
            Self {
                advance,
                wide: advance * 2.0,
            }
        }
    }

    impl Default for FakeMeasurer {
        fn default() -> Self {
            Self::new(10.0)
        }
    }

    impl Measurer for FakeMeasurer {
        fn width(&self, text: &str, style: MeasureStyle) -> f32 {
            let mut scale = 1.0;
            if style.bold {
                scale *= 1.5;
            }
            if style.italic {
                scale *= 1.2;
            }
            if style.mono {
                scale *= 0.9;
            }
            text.chars()
                .map(|ch| match ch {
                    '\u{4e00}'..='\u{9fff}' | '😀' => self.wide * scale,
                    _ => self.advance * scale,
                })
                .sum()
        }
    }

    fn scan(text: &str) -> Vec<Block> {
        let mut out = Vec::new();
        blocks::scan(text, &mut out);
        out
    }

    fn build(text: &str, blocks: &[Block], width: f32) -> Layout {
        let mut layout = Layout::begin(blocks, width);
        layout.extend(text, blocks, &FakeMeasurer::default(), 0);
        layout
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn line_text<'a>(text: &'a str, blocks: &[Block], line: &Placed) -> &'a str {
        let block = blocks[line.block as usize];
        &text[block.start + line.start as usize..block.start + line.end as usize]
    }

    #[test]
    fn a_paragraph_is_placed_and_its_gap_is_kept() {
        let text = "alpha beta\n\ngamma";
        let blocks = scan(text);
        assert_eq!(
            blocks.iter().map(|b| b.kind).collect::<Vec<_>>(),
            [Kind::Paragraph, Kind::Blank, Kind::Paragraph]
        );

        let layout = build(text, &blocks, 50.0);
        assert!(layout.done());
        let lines = layout.lines();
        assert_eq!(lines.len(), 3);
        assert_eq!((lines[0].start, lines[0].end), (0, 5));
        assert_eq!((lines[1].start, lines[1].end), (6, 10));
        assert_eq!((lines[2].start, lines[2].end), (0, 5));
        assert_eq!((lines[0].x, lines[1].x, lines[2].x), (0.0, 0.0, 0.0));
        assert!(close(lines[0].h, 25.6));

        // block_y is the top *before* a block's leading gap.
        assert!(close(lines[0].y, 0.0));
        assert!(close(lines[1].y, 25.6));
        assert!(close(
            layout.total(),
            25.6 * 2.0 + 10.0 + 16.0 * 1.6 * 0.55 + 25.6 + 10.0
        ));
        assert_eq!(line_text(text, &blocks, &lines[2]), "gamma");
    }

    #[test]
    fn a_heading_marker_hangs_and_only_the_first_line_moves() {
        let text = "# Title";
        let blocks = scan(text);
        let layout = build(text, &blocks, 500.0);
        let lines = layout.lines();
        assert_eq!(lines.len(), 1);
        // "# " in the heading's bold 28pt face: two 10pt cells at 1.5x.
        assert!(close(layout.max_hang(), 30.0));
        assert!(close(lines[0].x, -30.0));
        assert!(close(lines[0].y, 28.0)); // the heading's gap_before
        assert!(close(lines[0].h, 28.0 * 1.6));

        let long = "# alpha beta gamma delta epsilon";
        let long_blocks = scan(long);
        let layout = build(long, &long_blocks, 180.0);
        let lines = layout.lines();
        assert!(lines.len() > 1);
        assert!(close(lines[0].x, -30.0));
        for line in &lines[1..] {
            assert_eq!(line.x, 0.0, "continuation lines have no marker to hang");
        }
    }

    #[test]
    fn a_list_continuation_hangs_under_the_items_words() {
        let text = "- item with many words to wrap around the column";
        let blocks = scan(text);
        let layout = build(text, &blocks, 120.0);
        let lines = layout.lines();
        assert!(lines.len() > 1);
        assert_eq!(lines[0].x, 16.0);
        for line in &lines[1..] {
            assert_eq!(line.x, 36.0, "16pt list indent plus the '- ' marker");
        }
        assert_eq!(line_text(text, &blocks, &lines[0]), "- item");
    }

    #[test]
    fn a_task_box_is_part_of_the_list_hang() {
        let text = "- [ ] do the thing with enough words to wrap the column";
        let blocks = scan(text);
        assert_eq!(blocks[0].kind, Kind::ListItem);
        let layout = build(text, &blocks, 160.0);
        let lines = layout.lines();
        assert!(lines.len() > 1);
        assert_eq!(lines[0].x, 16.0);
        for line in &lines[1..] {
            assert_eq!(line.x, 76.0, "16 indent + 20 marker + 40 task box");
        }
    }

    #[test]
    fn a_quote_is_indented_on_every_line() {
        let text = "> quoted words that wrap around the column nicely";
        let blocks = scan(text);
        let layout = build(text, &blocks, 120.0);
        let lines = layout.lines();
        assert!(lines.len() > 1);
        for line in lines {
            assert_eq!(line.x, 18.0);
        }
    }

    #[test]
    fn code_is_one_placed_line_per_source_line() {
        let text = "```\nlet x = a very long line that does not wrap even here\n```";
        let blocks = scan(text);
        assert_eq!(blocks.iter().filter(|b| b.kind == Kind::Code).count(), 3);
        let layout = build(text, &blocks, 40.0);
        let lines = layout.lines();
        assert_eq!(lines.len(), 3, "a long code line is not reflowed");
        for (index, line) in lines.iter().enumerate() {
            assert_eq!(line.block, index as u32);
            let block = blocks[index];
            assert_eq!(line.start, 0);
            assert_eq!(line.end as usize, block.end - block.start);
            assert_eq!(line.x, 0.0);
            assert!(close(line.h, 16.0 * 0.92 * 1.6));
            assert!(!line.rule);
        }
        assert!(close(layout.total(), 3.0 * 14.72 * 1.6));
    }

    #[test]
    fn a_rule_is_a_one_point_line_with_its_gap() {
        let text = "---";
        let blocks = scan(text);
        assert_eq!(blocks[0].kind, Kind::Rule);
        let layout = build(text, &blocks, 100.0);
        let lines = layout.lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].rule);
        assert_eq!((lines[0].start, lines[0].end), (0, 0));
        assert!(close(lines[0].h, 1.0));
        assert!(close(lines[0].y, 0.0));
        assert!(close(layout.total(), 15.0));
    }

    #[test]
    fn blank_blocks_have_space_but_no_line() {
        let text = "a\n\nb";
        let blocks = scan(text);
        assert_eq!(
            blocks.iter().map(|b| b.kind).collect::<Vec<_>>(),
            [Kind::Paragraph, Kind::Blank, Kind::Paragraph]
        );
        let layout = build(text, &blocks, 100.0);
        assert_eq!(layout.lines().len(), 2);
        assert_eq!(layout.block_at(35.6), Some(1));
        assert_eq!(layout.block_at(40.0), Some(1));
        assert_eq!(layout.block_at(60.0), Some(2));
        assert_eq!(layout.block_at(-1.0), Some(0));
        assert!(close(layout.block_y[1], 35.6));
        assert!(close(layout.block_y[2], 35.6 + 14.08));
        assert!(close(layout.total(), 35.6 + 14.08 + 35.6));
    }

    #[test]
    fn a_long_word_is_cut_into_placeable_lines() {
        let text = "abcdefghij";
        let blocks = scan(text);
        let layout = build(text, &blocks, 25.0);
        let lines = layout.lines();
        assert_eq!(lines.len(), 5);
        for (index, line) in lines.iter().enumerate() {
            assert_eq!(line.start as usize, index * 2);
            assert_eq!(line.end as usize, index * 2 + 2);
            assert_eq!(line.x, 0.0);
            assert!(close(line.y, index as f32 * 25.6));
        }
        assert!(close(layout.total(), 5.0 * 25.6 + 10.0));
    }

    #[test]
    fn cjk_and_emoji_lines_use_char_boundaries() {
        let text = "你好世界😀你好世界😀";
        let blocks = scan(text);
        let layout = build(text, &blocks, 45.0);
        let lines = layout.lines();
        assert!(lines.len() > 1);
        let block = blocks[0];
        let segment = &text[block.start..block.end];
        let mut covered = vec![false; segment.len()];
        for line in lines {
            assert!(segment.is_char_boundary(line.start as usize));
            assert!(segment.is_char_boundary(line.end as usize));
            for slot in &mut covered[line.start as usize..line.end as usize] {
                assert!(!*slot, "overlapping lines");
                *slot = true;
            }
        }
        for (index, byte) in segment.bytes().enumerate() {
            if !matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                assert!(covered[index]);
            }
        }
    }

    #[test]
    fn a_mixed_document_is_covered_without_overlap() {
        let text = "# Title\n\npara one two three\n\n- item one two three\n\n> quote one two\n\n```\ncode line\n```\n\n---\n\nend";
        let blocks = scan(text);
        let layout = build(text, &blocks, 80.0);
        assert!(layout.done());
        assert!(!layout.lines().is_empty());

        let mut covered: Vec<Vec<bool>> = blocks
            .iter()
            .map(|block| vec![false; block.end - block.start])
            .collect();
        let mut previous_bottom = 0.0;
        for line in layout.lines() {
            let block = blocks[line.block as usize];
            assert!(line.start <= line.end);
            assert!((line.end as usize) <= block.end - block.start);
            assert!(text.is_char_boundary(block.start + line.start as usize));
            assert!(text.is_char_boundary(block.start + line.end as usize));
            assert!(line.y + 1e-3 >= previous_bottom, "lines overlap vertically");
            previous_bottom = line.y + line.h;
            if !line.rule {
                for slot in
                    &mut covered[line.block as usize][line.start as usize..line.end as usize]
                {
                    assert!(!*slot, "overlapping line ranges");
                    *slot = true;
                }
            }
        }

        for (index, block) in blocks.iter().enumerate() {
            let segment = &text[block.start..block.end];
            let lines = layout.lines().iter().filter(|l| l.block as usize == index);
            match block.kind {
                Kind::Blank => assert_eq!(lines.count(), 0),
                Kind::Rule => {
                    let rules: Vec<&Placed> = lines.collect();
                    assert_eq!(rules.len(), 1);
                    assert!(rules[0].rule);
                }
                Kind::Code => assert_eq!(lines.count(), 1),
                _ => {
                    assert!(lines.count() >= 1, "block {index} has no placed line");
                    for (at, byte) in segment.bytes().enumerate() {
                        if !matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                            assert!(covered[index][at], "block {index} byte {at} missing");
                        }
                    }
                }
            }
        }
        assert!(layout.total() >= previous_bottom);
    }

    /// Counts measured bytes so perf fixtures assert work, not wall time.
    struct CountingMeasurer {
        inner: FakeMeasurer,
        measured: std::cell::Cell<usize>,
    }

    impl Measurer for CountingMeasurer {
        fn width(&self, text: &str, style: MeasureStyle) -> f32 {
            self.measured.set(self.measured.get() + text.len());
            self.inner.width(text, style)
        }
    }

    #[test]
    fn covering_the_first_viewport_of_a_long_note_measures_little() {
        // Perf fixture: 200k words. Covering two viewports of height must
        // measure a small prefix of the document, never the whole note.
        let mut text = String::new();
        for paragraph in 0..2_000 {
            text.push_str(&format!("para{paragraph}"));
            for _ in 1..100 {
                text.push_str(" word");
            }
            text.push_str("\n\n");
        }
        let blocks = scan(&text);
        let measurer = CountingMeasurer {
            inner: FakeMeasurer::default(),
            measured: std::cell::Cell::new(0),
        };
        let mut layout = Layout::begin(&blocks, 660.0);
        layout.cover_to(&text, &blocks, &measurer, 2_000.0, 256);
        assert!(layout.total() >= 2_000.0, "viewport was not covered");
        assert!(
            layout.next_block() < blocks.len() / 10,
            "covering a viewport laid out {} of {} blocks",
            layout.next_block(),
            blocks.len()
        );
        // Wrap re-measures slices within a paragraph, so compare against a
        // full build's measured bytes instead of the raw text length.
        let full_measurer = CountingMeasurer {
            inner: FakeMeasurer::default(),
            measured: std::cell::Cell::new(0),
        };
        let mut full_layout = Layout::begin(&blocks, 660.0);
        full_layout.extend(&text, &blocks, &full_measurer, 0);
        assert!(
            measurer.measured.get() * 10 < full_measurer.measured.get(),
            "covering a viewport measured {} bytes, a full build {}",
            measurer.measured.get(),
            full_measurer.measured.get()
        );
        // The covered prefix is a prefix of the full build.
        let full = build(&text, &blocks, 660.0);
        assert_eq!(layout.lines(), &full.lines()[..layout.lines().len()]);
    }

    fn long_note(paragraphs: usize) -> String {
        let mut text = String::new();
        for paragraph in 0..paragraphs {
            text.push_str(&format!("para{paragraph}"));
            for _ in 1..100 {
                text.push_str(" word");
            }
            text.push_str("\n\n");
        }
        text
    }

    #[test]
    fn estimates_converge_to_the_exact_page() {
        // estimated tops are exact wherever placement has been, and
        // the estimate for the whole page converges to the true total.
        let text = long_note(200);
        let blocks = scan(&text);
        let full = build(&text, &blocks, 660.0);

        let mut layout = Layout::begin(&blocks, 660.0);
        let measurer = FakeMeasurer::default();
        let fresh = layout.estimated_total(&blocks, &measurer);
        assert!(
            (fresh - full.total()).abs() < full.total() * 0.15,
            "fresh estimate {fresh} vs exact {}",
            full.total()
        );

        layout.extend(&text, &blocks, &measurer, 40);
        for block in 0..layout.next_block() {
            let exact = layout.block_top(block).expect("placed block top");
            let estimated = layout.estimated_block_top(&blocks, &measurer, block);
            assert!(
                close(estimated, exact),
                "placed block {block}: estimated top {estimated} != exact {exact}"
            );
        }

        layout.extend(&text, &blocks, &measurer, 0);
        assert!(layout.done());
        assert!(
            close(layout.estimated_total(&blocks, &measurer), full.total()),
            "a finished layout's estimate is its exact total"
        );
    }

    #[test]
    fn jumping_to_the_end_measures_little_and_places_the_suffix_exactly() {
        // Perf fixture, a jump near the end of a 200k-word note
        // measures only the probe plus the suffix it places, and the
        // suffix's relative geometry is byte-identical to a full build's.
        let text = long_note(2_000);
        let blocks = scan(&text);
        let full = build(&text, &blocks, 660.0);
        let full_measurer = CountingMeasurer {
            inner: FakeMeasurer::default(),
            measured: std::cell::Cell::new(0),
        };
        let mut full_counted = Layout::begin(&blocks, 660.0);
        full_counted.extend(&text, &blocks, &full_measurer, 0);

        let measurer = CountingMeasurer {
            inner: FakeMeasurer::default(),
            measured: std::cell::Cell::new(0),
        };
        let mut layout = Layout::begin(&blocks, 660.0);
        let target = blocks.len() - 40;
        layout.jump_to_block(&blocks, &measurer, target);
        assert_eq!(layout.window_start(), target);
        assert_eq!(layout.next_block(), target);
        assert!(layout.lines().is_empty());
        assert!(
            measurer.measured.get() < 1_000,
            "the jump itself measured {} bytes (probe only, expected)",
            measurer.measured.get()
        );

        layout.extend(&text, &blocks, &measurer, 0);
        assert!(layout.done());
        assert!(
            measurer.measured.get() * 10 < full_measurer.measured.get(),
            "jump + suffix measured {} bytes, full build {}",
            measurer.measured.get(),
            full_measurer.measured.get()
        );

        let suffix: Vec<_> = full
            .lines()
            .iter()
            .filter(|line| line.block as usize >= target)
            .collect();
        assert_eq!(layout.lines().len(), suffix.len());
        let delta = layout.lines()[0].y - suffix[0].y;
        assert!(
            delta.abs() < full.total() * 0.1,
            "seed offset {delta} is not close to the exact prefix"
        );
        for (jumped, exact) in layout.lines().iter().zip(&suffix) {
            assert_eq!(jumped.block, exact.block);
            assert_eq!(jumped.start, exact.start);
            assert_eq!(jumped.end, exact.end);
            assert!(close(jumped.x, exact.x));
            assert!(close(jumped.h, exact.h));
            assert!(
                (jumped.y - exact.y - delta).abs() < 0.05,
                "suffix drifted: only the seed may offset absolute y"
            );
        }
        assert_eq!(layout.block_top(0), None, "skipped prefix has no top");
        assert!(layout.block_top(target).is_some());
        assert!(close(
            layout.estimated_total(&blocks, &measurer),
            layout.total()
        ));
    }

    #[test]
    fn a_truncate_above_the_window_reseeds_and_a_full_rebuild_is_exact() {
        // After a jump, an edit above the window truncates the window away;
        // placement reseeds from estimates instead of a stale block_y, and
        // truncating to 0 rebuilds the exact page.
        let text = long_note(200);
        let blocks = scan(&text);
        let full = build(&text, &blocks, 660.0);
        let measurer = FakeMeasurer::default();

        let mut layout = Layout::begin(&blocks, 660.0);
        layout.jump_to_block(&blocks, &measurer, 300);
        layout.extend(&text, &blocks, &measurer, 64);
        assert!(layout.window_start() == 300 && !layout.lines().is_empty());

        layout.truncate_from(0);
        layout.extend(&text, &blocks, &measurer, 0);
        assert_eq!(layout.lines(), full.lines());
        assert!(close(layout.total(), full.total()));

        // Jump again (fresh layout: in the rebuilt one, block 300 is
        // already placed and the jump is a no-op), then truncate to a block
        // above the window but below the top: the suffix is placed exactly,
        // offset only by the seed.
        let mut layout = Layout::begin(&blocks, 660.0);
        layout.jump_to_block(&blocks, &measurer, 300);
        layout.extend(&text, &blocks, &measurer, 64);
        layout.truncate_from(100);
        layout.extend(&text, &blocks, &measurer, 0);
        assert!(layout.done());
        let suffix: Vec<_> = full
            .lines()
            .iter()
            .filter(|line| line.block as usize >= 100)
            .collect();
        assert_eq!(layout.lines().len(), suffix.len());
        let delta = layout.lines()[0].y - suffix[0].y;
        for (reseeded, exact) in layout.lines().iter().zip(&suffix) {
            assert_eq!(
                (reseeded.block, reseeded.start, reseeded.end),
                (exact.block, exact.start, exact.end)
            );
            // f32 sums from different seeds drift by hundredths of a point
            // over thousands of lines; a structural error would drift by a
            // whole line height.
            assert!((reseeded.y - exact.y - delta).abs() < 0.05);
        }
    }

    #[test]
    fn budgeted_layout_equals_a_full_build() {
        let mut text = String::new();
        for index in 0..24 {
            text.push_str(&format!("paragraph {index} with several words\n\n"));
        }
        let blocks = scan(&text);
        assert!(blocks.len() > 20);
        let full = build(&text, &blocks, 160.0);

        let mut budgeted = Layout::begin(&blocks, 160.0);
        let mut calls = 0;
        while !budgeted.done() {
            budgeted.extend(&text, &blocks, &FakeMeasurer::default(), 1);
            calls += 1;
            assert!(calls <= blocks.len(), "budgeted build did not advance");
            // Each step leaves a prefix of the final page.
            let done_lines = budgeted.lines().len();
            assert!(done_lines <= full.lines().len());
            assert_eq!(budgeted.lines(), &full.lines()[..done_lines]);
            assert!(budgeted.total() <= full.total() + 1e-3);
        }
        assert_eq!(calls, blocks.len());
        assert_eq!(budgeted.lines(), full.lines());
        assert!(close(budgeted.total(), full.total()));
    }

    #[test]
    fn truncation_then_extend_reproduces_the_full_page() {
        let mut text = String::new();
        for index in 0..16 {
            text.push_str(&format!(
                "## section {index}\n\nbody {index} with words\n\n"
            ));
        }
        let blocks = scan(&text);
        let full = build(&text, &blocks, 140.0);

        for cut in [0, 1, 5, blocks.len() / 2, blocks.len() - 1, blocks.len()] {
            let mut layout = build(&text, &blocks, 140.0);
            layout.truncate_from(cut);
            assert!(!layout.done());
            assert!(
                layout
                    .lines()
                    .iter()
                    .all(|line| (line.block as usize) < cut)
            );
            if cut > 0 && cut < blocks.len() {
                assert!(close(layout.total(), layout.block_y[cut]));
            }
            layout.extend(&text, &blocks, &FakeMeasurer::default(), 0);
            assert!(layout.done());
            assert_eq!(layout.lines(), full.lines(), "cut at {cut}");
            assert!(close(layout.total(), full.total()), "cut at {cut}");
        }
    }

    #[test]
    fn max_hang_is_monotonic_across_a_truncation() {
        let text = "## A wide heading\n\nplain\n";
        let blocks = scan(text);
        let mut layout = build(text, &blocks, 200.0);
        let hang = layout.max_hang();
        assert!(hang > 0.0);
        layout.truncate_from(0);
        assert!(close(layout.max_hang(), hang));
        layout.extend(text, &blocks, &FakeMeasurer::default(), 0);
        assert!(close(layout.max_hang(), hang));
    }

    #[test]
    fn first_changed_invalidation_matches_a_fresh_layout() {
        // old, new, block containing the edit. The invalidation point is the
        // earlier of the edit block and the first structurally changed block.
        let cases: &[(&str, &str, usize)] = &[
            ("```\ncode\n```", "```\ncode\n``", 2),
            ("a\n\nb", "a\nb", 1),
            ("# Title", "Title", 0),
            ("alpha\n\nbeta", "alphA\n\nbetA", 0),
            ("> a\n\n> b", "> a\n> b", 1),
            ("- a\n\n  b", "- a\n  b", 1),
            ("---\ntitle: x\n---\nbody", "---\ntitle: x\n--\nbody", 0),
            ("a\n\nb\n\nc", "a\n\nB\n\nc", 2),
            ("a\nb", "a\n\nb", 0),
        ];
        let m = FakeMeasurer::default();
        for &(old_text, new_text, edit_block) in cases {
            let old_blocks = scan(old_text);
            let new_blocks = scan(new_text);
            let changed = first_changed(&old_blocks, &new_blocks);
            for budget in [0, 1, 2] {
                let mut incremental = Layout::begin(&old_blocks, 90.0);
                incremental.extend(old_text, &old_blocks, &m, budget);
                let from = edit_block.min(changed).saturating_sub(1);
                incremental.truncate_from(from);
                incremental.extend(new_text, &new_blocks, &m, 0);

                let fresh = build(new_text, &new_blocks, 90.0);
                assert_eq!(
                    incremental.lines(),
                    fresh.lines(),
                    "{old_text:?} -> {new_text:?}, budget {budget}, from {from}"
                );
                assert!(
                    close(incremental.total(), fresh.total()),
                    "{old_text:?} -> {new_text:?}, budget {budget}"
                );
                for block in 0..=new_blocks.len() {
                    match (incremental.block_top(block), fresh.block_top(block)) {
                        (Some(a), Some(b)) => assert!(
                            close(a, b),
                            "{old_text:?} -> {new_text:?}: block {block} top {a} vs {b}"
                        ),
                        (None, None) => {}
                        (a, b) => {
                            panic!("{old_text:?} -> {new_text:?}: block {block} top {a:?} vs {b:?}")
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_single_character_deletion_rebuilds_correctly() {
        let documents = [
            "# Title\n\nparagraph one\ncontinuation\n\n- item\n  continuation\n\n> quote\n> more\n\n---\n\nend",
            "```\ncode\n```\n\ntext",
            "---\ntitle: x\n---\nbody\n\n- [ ] task\nmore",
            "a\n\n\nb\n\n",
            "- a\n\n  b\n\nc",
        ];
        let m = FakeMeasurer::default();
        for old_text in documents {
            let old_blocks = scan(old_text);
            for at in 0..old_text.len() {
                let mut new_text = old_text.to_owned();
                new_text.remove(at);
                let new_blocks = scan(&new_text);
                let edit_block = old_blocks
                    .iter()
                    .position(|block| block.start <= at && at <= block.end)
                    .unwrap_or(old_blocks.len().saturating_sub(1));
                let changed = first_changed(&old_blocks, &new_blocks);
                for budget in [0, 1, 3] {
                    let mut layout = Layout::begin(&old_blocks, 80.0);
                    layout.extend(old_text, &old_blocks, &m, budget);
                    let from = edit_block.min(changed).saturating_sub(1);
                    layout.truncate_from(from);
                    layout.extend(&new_text, &new_blocks, &m, 0);
                    let fresh = build(&new_text, &new_blocks, 80.0);
                    assert_eq!(
                        layout.lines(),
                        fresh.lines(),
                        "delete {at} from {old_text:?}, budget {budget}, from {from}, changed {changed}"
                    );
                    assert!(
                        close(layout.total(), fresh.total()),
                        "delete {at} from {old_text:?}, budget {budget}"
                    );
                }
            }
        }
    }

    #[test]
    fn inserting_newlines_rebuilds_correctly() {
        let documents = [
            "# Title\n\nparagraph one\ncontinuation\n\n- item\n  continuation\n\n> quote\n> more\n\n---\n\nend",
            "```\ncode\n```\n\ntext",
            "---\ntitle: x\n---\nbody\n\n- [ ] task\nmore",
            "a\n\n\nb\n\n",
            "- a\n\n  b\n\nc",
        ];
        let m = FakeMeasurer::default();
        for old_text in documents {
            let old_blocks = scan(old_text);
            for at in 0..=old_text.len() {
                let mut new_text = old_text.to_owned();
                new_text.insert(at, '\n');
                let new_blocks = scan(&new_text);
                let edit_block = old_blocks
                    .iter()
                    .position(|block| block.start <= at && at <= block.end)
                    .unwrap_or(old_blocks.len().saturating_sub(1));
                let changed = first_changed(&old_blocks, &new_blocks);
                for budget in [0, 1, 3] {
                    let mut layout = Layout::begin(&old_blocks, 80.0);
                    layout.extend(old_text, &old_blocks, &m, budget);
                    let from = edit_block.min(changed).saturating_sub(1);
                    layout.truncate_from(from);
                    layout.extend(&new_text, &new_blocks, &m, 0);
                    let fresh = build(&new_text, &new_blocks, 80.0);
                    assert_eq!(
                        layout.lines(),
                        fresh.lines(),
                        "insert newline at {at} in {old_text:?}, budget {budget}, from {from}"
                    );
                    assert!(
                        close(layout.total(), fresh.total()),
                        "insert newline at {at} in {old_text:?}, budget {budget}"
                    );
                }
            }
        }
    }

    #[test]
    fn block_top_and_cover_to_walk_the_frontier() {
        let text = "\n\n# H\n\na\n\n---\n\nb\n\n";
        let blocks = scan(text);
        let m = FakeMeasurer::default();
        let mut layout = Layout::begin(&blocks, 200.0);
        for block in 0..blocks.len() {
            assert_eq!(layout.block_top(block), None, "nothing is laid out yet");
        }

        layout.cover_to(text, &blocks, &m, 60.0, 2);
        assert!(!layout.done());
        assert_eq!(layout.next_block, 2);
        assert!(close(layout.block_top(0).unwrap(), 0.0));
        assert!(close(layout.block_top(1).unwrap(), 14.08));
        assert_eq!(layout.block_top(2), None);

        layout.cover_to(text, &blocks, &m, f32::INFINITY, 1_000);
        assert!(layout.done());
        for block in 0..blocks.len() {
            assert!(layout.block_top(block).is_some(), "block {block}");
        }
        assert_eq!(layout.block_top(blocks.len()), None);

        let rule = blocks.iter().position(|b| b.kind == Kind::Rule).unwrap();
        let rule_top = layout.block_top(rule).unwrap();
        let rule_line = layout
            .lines()
            .iter()
            .find(|line| line.block as usize == rule)
            .unwrap();
        assert!(rule_line.rule);
        assert!(close(rule_line.y, rule_top));

        let last = blocks.len() - 1;
        assert_eq!(blocks[last].kind, Kind::Blank);
        assert!(close(
            layout.block_top(last).unwrap(),
            layout.total() - 14.08
        ));

        let frontier = layout.next_block;
        layout.cover_to(text, &blocks, &m, f32::INFINITY, 0);
        assert_eq!(layout.next_block, frontier, "zero budget makes no progress");
        layout.cover_to(text, &blocks, &m, layout.total() - 1.0, 10);
        assert_eq!(layout.next_block, frontier, "already covers the target");
    }

    #[test]
    fn revision_and_width_are_the_layout_cache_key() {
        let blocks = scan("a");
        let mut layout = Layout::begin_at(&blocks, 321.0, 7);
        assert_eq!(layout.revision(), 7);
        assert_eq!(layout.width(), 321.0);
        assert!(layout.matches(7, 321.0));
        assert!(layout.matches_scaled(7, 321.0, 1.0));
        assert!(!layout.matches_scaled(7, 321.0, 1.25));
        assert!(!layout.matches(8, 321.0));
        assert!(!layout.matches(7, 322.0));

        layout.set_revision(9);
        assert_eq!(layout.revision(), 9);
        assert!(layout.matches(9, 321.0));
        assert_eq!(Layout::begin(&blocks, 321.0).revision(), 0);
    }

    #[test]
    fn bad_measurer_widths_cannot_poison_hangs_or_coordinates() {
        struct Poison;
        impl Measurer for Poison {
            fn width(&self, text: &str, _style: MeasureStyle) -> f32 {
                match text {
                    "# " => f32::NAN,
                    "## " => 1.0e30,
                    "- " => -5.0,
                    "[ ] " => f32::INFINITY,
                    _ => text.chars().count() as f32 * 10.0,
                }
            }
        }

        let text = "# A\n\n## B\n\n- [ ] task with enough words to wrap a line\n";
        let blocks = scan(text);
        let mut layout = Layout::begin(&blocks, 120.0);
        layout.extend(text, &blocks, &Poison, 0);
        assert!(layout.max_hang().is_finite());
        assert!(layout.max_hang() <= MAX_HANG);
        assert!(close(layout.max_hang(), MAX_HANG));
        assert!(
            layout
                .lines()
                .iter()
                .all(|line| line.x.is_finite() && line.y.is_finite() && line.h.is_finite())
        );
        assert!(layout.total().is_finite());

        let first_heading = layout
            .lines()
            .iter()
            .find(|line| line.block as usize == 0)
            .unwrap();
        assert_eq!(first_heading.x, 0.0, "NaN marker width is zero");

        let second_heading = layout
            .lines()
            .iter()
            .find(|line| line.block as usize == 2)
            .unwrap();
        assert_eq!(second_heading.x, -MAX_HANG, "huge marker width is capped");

        let list_index = blocks
            .iter()
            .position(|block| block.kind == Kind::ListItem)
            .unwrap();
        for line in layout
            .lines()
            .iter()
            .filter(|line| line.block as usize == list_index)
        {
            assert_eq!(line.x, 16.0, "negative/infinite list widths are zero");
        }
    }

    #[test]
    fn u32_layout_coordinates_are_documented_as_clamped_at_4_gib() {
        assert_eq!(to_u32(0), 0);
        assert_eq!(to_u32(12), 12);
        assert_eq!(to_u32(u32::MAX as usize), u32::MAX);
    }

    #[test]
    fn first_at_and_block_at_index_the_page() {
        let text = "# H\n\npara\n\n---\n\nend";
        let blocks = scan(text);
        let layout = build(text, &blocks, 200.0);
        let lines = layout.lines();
        assert!(lines.len() >= 4);

        assert_eq!(layout.first_at(-100.0), 0);
        for (index, line) in lines.iter().enumerate() {
            assert_eq!(layout.first_at(line.y), index);
            assert_eq!(
                layout.first_at(line.y + line.h),
                index + 1,
                "a line whose bottom is exactly y is above the point"
            );
        }
        assert_eq!(layout.first_at(layout.total()), lines.len());
        assert_eq!(layout.first_at(layout.total() + 100.0), lines.len());

        assert_eq!(layout.block_at(-1.0), Some(0));
        assert_eq!(layout.block_at(0.0), Some(0));
        for block in 0..layout.next_block {
            let top = layout.block_top(block).unwrap();
            assert_eq!(layout.block_at(top), Some(block));
            if block > 0 {
                assert_eq!(
                    layout.block_at(top - 0.001),
                    Some(block - 1),
                    "just above block {block}"
                );
            }
        }
        let last_block = layout.next_block - 1;
        assert_eq!(layout.block_at(layout.total() + 100.0), Some(last_block));
        assert_eq!(
            layout.block_at(layout.block_y[last_block]),
            Some(last_block)
        );
    }

    #[test]
    fn an_empty_document_is_one_blank_block_with_caret_space() {
        let text = "";
        let blocks = scan(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, Kind::Blank);
        let mut layout = Layout::begin(&blocks, 200.0);
        assert!(!layout.done());
        assert_eq!(layout.block_at(0.0), None);
        assert_eq!(layout.block_top(0), None);
        assert!(layout.extend(text, &blocks, &FakeMeasurer::default(), 0));
        assert!(layout.done());
        assert!(layout.lines().is_empty());
        assert!(close(layout.total(), 14.08));
        assert_eq!(layout.block_top(0), Some(0.0));
        assert_eq!(layout.block_top(1), None);
        assert_eq!(layout.block_at(0.0), Some(0));
        assert_eq!(layout.first_at(0.0), 0);
        assert_eq!(layout.first_at(layout.total()), 0);
    }

    #[test]
    fn extend_with_a_zero_budget_places_everything() {
        let text = "one\n\n- two\n\n> three\n";
        let blocks = scan(text);
        let mut layout = Layout::begin(&blocks, 120.0);
        assert!(layout.extend(text, &blocks, &FakeMeasurer::default(), 0));
        assert!(layout.done());
        assert!(layout.total() > 0.0);
        assert!(!layout.lines().is_empty());
    }

    #[test]
    fn extend_repairs_a_document_that_gained_or_lost_blocks() {
        let short = "a\n\nb";
        let short_blocks = scan(short);
        let mut layout = build(short, &short_blocks, 200.0);

        // Append a paragraph without truncating first: extend notices that
        // there is more work and builds the tail.
        let long = "a\n\nb\n\nc";
        let long_blocks = scan(long);
        assert!(layout.extend(long, &long_blocks, &FakeMeasurer::default(), 0));
        let full = build(long, &long_blocks, 200.0);
        assert_eq!(layout.lines(), full.lines());
        assert!(close(layout.total(), full.total()));

        // Shrink without truncating: stale lines for removed blocks must not
        // survive, and the page must match a fresh build.
        assert!(
            layout.extend(short, &short_blocks, &FakeMeasurer::default(), 0),
            "the layout is done again after repairing"
        );
        let full = build(short, &short_blocks, 200.0);
        assert_eq!(layout.lines(), full.lines());
        assert!(close(layout.total(), full.total()));
    }

    #[test]
    fn inline_kinds_change_the_break() {
        // Bold "bb" widens the prefix, so this paragraph needs two lines.
        // Measured as one face it would fit on one.
        let text = "aa **bb** cc";
        let blocks = scan(text);
        assert_eq!(blocks.len(), 1);
        let layout = build(text, &blocks, 120.0);
        let lines = layout.lines();
        assert_eq!(lines.len(), 2, "bold text must be measured bold");
        assert_eq!(line_text(text, &blocks, &lines[0]), "aa **bb**");
        assert_eq!(line_text(text, &blocks, &lines[1]), "cc");
    }

    #[test]
    fn body_face_is_the_style_modules_body_size() {
        let look = crate::style::look(blocks::Block {
            kind: Kind::Paragraph,
            level: 0,
            start: 0,
            end: 0,
            marker: 0,
        });
        assert_eq!(look.size, crate::style::BODY);
        assert_eq!(BODY.size, crate::style::BODY);
    }
    #[test]
    fn rewrapping_from_a_visual_line_matches_a_fresh_build() {
        let text = (0..80)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let blocks = scan(&text);
        let width = 180.0;
        let mut layout = build(&text, &blocks, width);
        let original = layout.lines().to_vec();
        assert!(original.len() > 8);
        let line = original[5];
        let from = line.start as usize;
        let prefix = original[..5].to_vec();
        assert!(layout.truncate_block_from_offset(0, from).is_some());
        assert!(layout.rewrap_block_from(&text, &blocks, &FakeMeasurer::default(), 0, from));
        layout.extend(&text, &blocks, &FakeMeasurer::default(), 0);
        let fresh = build(&text, &blocks, width);
        assert_eq!(layout.lines(), fresh.lines());
        assert_eq!(&layout.lines()[..5], &prefix[..]);
    }

    #[test]
    fn rewrapping_a_task_suffix_keeps_the_continuation_hang() {
        let text = "- [ ] task with enough words to wrap and wrap again";
        let blocks = scan(text);
        let mut layout = build(text, &blocks, 110.0);
        let lines = layout.lines().to_vec();
        assert!(lines.len() > 2);
        let from = lines[1].start as usize;
        assert!(layout.truncate_block_from_offset(0, from).is_some());
        assert!(layout.rewrap_block_from(text, &blocks, &FakeMeasurer::default(), 0, from));
        layout.extend(text, &blocks, &FakeMeasurer::default(), 0);
        let fresh = build(text, &blocks, 110.0);
        assert_eq!(layout.lines(), fresh.lines());
    }
    #[test]
    fn line_level_rewrap_measures_only_the_suffix() {
        struct Counting {
            inner: FakeMeasurer,
            calls: std::cell::Cell<usize>,
        }
        impl Measurer for Counting {
            fn width(&self, text: &str, style: MeasureStyle) -> f32 {
                self.calls.set(self.calls.get() + 1);
                self.inner.width(text, style)
            }
        }

        let text = (0..600)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let blocks = scan(&text);
        let initial = Counting {
            inner: FakeMeasurer::default(),
            calls: std::cell::Cell::new(0),
        };
        let mut layout = Layout::begin(&blocks, 180.0);
        layout.extend(&text, &blocks, &initial, 0);
        let line = layout.lines()[layout.lines().len() * 3 / 4];
        let from = line.start as usize;
        initial.calls.set(0);
        assert!(layout.truncate_block_from_offset(0, from).is_some());
        assert!(layout.rewrap_block_from(&text, &blocks, &initial, 0, from));
        let suffix_calls = initial.calls.get();

        let full = Counting {
            inner: FakeMeasurer::default(),
            calls: std::cell::Cell::new(0),
        };
        let mut fresh = Layout::begin(&blocks, 180.0);
        fresh.extend(&text, &blocks, &full, 0);
        assert!(
            suffix_calls < full.calls.get() / 2,
            "suffix measured {suffix_calls} times, full {full_calls}",
            full_calls = full.calls.get()
        );
    }
    #[test]
    fn scaled_incremental_rewrap_matches_a_fresh_scaled_layout() {
        let text = (0..80)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let blocks = scan(&text);
        let mut layout = Layout::begin_at_scaled(&blocks, 180.0, 7, 1.25);
        layout.extend(&text, &blocks, &FakeMeasurer::default(), 0);
        let line = layout.lines()[layout.lines().len() / 2];
        let from = line.start as usize;
        assert!(layout.truncate_block_from_offset(0, from).is_some());
        assert!(layout.rewrap_block_from(&text, &blocks, &FakeMeasurer::default(), 0, from));
        layout.extend(&text, &blocks, &FakeMeasurer::default(), 0);
        let mut fresh = Layout::begin_at_scaled(&blocks, 180.0, 7, 1.25);
        fresh.extend(&text, &blocks, &FakeMeasurer::default(), 0);
        assert_eq!(layout.lines(), fresh.lines());
        assert!(
            fresh.total() > {
                let mut base = Layout::begin(&blocks, 180.0);
                base.extend(&text, &blocks, &FakeMeasurer::default(), 0);
                base.total()
            }
        );
    }

    #[test]
    fn non_finite_scale_falls_back_to_unit_scale_in_layout() {
        let text = "# Heading\n\nparagraph body";
        let blocks = scan(text);
        let mut unit = Layout::begin_at_scaled(&blocks, 200.0, 1, 1.0);
        unit.extend(text, &blocks, &FakeMeasurer::default(), 0);

        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut layout = Layout::begin_at_scaled(&blocks, 200.0, 1, bad);
            layout.extend(text, &blocks, &FakeMeasurer::default(), 0);
            assert_eq!(layout.lines(), unit.lines());
            assert!(close(layout.total(), unit.total()));
            assert!(layout.matches_scaled(1, 200.0, 1.0));
        }
    }
}
