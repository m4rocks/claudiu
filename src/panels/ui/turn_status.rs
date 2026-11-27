//! Quiet, unboxed turn progress and duration rows beneath the conversation.
use gpui::{AnyElement, Div, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div, px, relative};

use crate::panels::{scale, theme::theme};

fn row() -> Div {
    div().px(px(2.0)).py(px(3.0)).text_size(scale::px(12.0)).line_height(relative(1.4))
}

pub fn working(label: impl Into<SharedString>, elapsed: Option<String>, stop: Option<AnyElement>) -> AnyElement {
    row().flex().items_center().gap(px(7.0))
        .child(div().text_color(theme().muted_foreground).child(label.into()))
        .children(elapsed.map(|duration| div().text_color(theme().subtle_foreground).child(duration)))
        .children(stop.map(|stop| div().ml(px(4.0)).child(stop)))
        .debug_selector(|| "turn-working".into())
        .into_any_element()
}

pub fn done(duration: impl Into<SharedString>) -> AnyElement {
    row().text_color(theme().subtle_foreground)
        .child(format!("Worked for {}", duration.into()))
        .debug_selector(|| "turn-done".into())
        .into_any_element()
}
