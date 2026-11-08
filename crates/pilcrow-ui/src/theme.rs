//! Color tokens and the contrast checks that keep the page readable.
//!
//! The values are the measured light and dark palettes from the rebuild plan.
//! Colors are stored as [`Hsla`] because that is what GPUI paints; the
//! contrast helper converts back to sRGB for the WCAG calculation.

use gpui::{Hsla, rgb};

/// The colors shared by the page and its chrome.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    /// The page background.
    pub paper: Hsla,
    /// Body text and the caret.
    pub ink: Hsla,
    /// Secondary text and focus-dimmed body.
    pub mid: Hsla,
    /// Markers, counts and other quiet furniture.
    pub dim: Hsla,
    /// Hairlines and code edges.
    pub rule: Hsla,
    /// Links and the focus ring.
    pub accent: Hsla,
    /// The flat selection wash.
    pub selection: Hsla,
    /// The tint behind code.
    pub code_bg: Hsla,
    /// The wavy spell underline.
    pub spell: Hsla,
}

/// The light page, measured from the current app.
pub fn light() -> Tokens {
    Tokens {
        paper: hex(0xFBFBFA),
        ink: hex(0x1A1A1A),
        mid: hex(0x6D6D6D),
        dim: hex(0x6D6D6D),
        rule: hex(0xC4C4C0),
        accent: hex(0x1A5CCC),
        selection: hex(0xD6E4FA),
        code_bg: hex(0xEAEAE6),
        spell: hex(0xC0392B),
    }
}

/// The dark page. Used once system theme selection lands.
pub fn dark() -> Tokens {
    Tokens {
        paper: hex(0x101011),
        ink: hex(0xE6E6E3),
        mid: hex(0x838383),
        dim: hex(0x8A8A8A),
        rule: hex(0x3C3C3F),
        accent: hex(0x4C9AFF),
        selection: hex(0x20344F),
        code_bg: hex(0x1E1E21),
        spell: hex(0xE0655B),
    }
}

/// The light page at maximum contrast: true white and black, every text
/// role at WCAG AAA (7:1 or better) against the paper. The grayscale
/// ramp and blue accent keep it recognizably the same page.
pub fn light_high_contrast() -> Tokens {
    Tokens {
        paper: hex(0xFFFFFF),
        ink: hex(0x000000),
        mid: hex(0x2D2D2D),
        dim: hex(0x545454),
        rule: hex(0x767676),
        accent: hex(0x002FA3),
        selection: hex(0xB9D2FF),
        code_bg: hex(0xE8E8E8),
        spell: hex(0xA60000),
    }
}

/// The dark page at maximum contrast: true black and white, same AAA
/// floors.
pub fn dark_high_contrast() -> Tokens {
    Tokens {
        paper: hex(0x000000),
        ink: hex(0xFFFFFF),
        mid: hex(0xD9D9D9),
        dim: hex(0xA6A6A6),
        rule: hex(0x8C8C8C),
        accent: hex(0x8AB8FF),
        selection: hex(0x24385C),
        code_bg: hex(0x1C1C1C),
        spell: hex(0xFF9E94),
    }
}

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

/// WCAG 2.x relative contrast between two opaque colors.
///
/// Returns a ratio in `1.0..=21.0`. Alpha is ignored; every token in this
/// module is opaque, and a translucent wash is measured against its paper
/// before it is made a token.
pub fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
    let a = relative_luminance(a.to_rgb());
    let b = relative_luminance(b.to_rgb());
    let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
}

fn relative_luminance(color: gpui::Rgba) -> f32 {
    fn linear(channel: f32) -> f32 {
        let channel = channel.clamp(0.0, 1.0);
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }

    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn themes() -> [(&'static str, Tokens); 4] {
        [
            ("light", light()),
            ("dark", dark()),
            ("light high contrast", light_high_contrast()),
            ("dark high contrast", dark_high_contrast()),
        ]
    }

    #[test]
    fn high_contrast_themes_meet_aaa_floors() {
        for (name, tokens) in [
            ("light high contrast", light_high_contrast()),
            ("dark high contrast", dark_high_contrast()),
        ] {
            let ink = contrast_ratio(tokens.ink, tokens.paper);
            assert!(ink >= 15.0, "{name} body contrast was {ink:.2}:1");
            for (role, color) in [
                ("mid", tokens.mid),
                ("dim", tokens.dim),
                ("accent", tokens.accent),
            ] {
                let ratio = contrast_ratio(color, tokens.paper);
                assert!(ratio >= 7.0, "{name} {role} contrast was {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn body_ink_clears_twelve_to_one() {
        for (name, tokens) in themes() {
            let ratio = contrast_ratio(tokens.ink, tokens.paper);
            assert!(ratio >= 12.0, "{name} body contrast was {ratio:.2}:1");
        }
    }

    #[test]
    fn secondary_and_quiet_text_clear_their_floors() {
        for (name, tokens) in themes() {
            let mid = contrast_ratio(tokens.mid, tokens.paper);
            let dim = contrast_ratio(tokens.dim, tokens.paper);
            assert!(mid >= 4.5, "{name} mid contrast was {mid:.2}:1");
            assert!(
                dim >= 4.5,
                "{name} dim text contrast was {dim:.2}:1; raise it or use mid"
            );
        }
    }

    #[test]
    fn accent_clears_normal_text() {
        let tokens = light();
        assert!(contrast_ratio(tokens.accent, tokens.paper) >= 4.5);
        assert!(contrast_ratio(tokens.accent, tokens.selection) >= 4.5);
    }

    #[test]
    fn tokens_are_distinct_and_washes_are_visible() {
        for (name, tokens) in themes() {
            assert_ne!(tokens.paper, tokens.ink, "{name} paper equals ink");
            assert_ne!(tokens.paper, tokens.mid, "{name} paper equals mid");
            assert_ne!(
                tokens.paper, tokens.selection,
                "{name} selection is invisible"
            );
            assert_ne!(
                tokens.paper, tokens.code_bg,
                "{name} code background is invisible"
            );
            assert_ne!(tokens.ink, tokens.mid, "{name} mid equals ink");
            assert_ne!(tokens.rule, tokens.paper, "{name} rule is invisible");
        }
    }

    #[test]
    fn contrast_is_symmetric_and_bounded() {
        let tokens = light();
        let forward = contrast_ratio(tokens.ink, tokens.paper);
        let backward = contrast_ratio(tokens.paper, tokens.ink);
        assert!((forward - backward).abs() < 0.001);
        assert!(forward <= 21.0);
        assert!((contrast_ratio(tokens.paper, tokens.paper) - 1.0).abs() < 0.001);
    }
}
