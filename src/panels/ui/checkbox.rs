//! A checkbox after shadcn's Checkbox: a small rounded square, filled and checked when on. Drawn only; the row it
//! sits in takes the click.

use gpui::{Div, ParentElement, Styled, div, prelude::FluentBuilder, px};
use gpui_kit::assets::IconName;

use crate::panels::theme::theme;
use crate::panels::widgets::icon;

pub fn checkbox(checked: bool) -> Div {
    div()
        .flex_none()
        .size(px(14.0))
        .rounded(px(4.0))
        .border_1()
        .border_color(if checked { theme().primary } else { theme().ring })
        .bg(if checked { theme().primary } else { theme().background })
        .flex()
        .items_center()
        .justify_center()
        .when(checked, |d| d.child(icon(IconName::Check, 10.0).text_color(theme().primary_foreground)))
}
