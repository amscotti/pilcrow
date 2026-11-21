//! The reading page. A display copy of the source, rendered with the same
//! type scale but without markers, a caret, or edit affordances.

use std::ops::Range;

use gpui::prelude::*;
use gpui::{
    ElementId, FocusHandle, FontStyle, FontWeight, HighlightStyle, InteractiveText, Role,
    ScrollHandle, SharedString, StrikethroughStyle, StyledText, UnderlineStyle, div, px,
};
use pilcrow_core::inline::Kind as InlineKind;
use pilcrow_core::{Block, BlockKind, display, export, style};
use pilcrow_ui::theme::Tokens;

use crate::BODY_FONT;

pub(crate) struct ReadingHighlights {
    pub(crate) highlights: Vec<(Range<usize>, HighlightStyle)>,
    pub(crate) click_ranges: Vec<Range<usize>>,
    pub(crate) click_urls: Vec<String>,
}

/// Flatten potentially overlapping inline spans and links into sorted,
/// non-overlapping highlights for GPUI's `StyledText::with_highlights`, plus
/// clickable external link ranges and their destination URLs.
pub(crate) fn build_reading_highlights(
    text: &str,
    spans: &[display::DisplaySpan],
    links: &[display::DisplayLink],
    tokens: &Tokens,
) -> ReadingHighlights {
    if text.is_empty() {
        return ReadingHighlights {
            highlights: Vec::new(),
            click_ranges: Vec::new(),
            click_urls: Vec::new(),
        };
    }
    let len = text.len();
    let mut points = Vec::with_capacity(2 + (spans.len() + links.len()) * 2);
    points.push(0);
    points.push(len);
    for span in spans {
        let s = span.range.start.min(len);
        let e = span.range.end.min(len);
        if s < e && text.is_char_boundary(s) && text.is_char_boundary(e) {
            points.push(s);
            points.push(e);
        }
    }
    for link in links {
        let s = link.range.start.min(len);
        let e = link.range.end.min(len);
        if s < e && text.is_char_boundary(s) && text.is_char_boundary(e) {
            points.push(s);
            points.push(e);
        }
    }
    points.sort_unstable();
    points.dedup();

    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b {
            continue;
        }
        let mut style = HighlightStyle::default();
        let mut styled = false;
        for span in spans {
            if span.range.start <= a && b <= span.range.end {
                styled = true;
                match span.kind {
                    InlineKind::Bold => style.font_weight = Some(FontWeight::BOLD),
                    InlineKind::Italic => style.font_style = Some(FontStyle::Italic),
                    InlineKind::Strike => {
                        style.strikethrough = Some(StrikethroughStyle {
                            thickness: px(1.0),
                            color: Some(tokens.ink),
                        });
                    }
                    InlineKind::Code => style.background_color = Some(tokens.code_bg),
                    InlineKind::Plain | InlineKind::Marker => {}
                }
            }
        }
        for link in links {
            if link.range.start <= a && b <= link.range.end {
                styled = true;
                style.color = Some(tokens.accent);
                style.underline = Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(tokens.accent),
                    wavy: false,
                });
            }
        }
        if styled && style != HighlightStyle::default() {
            if let Some((last_range, last_style)) = highlights.last_mut()
                && last_range.end == a
                && *last_style == style
            {
                last_range.end = b;
            } else {
                highlights.push((a..b, style));
            }
        }
    }

    let mut click_ranges = Vec::new();
    let mut click_urls = Vec::new();
    for link in links {
        let s = link.range.start.min(len);
        let e = link.range.end.min(len);
        if s < e
            && text.is_char_boundary(s)
            && text.is_char_boundary(e)
            && export::is_external_url(&link.url)
        {
            click_ranges.push(s..e);
            click_urls.push(link.url.clone());
        }
    }

    ReadingHighlights {
        highlights,
        click_ranges,
        click_urls,
    }
}

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
        .font_family(BODY_FONT)
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
                .when(index > 0 && look.gap_before > 0.0, |row| {
                    row.mt(px(look.gap_before))
                })
                .mb(px(look.gap))
                .bg(tokens.rule)
        } else {
            let ReadingHighlights {
                highlights,
                click_ranges,
                click_urls,
            } = build_reading_highlights(&block.text, &block.spans, &block.links, tokens);
            let content = if block.text.is_empty() {
                SharedString::from("").into_any_element()
            } else {
                let styled =
                    StyledText::new(SharedString::from(block.text)).with_highlights(highlights);
                if click_ranges.is_empty() {
                    styled.into_any_element()
                } else {
                    InteractiveText::new(
                        ElementId::Name(format!("reading-text-{index}").into()),
                        styled,
                    )
                    .on_click(click_ranges, move |range_ix, _window, cx| {
                        if let Some(url) = click_urls.get(range_ix) {
                            cx.open_url(url);
                        }
                    })
                    .into_any_element()
                }
            };
            let indent = if block.kind == BlockKind::ListItem {
                look.indent + block.depth as f32 * 16.0
            } else {
                look.indent
            };
            let mut row = div()
                .id(ElementId::Name(format!("reading-{index}").into()))
                .w(px(width))
                .text_size(px(look.size))
                .line_height(px(look.size * 1.6))
                .pl(px(indent))
                .when(index > 0 && look.gap_before > 0.0, |row| {
                    row.mt(px(look.gap_before))
                })
                .mb(px(look.gap))
                .text_color(tokens.ink)
                .child(content);
            if look.bold {
                row = row.font_weight(FontWeight::SEMIBOLD);
            }
            if block.kind == BlockKind::Code {
                row = row
                    .bg(tokens.code_bg)
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(4.0))
                    .text_color(tokens.mid);
            } else if block.kind == BlockKind::Quote {
                row = row
                    .border_l_2()
                    .border_color(tokens.rule)
                    .text_color(tokens.mid);
            }
            row
        };
        page = page.child(row);
    }
    page
}
