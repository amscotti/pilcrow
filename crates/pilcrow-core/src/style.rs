//! What a block looks like, derived from what it is.
//!
//! Sizes and weights are here rather than in the view so the type scale is one
//! table that can be argued with, and so the reading view can render the same
//! document with a different table instead of a different parser. Colours are
//! the UI's business; this module only says how big and how far in.

use crate::blocks::{Block, Kind};

/// The body size everything else is a multiple of.
pub const BODY: f32 = 16.0;

/// Line height, as a multiple of the size.
pub const LEADING: f32 = 1.6;

/// The type scale and spacing of one block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub size: f32,
    pub bold: bool,
    /// Offset of the block's text from the column edge.
    pub indent: f32,
    /// Space after the block, in points.
    pub gap: f32,
    /// Space before the block, in points. Headings need air above them or the
    /// page reads as a list of lines rather than a document.
    pub gap_before: f32,
    /// A horizontal rule instead of text.
    pub is_rule: bool,
}

/// The look of `block`; a block's kind and heading level are all it takes.
pub fn look(block: Block) -> Look {
    match block.kind {
        Kind::Blank => Look {
            size: BODY,
            bold: false,
            indent: 0.0,
            // A blank line is the page's paragraph break, not a line of text:
            // slightly more than a paragraph gap, a little less than a full
            // line of air.
            gap: BODY * LEADING * 0.55,
            gap_before: 0.0,
            is_rule: false,
        },
        Kind::Paragraph => Look {
            size: BODY,
            bold: false,
            indent: 0.0,
            gap: 10.0,
            gap_before: 0.0,
            is_rule: false,
        },
        Kind::Heading => match block.level {
            1 => Look {
                size: BODY * 1.75,
                bold: true,
                indent: 0.0,
                gap: 18.0,
                gap_before: 28.0,
                is_rule: false,
            },
            2 => Look {
                size: BODY * 1.375,
                bold: true,
                indent: 0.0,
                gap: 16.0,
                gap_before: 24.0,
                is_rule: false,
            },
            3 => Look {
                size: BODY * 1.125,
                bold: true,
                indent: 0.0,
                gap: 12.0,
                gap_before: 18.0,
                is_rule: false,
            },
            _ => Look {
                size: BODY,
                bold: true,
                indent: 0.0,
                gap: 6.0,
                gap_before: 12.0,
                is_rule: false,
            },
        },
        // Code is the one place a smaller size is right: monospace already
        // reads larger than the same point size of prose.
        Kind::Code => Look {
            size: BODY * 0.92,
            bold: false,
            indent: 0.0,
            gap: 0.0,
            gap_before: 0.0,
            is_rule: false,
        },
        Kind::Quote => Look {
            size: BODY,
            bold: false,
            indent: 18.0,
            gap: 8.0,
            gap_before: 0.0,
            is_rule: false,
        },
        Kind::ListItem => Look {
            size: BODY,
            bold: false,
            indent: 16.0,
            gap: 4.0,
            gap_before: 0.0,
            is_rule: false,
        },
        Kind::Rule => Look {
            size: BODY,
            bold: false,
            indent: 0.0,
            gap: 14.0,
            gap_before: 0.0,
            is_rule: true,
        },
        Kind::Frontmatter => Look {
            size: BODY * 0.82,
            bold: false,
            indent: 0.0,
            gap: 0.0,
            gap_before: 0.0,
            is_rule: false,
        },
    }
}

/// The same look at a reader-selected text size. Spacing scales with the
/// type so the page keeps its rhythm instead of growing cramped.
pub fn look_scaled(block: Block, scale: f32) -> Look {
    let mut look = look(block);
    let scale = if scale.is_finite() {
        scale.clamp(0.75, 1.75)
    } else {
        1.0
    };
    look.size *= scale;
    look.indent *= scale;
    look.gap *= scale;
    look.gap_before *= scale;
    look
}

/// The line height of a block, in points.
pub fn line_height(look: Look) -> f32 {
    look.size * LEADING
}

#[cfg(test)]
mod tests {
    use super::{BODY, LEADING, line_height, look};
    use crate::blocks::{Block, Kind};

