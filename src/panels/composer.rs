//! The message box, as the Welcome screen and the chat both draw it: where you type on top, a row of chips (model,
//! mode, project, branch) and actions under it.

use gpui::{AnyElement, Div, ElementId, InteractiveElement, IntoElement, ParentElement, SharedString, Stateful, Styled, div, prelude::FluentBuilder, px};
use gpui_kit::assets::IconName;

use crate::panels::scale;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::icon;

/// The box: `top` (a question waiting for an answer) above `input`, then `chips` on the left of the footer and
/// `actions` on its right. `lit` draws the accent border, for focus or a waiting question.
pub fn frame(lit: bool, top: Option<AnyElement>, input: AnyElement, chips: Vec<AnyElement>, actions: Vec<AnyElement>) -> Div {
    div()
        .w_full()
        .rounded(px(14.0))
        .bg(hsla(theme::SURFACE_ELEVATED))
        .border_1()
        .border_color(hsla(if lit { theme::ACCENT } else { theme::PANEL_BORDER }))
        .overflow_hidden()
        .flex()
        .flex_col()
        .children(top)
        .child(input)
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .border_t_1()
                .border_color(hsla(theme::BORDER))
                .child(div().flex().flex_wrap().items_center().gap(px(6.0)).children(chips))
                .child(div().flex().flex_wrap().items_center().gap(px(6.0)).children(actions)),
        )
}

/// Padding around the text area inside `frame`.
pub fn input_area() -> Div {
    div().px(px(14.0)).pt(px(12.0)).pb(px(8.0))
}

/// One chip in the footer; `open` while its dropdown is up.
pub fn chip(id: impl Into<ElementId>, open: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(26.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .bg(hsla(if open { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
        .border_1()
        .border_color(hsla(if open { theme::ACCENT } else { theme::PANEL_BORDER }))
        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
        .flex()
        .items_center()
        .gap(px(5.0))
        .cursor_pointer()
        .text_size(scale::px(11.5))
}

/// What the branch chip shows: the branch (or worktree) and, in a linked worktree, its name; a spinner `tail` while
/// git works.
pub fn git_chip(id: impl Into<ElementId>, open: bool, primary: impl Into<SharedString>, linked: Option<String>, tail: Option<AnyElement>) -> Stateful<Div> {
    chip(id, open)
        .text_color(hsla(theme::TEXT))
        .child(icon(IconName::GitBranch, 12.0).text_color(hsla(theme::TEXT_DIM)))
        .child(div().max_w(px(150.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(primary.into()))
        .when_some(linked, |d, wt| {
            d.child(div().text_color(hsla(theme::TEXT_FAINT)).child("·"))
                .child(icon(IconName::FolderGit2, 11.0).text_color(hsla(theme::TEXT_FAINT)))
                .child(div().max_w(px(100.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(hsla(theme::TEXT_DIM)).child(wt))
        })
        .child(tail.unwrap_or_else(|| icon(IconName::ChevronDown, 10.5).text_color(hsla(theme::TEXT_FAINT)).into_any_element()))
}
