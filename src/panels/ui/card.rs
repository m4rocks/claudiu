//! A bordered surface after shadcn's Card: a rounded box with an optional header row, a body and a footer of
//! actions. The pieces are plain `Div`s, so a caller adds its own children and handlers.

use gpui::{Div, Hsla, Styled, div, px};

use crate::panels::theme::theme;

/// The box: 8px radius, a 1px border, the card fill, clipped so a header's fill keeps to the corners.
pub fn card() -> Div {
    outlined(theme().border)
}

/// [`card`] with its border in `color`, for a card whose state matters (a review, the merge box).
pub fn outlined(color: Hsla) -> Div {
    div().flex().flex_col().rounded(px(8.0)).border_1().border_color(color).bg(theme().card).overflow_hidden()
}

/// The top row: 36px high, its parts 8px apart, a rule under it.
pub fn header() -> Div {
    div().flex().flex_none().items_center().gap(px(8.0)).min_h(px(36.0)).px(px(12.0)).py(px(6.0)).bg(theme().card_header).border_b_1().border_color(theme().border)
}

/// The body, padded 12px.
pub fn content() -> Div {
    div().flex().flex_col().gap(px(10.0)).p(px(12.0))
}

/// A row of actions along the bottom, right-aligned, a rule above it.
pub fn footer() -> Div {
    div().flex().flex_wrap().items_center().justify_end().gap(px(6.0)).px(px(12.0)).py(px(10.0)).border_t_1().border_color(theme().border)
}
