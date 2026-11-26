//! Placeholders after shadcn's Skeleton: muted bars where content will be, pulsing until it arrives.

use std::time::Duration;

use gpui::{Animation, AnimationElement, AnimationExt, Div, ElementId, ParentElement, Styled, div, px};

use crate::panels::theme::theme;

/// One bar, `w` wide (a fraction of its parent) and `h` high.
pub fn bar(w: f32, h: f32) -> Div {
    div().h(px(h)).w(gpui::relative(w)).rounded(px(4.0)).bg(theme().muted)
}

/// `count` two-line rows, like a list loading.
pub fn skeleton_rows(count: usize) -> Div {
    div().flex().flex_col().gap(px(14.0)).p(px(12.0)).children((0..count).map(|row| {
        div().flex().flex_col().gap(px(6.0))
            .child(div().h(px(10.0)).w(px(112.0 + (row % 3) as f32 * 22.0)).rounded(px(4.0)).bg(theme().muted))
            .child(div().h(px(8.0)).w(px(76.0)).rounded(px(4.0)).bg(theme().muted))
    }))
}

/// `el` fading between dim and full, over and over.
pub fn pulse<E: Styled + gpui::IntoElement + 'static>(el: E, id: impl Into<ElementId>) -> AnimationElement<E> {
    el.with_animation(id, Animation::new(Duration::from_millis(1200)).repeat().with_easing(gpui::pulsating_between(0.45, 1.0)), |d, t| d.opacity(t))
}
