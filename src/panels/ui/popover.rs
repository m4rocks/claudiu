//! The floating surfaces after shadcn's Popover and Dialog: a bordered, shadowed card that keeps the pointer from
//! what is under it. Each surface declares whether it supports Up/Down arrow navigation via [`ArrowNav`].

use gpui::{Div, ElementId, InteractiveElement, Stateful, Styled, div, px};

use crate::panels::theme::theme;

/// Whether a dialog or popover supports Up/Down arrow navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowNav {
    /// Arrow keys move through the dialog's items.
    Enabled,
    /// Pointer-only or non-list dialog: arrow keys do not navigate it.
    Disabled,
}

impl ArrowNav {
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// A popover: a picker, a dropdown, the editor's quick open, or a hover card.
pub fn content(id: impl Into<ElementId>, _arrow_nav: ArrowNav) -> Stateful<Div> {
    div().id(id).occlude().cursor_default().rounded(px(10.0)).bg(theme().popover).border_1().border_color(theme().panel_border).shadow_lg()
}

/// A dialog's card, over the backdrop.
pub fn dialog(id: impl Into<ElementId>, _arrow_nav: ArrowNav) -> Stateful<Div> {
    div().id(id).occlude().rounded(px(12.0)).bg(theme().background).border_1().border_color(theme().panel_border).shadow_lg()
}
