use gpui::{Div, SharedString, ParentElement, Styled, div, px};
use gpui_kit::assets::IconName;
use crate::panels::{theme::theme, widgets::icon};

pub fn empty_panel(glyph: IconName, label: impl Into<SharedString>) -> Div {
    div().flex().flex_col().items_center().justify_center().gap(px(10.0)).p(px(24.0))
        .child(div().flex().items_center().justify_center().size(px(32.0)).rounded(px(8.0))
            .bg(theme().accent).child(icon(glyph, 16.0).text_color(theme().muted_foreground)))
        .child(div().text_center().text_size(crate::panels::scale::px(13.0)).text_color(theme().muted_foreground).child(label.into()))
}

