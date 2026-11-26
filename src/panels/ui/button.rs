//! Buttons after shadcn's Button: five variants and three sizes, a focus-visible ring, and a dimmed disabled state.
//! The result is gpui-base's `Button`, which owns Tab focus and Enter/Space activation; attach `.on_click`.

use gpui::{FontWeight, prelude::FluentBuilder, InteractiveElement, ParentElement, SharedString, Styled, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Button;

use crate::panels::theme::theme;
use crate::panels::{scale, widgets::icon};

/// How loud a button is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variant {
    /// The one thing to do: light fill.
    #[default]
    Default,
    /// A plain choice: dark fill.
    Secondary,
    /// A bordered button with no fill.
    Outline,
    /// No fill or border until the pointer is on it.
    Ghost,
    /// Something that destroys work.
    Destructive,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Size {
    /// 22px, for toolbars and dense rows.
    Xs,
    /// 26px, the app's usual button.
    #[default]
    Sm,
    /// 22px square, for an icon alone.
    Icon,
}

/// A button showing `label`, with `glyph` before it when given.
pub fn button(id: impl Into<SharedString>, glyph: Option<IconName>, label: impl Into<SharedString>, variant: Variant, size: Size) -> Button {
    let label = label.into();
    let glyph_size = if size == Size::Icon { 14.0 } else if size == Size::Xs { 12.0 } else { 13.0 };
    let base = foundation(id, size);
    let styled = match variant {
        Variant::Default => base.bg(theme().primary).text_color(theme().primary_foreground).hover(|s| s.opacity(0.9)),
        Variant::Secondary => base.bg(theme().secondary).text_color(theme().foreground).hover(|s| s.bg(theme().secondary_hover)),
        Variant::Outline => base.border_color(theme().panel_border).text_color(theme().foreground).hover(|s| s.bg(theme().secondary_hover)),
        Variant::Ghost => base.text_color(theme().foreground).hover(|s| s.bg(theme().secondary_hover)),
        Variant::Destructive => base.bg(theme().destructive.opacity(0.6)).text_color(theme().foreground).hover(|s| s.bg(theme().destructive.opacity(0.75))),
    };
    styled.children(glyph.map(|g| icon(g, glyph_size))).when(!label.is_empty(), |b| b.child(label))
}

/// Common geometry, disabled state and keyboard focus for every button variant.
fn foundation(id: impl Into<SharedString>, size: Size) -> Button {
    let (height, pad, text) = match size {
        Size::Xs => (22.0, 8.0, 11.5),
        Size::Sm => (26.0, 10.0, 12.0),
        Size::Icon => (22.0, 0.0, 12.0),
    };
    Button::new(id.into())
        .h(px(height))
        .when(size == Size::Icon, |b| b.w(px(height)))
        .px(px(pad))
        .gap(px(5.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(gpui::transparent_black())
        .text_size(scale::px(text))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .focus_visible(|s| s.border_color(theme().ring))
        .styles(|s| s.disabled(|s| s.opacity(0.5).cursor_default()))
}

/// The bare focusable button under every kit button: Tab reaches it, Enter or Space presses it, and the keyboard's
/// focus shows as a ring.
pub fn focusable(id: impl Into<gpui::ElementId>) -> Button {
    Button::new(id).border_1().border_color(gpui::transparent_black()).focus_visible(|s| s.border_color(theme().primary))
}

/// The app's usual text button: [`Variant::Default`] when `primary`, else [`Variant::Secondary`].
pub fn text(id: impl Into<SharedString>, label: impl Into<SharedString>, primary: bool) -> Button {
    button(id, None, label, if primary { Variant::Default } else { Variant::Secondary }, Size::Sm)
}

/// [`text`] with `glyph` before the label.
pub fn leading(id: impl Into<SharedString>, glyph: IconName, label: impl Into<SharedString>, primary: bool) -> Button {
    button(id, Some(glyph), label, if primary { Variant::Default } else { Variant::Secondary }, Size::Sm)
}

/// A button that can't be pressed right now: dimmed, with no hover; attach no click handler.
pub fn disabled(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Button {
    disabled_leading(id, None, label)
}

/// [`disabled`] with `glyph` before the label.
pub fn disabled_leading(id: impl Into<SharedString>, glyph: Option<IconName>, label: impl Into<SharedString>) -> Button {
    button(id, glyph, label, Variant::Secondary, Size::Sm).disabled(true)
}

/// Something that destroys work (end a session, quit).
pub fn destructive(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Button {
    button(id, None, label, Variant::Destructive, Size::Sm)
}

/// An icon alone on a ghost button, muted until the pointer is on it.
pub fn icon_button(id: impl Into<SharedString>, glyph: IconName) -> Button {
    focusable(id.into())
        .size(px(22.0))
        .rounded(px(5.0))
        .text_color(theme().muted_foreground)
        .cursor_pointer()
        .hover(|s| s.bg(theme().secondary_hover).text_color(theme().foreground))
        .child(icon(glyph, 14.0))
}

/// [`icon_button`] dimmed, with no hover; attach no click handler.
pub fn icon_button_disabled(id: impl Into<SharedString>, glyph: IconName) -> Button {
    Button::new(id.into())
        .disabled(true)
        .size(px(22.0))
        .rounded(px(5.0))
        .text_color(theme().subtle_foreground)
        .cursor_default()
        .child(icon(glyph, 14.0))
}

/// A button in colors of its own, for the few that carry a state's color (Send, Stop, Jump to latest).
#[allow(clippy::too_many_arguments)]
pub fn colored(
    id: impl Into<SharedString>,
    glyph: Option<IconName>,
    label: impl Into<SharedString>,
    bg: gpui::Hsla,
    fg: gpui::Hsla,
    hover: gpui::Hsla,
    border: Option<gpui::Hsla>,
    bold: bool,
) -> Button {
    let label = label.into();
    foundation(id, Size::Sm)
        .bg(bg)
        .border_color(border.unwrap_or_else(gpui::transparent_black))
        .text_color(fg)
        .text_size(scale::px(12.0))
        .font_weight(if bold { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .children(glyph.map(|g| icon(g, 13.0)))
        .when(!label.is_empty(), |b| b.child(label))
}
