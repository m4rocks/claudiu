//! A switch after shadcn's Switch: a pill track with a round thumb, light and to the right when on. Drawn only; the
//! row it sits in takes the click, so the whole row flips it.

use gpui::{InteractiveElement, Div, ParentElement, Styled, div, prelude::FluentBuilder, px};

use crate::panels::theme::theme;

pub fn switch(on: bool) -> Div {
    div()
        .flex_none()
        .w(px(32.0))
        .h(px(18.0))
        .p(px(2.0))
        .rounded_full()
        .bg(if on { theme().primary } else { theme().input })
        .flex()
        .items_center()
        .when(on, |d| d.justify_end())
        .child(div().size(px(14.0)).rounded_full().shadow_sm().bg(if on { theme().primary_foreground } else { theme().knob }))
}

/// Shared focusable row for a switch, with label and switch supplied as children.
pub fn row(id: impl Into<gpui::ElementId>) -> gpui_kit::base::Button {
    super::button::focusable(id)
        .w_full().px(px(14.0)).py(px(11.0)).flex().items_center().justify_between().gap(px(12.0))
        .text_size(crate::panels::scale::px(13.0)).cursor_pointer().hover(|s| s.bg(theme().accent))
}