    fn block(kind: Kind, level: u8) -> Block {
        Block {
            kind,
            level,
            start: 0,
            end: 0,
            marker: 0,
        }
    }

    fn heading(level: u8) -> Block {
        block(Kind::Heading, level)
    }

    #[test]
    fn body_and_leading_are_the_page_constants() {
        assert_eq!(BODY, 16.0);
        assert_eq!(LEADING, 1.6);
        let paragraph = look(block(Kind::Paragraph, 0));
        assert_eq!(paragraph.size, 16.0);
        assert_eq!(line_height(paragraph), 25.6);
    }

    #[test]
    fn heading_ladder_is_one_and_three_quarters_to_body() {
        let sizes: Vec<f32> = (1..=6).map(|level| look(heading(level)).size).collect();
        assert_eq!(sizes, [28.0, 22.0, 18.0, 16.0, 16.0, 16.0]);
        for level in 1..=6 {
            assert!(look(heading(level)).bold, "H{level} is bold");
            assert!(!look(heading(level)).is_rule);
        }
        assert_eq!(line_height(look(heading(1))), 44.8);
        assert_eq!(line_height(look(heading(4))), 25.6);
    }

    #[test]
    fn heading_air_grows_with_the_level() {
        let h1 = look(heading(1));
        let h2 = look(heading(2));
        let h3 = look(heading(3));
        let h4 = look(heading(4));
        assert_eq!((h1.gap, h1.gap_before), (18.0, 28.0));
        assert_eq!((h2.gap, h2.gap_before), (16.0, 24.0));
        assert_eq!((h3.gap, h3.gap_before), (12.0, 18.0));
        assert_eq!((h4.gap, h4.gap_before), (6.0, 12.0));
        assert!(h1.gap_before > h2.gap_before);
        assert!(h2.gap_before > h3.gap_before);
        assert!(h3.gap_before > h4.gap_before);
    }

    #[test]
    fn paragraphs_quotes_and_lists_indent_as_the_page_does() {
        let paragraph = look(block(Kind::Paragraph, 0));
        assert_eq!((paragraph.indent, paragraph.gap), (0.0, 10.0));

        let quote = look(block(Kind::Quote, 0));
        assert_eq!((quote.indent, quote.gap), (18.0, 8.0));

        let item = look(block(Kind::ListItem, 0));
        assert_eq!((item.indent, item.gap), (16.0, 4.0));
    }

    #[test]
    fn blank_lines_are_air_without_a_rule() {
        let blank = look(block(Kind::Blank, 0));
        assert_eq!(blank.gap, BODY * LEADING * 0.55);
        assert!((blank.gap - 14.0).abs() < 0.1);
        assert_eq!(blank.gap_before, 0.0);
        assert!(!blank.is_rule);
        assert_eq!(blank.size, BODY);
    }

    #[test]
    fn rules_are_a_hairline_with_their_own_gap() {
        let rule = look(block(Kind::Rule, 0));
        assert!(rule.is_rule);
        assert_eq!(rule.gap, 14.0);
        assert_eq!(rule.indent, 0.0);
        assert_eq!(rule.size, BODY);
    }

    #[test]
    fn code_and_frontmatter_are_smaller_and_tight() {
        let code = look(block(Kind::Code, 0));
        assert!((code.size - BODY * 0.92).abs() < 1e-6);
        assert!((code.size - 14.72).abs() < 1e-4);
        assert_eq!(code.gap, 0.0);
        assert!(!code.is_rule);
        assert!(!code.bold);

        let frontmatter = look(block(Kind::Frontmatter, 0));
        assert!((frontmatter.size - BODY * 0.82).abs() < 1e-6);
        assert_eq!(frontmatter.gap, 0.0);
        assert!(!frontmatter.bold);
    }

    #[test]
    fn line_height_tracks_the_block_size() {
        for kind in [
            Kind::Paragraph,
            Kind::Code,
            Kind::Quote,
            Kind::Blank,
            Kind::Rule,
        ] {
            let look = look(block(kind, 0));
            assert!((line_height(look) - look.size * LEADING).abs() < f32::EPSILON);
        }
        let nan = super::look_scaled(block(Kind::Paragraph, 0), f32::NAN);
        assert_eq!(nan.size, BODY);
    }
}
