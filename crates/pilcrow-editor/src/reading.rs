//! The reading page. A display copy of the source, rendered with the same
//! type scale but without markers, a caret, or edit affordances.

use gpui::prelude::*;
use gpui::{ElementId, FocusHandle, FontWeight, Role, ScrollHandle, SharedString, div, px};
use pilcrow_core::{Block, BlockKind, display, style};
use pilcrow_ui::theme::Tokens;

use crate::BODY_FONT;

pub(crate) fn page(
    text: &str,
    width: f32,
    focus: &FocusHandle,
    tokens: &Tokens,
    scroll: &ScrollHandle,
    scale: f32,
) -> impl IntoElement {
    let mut page = div()
        .id("reading-page")
        .debug_selector(|| "reading-page".into())
        .role(Role::Document)
        .aria_label("Reading view")
        .track_focus(focus)
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .items_center()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .py(px(40.0));
    for (index, block) in display::blocks(text).into_iter().enumerate() {
        let look = style::look_scaled(
            Block {
                kind: block.kind,
                level: block.level,
                start: 0,
                end: 0,
                marker: 0,
            },
            scale,
        );
        let row = if block.kind == BlockKind::Rule {
            div()
                .id(ElementId::Name(format!("reading-{index}").into()))
                .h(px(1.0))
                .w(px(width))
                .mb(px(look.gap))
                .bg(tokens.rule)
        } else {
            let mut row = div()
                .id(ElementId::Name(format!("reading-{index}").into()))
                .w(px(width))
                .text_size(px(look.size))
                .line_height(px(look.size * 1.6))
                .pl(px(look.indent))
                .mb(px(look.gap))
                .text_color(tokens.ink)
                .child(SharedString::from(block.text));
            if look.bold {
                row = row.font_weight(FontWeight::SEMIBOLD);
            }
            if block.kind == BlockKind::Code {
                row = row
                    .font_family(BODY_FONT)
                    .text_size(px(look.size * 0.92))
                    .text_color(tokens.mid);
            } else if block.kind == BlockKind::Quote {
                row = row.text_color(tokens.mid);
            }
            row
        };
        page = page.child(row);
    }
    page
}
