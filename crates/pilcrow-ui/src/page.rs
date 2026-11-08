//! Page geometry: a measured column, offset left by a width-dependent margin,
//! under a fixed head.
//!
//! The column width itself is measured by the editor in the face it actually
//! paints. This module owns the surrounding arithmetic so the caret, clicks
//! and chrome all agree on where the page is.

use crate::typography::HEAD;

/// The placed page within a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Page {
    /// Empty space above the first line.
    pub head: f32,
    /// Space between the window's left edge and the column.
    pub margin: f32,
    /// The measured text column.
    pub column: f32,
}

/// The left margin: an offset that grows with the window, clamped so a narrow
/// window keeps a readable gutter and a wide one does not push the page away.
pub fn margin_for(window_width: f32) -> f32 {
    (window_width * 0.17).clamp(72.0, 240.0)
}

/// Place a measured column in a window.
pub fn page_for(window_width: f32, column: f32) -> Page {
    Page {
        head: HEAD,
        margin: margin_for(window_width),
        column,
    }
}

/// The true space to the right of the column. This can be negative when the
/// window is narrower than the page; check [`fits`] rather than hiding the
/// overflow behind a floor.
pub fn right_for(window_width: f32, page: Page) -> f32 {
    window_width - page.margin - page.column
}

/// The smallest window width that leaves at least 24 pt to the right of the
/// measured column while keeping the plan's 720 pt floor.
pub fn min_width_for(column: f32) -> f32 {
    ((column + 32.0) / 0.83).max(720.0)
}

/// Whether the page has a real right gutter in this window.
pub fn fits(window_width: f32, page: Page) -> bool {
    right_for(window_width, page) >= 24.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 66 IBM Plex Mono `x`s at 16 px is about 634 points (0.6 em advance).
    /// The exact value comes from the text system in the editor; this is
    /// only a stable stand-in here.
    const COLUMN: f32 = 633.6;

    #[test]
    fn page_geometry_is_stable_across_widths() {
        for width in [720.0, 800.0, 1000.0, 1600.0] {
            let page = page_for(width, COLUMN);
            assert_eq!(page.head, 64.0, "head changed at {width}");
            assert_eq!(page.column, COLUMN, "column changed at {width}");
            assert!(
                (72.0..=240.0).contains(&page.margin),
                "margin {} out of range at {width}",
                page.margin
            );
            assert!(page.column > 0.0, "empty column at {width}");
            if width >= min_width_for(COLUMN) {
                assert!(
                    fits(width, page),
                    "right gutter collapsed at legal width {width}"
                );
            }
            if width == 720.0 {
                assert!(
                    !fits(width, page),
                    "the known-overflowing 720 pt window was reported as fitting"
                );
            }
        }
    }

    #[test]
    fn minimum_width_leaves_a_real_gutter() {
        let min = min_width_for(COLUMN);
        let page = page_for(min, COLUMN);
        assert!(fits(min, page));
        assert!(right_for(min, page) >= 24.0);
    }

    #[test]
    fn margin_only_grows_until_the_cap() {
        assert_eq!(margin_for(0.0), 72.0);
        assert_eq!(margin_for(720.0), 122.4);
        assert_eq!(margin_for(1000.0), 170.0);
        assert_eq!(margin_for(1600.0), 240.0);
        assert_eq!(margin_for(4000.0), 240.0);
        assert!(margin_for(800.0) < margin_for(1000.0));
    }

    #[test]
    fn a_wide_window_leaves_the_column_where_it_is() {
        let narrow = page_for(1000.0, COLUMN);
        let wide = page_for(1600.0, COLUMN);
        assert_eq!(narrow.column, wide.column);
        assert!(right_for(1600.0, wide) > right_for(1000.0, narrow));
    }
}
