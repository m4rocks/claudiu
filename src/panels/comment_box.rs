//! The box a comment on a line is typed into: the same under a line of Review changes and beside a line in editor mode.
//! Enter and Cmd/Ctrl+Enter mean different things in each, so the caller handles `InputEvent::PressEnter`; the box only
//! says what the two do.

use gpui::{App, AppContext, Entity, InteractiveElement, ParentElement, Styled, Window, div, px};
use gpui_kit::{assets::IconName, base::Button, component::input::{Input, InputState}};

use crate::panels::scale;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{fade_in, focusable, icon, shortcut};

pub const COMMENT_ICON: IconName = IconName::MessageSquare;
pub const COMMENT_SIZE: f32 = 22.0;
pub const COMMENT_ICON_SIZE: f32 = 14.0;

pub fn comment_trigger(id: impl Into<gpui::SharedString>) -> Button {
    focusable(id.into())
        .accessibility_label("Add comment")
        .size(px(COMMENT_SIZE))
        .rounded(px(4.0))
        .bg(hsla(theme::SURFACE_ELEVATED))
        .hover(|s| s.bg(hsla(theme::CONTROL_HOVER)))
        .cursor_pointer()
        .text_color(hsla(theme::TEXT))
        .child(icon(COMMENT_ICON, COMMENT_ICON_SIZE))
}

/// A new, focused input for a comment on what `on` names, such as `line 12`.
pub fn comment_input(on: &str, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(format!("Comment on {on}")));
    input.update(cx, |i, cx| i.focus(window, cx));
    input
}

/// The input, with a line under it saying what Enter (`enter`) and Cmd/Ctrl+Enter (`send`) do.
pub fn comment_box(input: &Entity<InputState>, enter: &str, send: &str) -> impl gpui::IntoElement {
    let panel = div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .p(px(6.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(hsla(theme::FOCUS_BORDER))
        .bg(hsla(theme::PANEL_BG))
        .child(Input::new(input))
        .child(div().px(px(2.0)).text_size(scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(format!("Enter {enter} · {} {send} · Esc cancels", shortcut("Enter"))));
    fade_in(panel, format!("comment-input-{:?}", input.entity_id()), 140, 0.8)
}
