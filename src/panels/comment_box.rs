//! The box a comment on a line is typed into: the same under a line of Review changes and beside a line in editor mode.
//! Enter and Cmd/Ctrl+Enter mean different things in each, so the caller handles `InputEvent::PressEnter`; the box only
//! says what the two do.

use gpui::{App, AppContext, Entity, InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, Window, div, px};
use gpui_kit::{assets::IconName, base::Button, component::input::{Input, InputState}};

use crate::panels::scale;
use crate::panels::theme::theme;
use crate::panels::ui::kbd::kbd;
use crate::panels::widgets::{fade_in, icon, shortcut};
use crate::panels::ui::button::focusable;

pub const COMMENT_ICON: IconName = IconName::MessageSquare;
pub const COMMENT_SIZE: f32 = 22.0;
pub const COMMENT_ICON_SIZE: f32 = 14.0;

pub const COMMENT_SHORTCUT: &str = "Shift+M";

pub fn comment_trigger(id: impl Into<gpui::SharedString>, shortcut_label: bool) -> Button {
    let hint = if shortcut_label { format!("Add comment ({})", shortcut(COMMENT_SHORTCUT)) } else { "Add comment".into() };
    focusable(id.into())
        .accessibility_label("Add comment")
        .tooltip(crate::panels::ui::tooltip::tooltip(hint))
        .size(px(COMMENT_SIZE))
        .rounded(px(4.0))
        .bg(theme().card)
        .hover(|s| s.bg(theme().secondary_hover))
        .cursor_pointer()
        .text_color(theme().foreground)
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
    let hint = |keys: String, what: String| div().flex().items_center().gap(px(4.0)).child(kbd(keys)).child(what);
    let panel = div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .p(px(8.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme().ring)
        .bg(theme().popover)
        .shadow_lg()
        .child(Input::new(input))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(10.0))
                .px(px(2.0))
                .text_size(scale::px(11.0))
                .text_color(theme().subtle_foreground)
                .child(hint("Enter".into(), enter.to_string()))
                .child(hint(shortcut("Enter"), send.to_string()))
                .child(hint("Esc".into(), "cancels".into())),
        );
    fade_in(panel, format!("comment-input-{:?}", input.entity_id()), 140, 0.8)
}
