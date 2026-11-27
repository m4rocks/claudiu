//! Transient notices with an optional action and a dismiss control.
use gpui::{AnyElement, Div, ParentElement, SharedString, Styled, px};

pub fn toast(message: impl Into<SharedString>, action: Option<AnyElement>, dismiss: impl gpui::IntoElement) -> Div {
    super::notice::notice(message, super::alert::Tone::Default)
        .w(px(360.0)).max_w(px(420.0)).shadow_lg()
        .children(action).child(dismiss)
}
