//! A small status pill after shadcn's Badge: fully rounded, 11px medium text.

use gpui::{Div, FontWeight, ParentElement, SharedString, Styled, div, px};

use crate::panels::theme::theme;
use crate::panels::scale;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variant {
    /// Light fill, for the one thing to notice.
    Default,
    /// Dark fill.
    #[default]
    Secondary,
    /// Just an outline.
    Outline,
    Destructive,
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
        Variant::Default => base.bg(theme().primary).text_color(theme().primary_foreground),
        Variant::Secondary => base.bg(theme().secondary).text_color(theme().foreground),
        Variant::Outline => base.border_color(theme().panel_border).text_color(theme().foreground),
        Variant::Destructive => base.bg(theme().destructive.opacity(0.6)).text_color(theme().foreground),
    };
    styled.child(label.into())
}
