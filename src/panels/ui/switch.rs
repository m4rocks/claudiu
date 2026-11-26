//! A switch after shadcn's Switch: a pill track with a round thumb, light and to the right when on. Drawn only; the
//! row it sits in takes the click, so the whole row flips it.

use gpui::{Div, ParentElement, Styled, div, prelude::FluentBuilder, px};

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
