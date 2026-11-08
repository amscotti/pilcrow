//! The type scale for the writing page.
//!
//! The body size and leading are the plan's provisional values until IBM Plex
//! Mono is bundled. The measure and head height are shared with the page
//! geometry so the editor and the design system cannot drift.

/// Monospace body size in points.
pub const BODY_SIZE: f32 = 16.0;

/// Line height as a multiple of [`BODY_SIZE`].
pub const LEADING: f32 = 1.6;

/// The column is measured as this many `x` characters in the actual face.
pub const MEASURE_CHARS: usize = 66;

/// The empty space above the first line.
pub const HEAD: f32 = 64.0;

/// Body size, leading and the six heading sizes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypeScale {
    /// Body text size in points.
    pub body: f32,
    /// Line height as a multiple of `body`.
    pub leading: f32,
    /// Heading sizes, H1 through H6, in points.
    pub heading: [f32; 6],
}

impl Default for TypeScale {
    fn default() -> Self {
        Self {
            body: BODY_SIZE,
            leading: LEADING,
            heading: [1.75, 1.375, 1.125, 1.0, 1.0, 1.0].map(|scale| scale * BODY_SIZE),
        }
    }
}

/// The body line height in points: `body * leading`.
pub const fn line_height() -> f32 {
    BODY_SIZE * LEADING
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_ladder_is_one_and_three_quarters_to_body() {
        let scale = TypeScale::default();
        assert_eq!(scale.body, 16.0);
        assert_eq!(scale.leading, 1.6);
        assert_eq!(scale.heading[0], 28.0);
        assert_eq!(scale.heading[1], 22.0);
        assert_eq!(scale.heading[2], 18.0);
        assert_eq!(scale.heading[3], 16.0);
        assert_eq!(scale.heading[4], 16.0);
        assert_eq!(scale.heading[5], 16.0);
    }

    #[test]
    fn heading_sizes_do_not_increase_down_the_ladder() {
        let scale = TypeScale::default();
        for pair in scale.heading.windows(2) {
            assert!(pair[0] >= pair[1], "heading ladder is not monotonic");
        }
        assert!(scale.heading[0] > scale.body);
    }

    #[test]
    fn line_height_is_body_times_leading() {
        let scale = TypeScale::default();
        assert!((line_height() - scale.body * scale.leading).abs() < f32::EPSILON);
        assert_eq!(line_height(), 25.6);
    }

    #[test]
    fn measure_and_head_match_the_page() {
        assert_eq!(MEASURE_CHARS, 66);
        assert_eq!(HEAD, 64.0);
    }
}
