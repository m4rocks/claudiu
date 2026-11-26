//! Buttons after shadcn's Button: six variants and four sizes, a focus-visible ring, and a dimmed disabled state.
//! The result is gpui-base's `Button`, which owns Tab focus and Enter/Space activation; attach `.on_click`.

use gpui::{FontWeight, prelude::FluentBuilder, InteractiveElement, ParentElement, SharedString, Styled, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Button;

use super::token;
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
    /// Text that underlines under the pointer.
    Link,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Size {
    /// 22px, for toolbars and dense rows.
    Xs,
    /// 26px, the app's usual button.
    #[default]
    Sm,
    /// 32px.
    Default,
    /// 22px square, for an icon alone.
    Icon,
}

/// A button showing `label`, with `glyph` before it when given.
pub fn button(id: impl Into<SharedString>, glyph: Option<IconName>, label: impl Into<SharedString>, variant: Variant, size: Size) -> Button {
    let label = label.into();
    let (height, pad, text, glyph_size) = match size {
        Size::Xs => (22.0, 8.0, 11.5, 12.0),
        Size::Sm => (26.0, 10.0, 12.0, 13.0),
        Size::Default => (32.0, 14.0, 12.5, 14.0),
        Size::Icon => (22.0, 0.0, 12.0, 14.0),
    };
    let base = Button::new(id.into())
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
        .focus_visible(|s| s.border_color(token::ring()))
        .styles(|s| s.disabled(|s| s.opacity(0.5).cursor_default()));
    let styled = match variant {
        Variant::Default => base.bg(token::primary()).text_color(token::primary_foreground()).hover(|s| s.opacity(0.9)),
        Variant::Secondary => base.bg(token::secondary()).text_color(token::foreground()).hover(|s| s.bg(token::secondary_hover())),
        Variant::Outline => base.border_color(token::border()).text_color(token::foreground()).hover(|s| s.bg(token::accent())),
        Variant::Ghost => base.text_color(token::foreground()).hover(|s| s.bg(token::accent())),
        Variant::Destructive => base.bg(token::destructive().opacity(0.6)).text_color(token::foreground()).hover(|s| s.bg(token::destructive().opacity(0.75))),
        Variant::Link => base.text_color(token::foreground()).hover(|s| s.underline()),
    };
    styled.children(glyph.map(|g| icon(g, glyph_size))).when(!label.is_empty(), |b| b.child(label))
}
