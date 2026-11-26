//! A small status pill after shadcn's Badge: fully rounded, 11px medium text.

use gpui::{Div, FontWeight, ParentElement, SharedString, Styled, div, px};

use crate::panels::theme::theme;
use crate::panels::scale;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variant {
    /// Dark fill.
    #[default]
    Secondary,
    /// Just an outline.
    Outline,
}

pub fn badge(label: impl Into<SharedString>, variant: Variant) -> Div {
    let base = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.0))
        .px(px(7.0))
        .py(px(1.0))
        .rounded_full()
        .border_1()
        .border_color(gpui::transparent_black())
        .whitespace_nowrap()
        .text_size(scale::px(11.0))
        .font_weight(FontWeight::MEDIUM);
    let styled = match variant {
        Variant::Secondary => base.bg(theme().secondary).text_color(theme().foreground),
        Variant::Outline => base.border_color(theme().panel_border).text_color(theme().foreground),
    };
    styled.child(label.into())
}

/// A state pill in its own `color` (open, merged, failing, draft): a tinted fill, a border and text in that color,
/// with `glyph` first.
pub fn status(glyph: Option<gpui_kit::assets::IconName>, label: impl Into<SharedString>, color: gpui::Hsla) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(4.0))
        .h(px(22.0))
        .px(px(8.0))
        .rounded_full()
        .border_1()
        .border_color(color.opacity(0.45))
        .bg(color.opacity(0.14))
        .text_color(color)
        .whitespace_nowrap()
        .text_size(scale::px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .children(glyph.map(|g| crate::panels::widgets::icon(g, 13.0)))
        .child(label.into())
}
