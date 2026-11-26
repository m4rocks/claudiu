//! Labels after shadcn's Label, in the small uppercase form m4code puts over sections and fields.

use gpui::{Div, FontWeight, ParentElement, SharedString, Styled, div};

use crate::panels::theme::theme;

/// The faint, semibold caption over a section or a field.
pub fn section_label(text: impl Into<SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme().subtle_foreground).child(text.into())
}
