//! A keycap after shadcn's Kbd, for shortcut hints in menus, tooltips and help text.

use gpui::{Div, FontWeight, ParentElement, SharedString, Styled, div, px};

use super::token;
use crate::panels::scale;

/// `keys` as one keycap, such as "Esc" or "Ctrl+K".
pub fn kbd(keys: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .h(px(18.0))
        .min_w(px(18.0))
        .px(px(4.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .bg(token::secondary())
        .border_1()
        .border_color(token::border())
        .whitespace_nowrap()
        .text_size(scale::px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(token::muted_foreground())
        .child(keys.into())
}
