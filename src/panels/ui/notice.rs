//! The compact bordered notice used throughout the conversation, including transient state and actions.
use gpui::{Div, ParentElement, SharedString, Styled, div, px};

use super::alert::Tone;
use crate::panels::{scale, theme::theme};

/// Add action buttons as children; the message wraps and keeps the same surface in every location.
pub fn notice(text: impl Into<SharedString>, tone: Tone) -> Div {
    content(div().child(text.into()), tone)
}

/// Shared surface for notices that need structured text, such as an optional title.
pub fn content(body: impl gpui::IntoElement, tone: Tone) -> Div {
    let t = theme();
    let (background, border, foreground) = match tone {
        Tone::Default => (t.background, t.panel_border, t.muted_foreground),
        Tone::Warning => (t.notice, t.notice_border, t.warning),
        Tone::Destructive => (t.destructive_subtle, t.destructive_subtle_border, t.destructive),
    };
    div().w_full().flex().items_center().flex_wrap().gap(px(8.0))
        .px(px(10.0)).py(px(6.0)).rounded(px(6.0))
        .bg(background).border_1().border_color(border)
        .text_size(scale::px(12.0)).text_color(foreground)
        .child(div().flex_1().min_w_0().child(body))
}
