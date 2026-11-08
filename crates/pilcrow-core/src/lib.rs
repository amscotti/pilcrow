//! The pure document model.
//!
//! Nothing in this crate may depend on GPUI, the filesystem, or a window.
//! The editor supplies a measurer; tests supply a fake one.

#![forbid(unsafe_code)]

pub mod blocks;
pub mod buffer;
pub mod display;
pub mod export;
pub mod format;
pub mod history;
pub mod inline;
pub mod layout;
pub mod lists;
pub mod search;
pub mod stats;
pub mod style;
pub mod wrap;

pub use blocks::{Block, Kind as BlockKind, first_changed, rescan_from, scan as scan_blocks};
pub use buffer::{Buffer, boundary_at_or_before, content_hash, next_boundary, prev_boundary};
pub use format::{
    Command as FormatCommand, FormatEdit, Placement as FormatPlacement, command as format_command,
};
pub use history::{Applied, Edit, Group as EditGroup, History};
pub use inline::{Kind as InlineKind, Link, kinds as inline_kinds, link_at, links as inline_links};
pub use layout::{Layout, Placed};
pub use stats::{Stats, compute as compute_stats};
pub use style::{BODY, LEADING, Look, line_height, look};
pub use wrap::{Line, MeasureStyle, Measurer, width_range, wrap as wrap_lines};

/// A provisional placeholder until the buffer lands.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert!(!super::version().is_empty());
    }
}
