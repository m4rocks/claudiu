//! An empty or not-ready state after shadcn's Empty: an icon on a tile, a title, a muted description, then any
//! actions the caller adds as children.

use gpui::{Div, FontWeight, ParentElement, SharedString, Styled, div, px};
use gpui_kit::assets::IconName;

use crate::panels::scale;
use crate::panels::{theme::theme, widgets::icon};

/// The icon and one line of muted text, for a panel with nothing in it.
pub fn empty_panel(glyph: IconName, label: impl Into<SharedString>) -> Div {
    div().flex().flex_col().items_center().justify_center().gap(px(10.0)).p(px(24.0))
        .child(media(glyph))
        .child(div().text_center().text_size(scale::px(13.0)).text_color(theme().muted_foreground).child(label.into()))
}

/// The icon, a title and a description, centered; add buttons as children.
pub fn empty_state(glyph: IconName, title: impl Into<SharedString>, description: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.0))
        .p(px(24.0))
        .max_w(px(480.0))
        .child(div().mb(px(6.0)).child(media(glyph)))
        .child(div().text_center().text_size(scale::px(14.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme().foreground).child(title.into()))
        .child(div().text_center().text_size(scale::px(12.5)).text_color(theme().muted_foreground).child(description.into()))
}

fn media(glyph: IconName) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .size(px(36.0))
        .rounded(px(8.0))
        .bg(theme().accent)
        .border_1()
        .border_color(theme().border)
        .child(icon(glyph, 17.0).text_color(theme().muted_foreground))
}
