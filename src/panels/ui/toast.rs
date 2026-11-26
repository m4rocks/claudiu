//! Transient notifications with a message, an optional action and a dismiss control.
use gpui::{AnyElement, Div, ParentElement, SharedString, Styled, div, px};
use crate::panels::{scale, theme::theme};

pub fn toast(message: impl Into<SharedString>, action: Option<AnyElement>, dismiss: impl gpui::IntoElement) -> Div {
    super::card::card().w(px(360.0)).max_w(px(420.0)).shadow_lg()
        .child(div().flex().items_start().gap(px(12.0)).p(px(12.0))
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(8.0))
                .text_size(scale::px(12.5)).text_color(theme().foreground)
                .child(message.into()).children(action))
            .child(dismiss))
}
