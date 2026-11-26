//! The floating surfaces after shadcn's Popover and Dialog: a bordered, shadowed card that keeps the pointer from
//! what is under it.

use gpui::{Div, ElementId, InteractiveElement, Stateful, Styled, div, px};

use crate::panels::theme::theme;

/// A popover: a picker, a dropdown, the editor's quick open.
pub fn content(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id).occlude().cursor_default().rounded(px(10.0)).bg(theme().popover).border_1().border_color(theme().panel_border).shadow_lg()
}

/// A dialog's card, over the backdrop.
pub fn dialog(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id).occlude().rounded(px(12.0)).bg(theme().background).border_1().border_color(theme().panel_border).shadow_lg()
}
