//! Tabs after shadcn's Tabs, in its two looks: [`list`] and [`trigger`] are the sunken track with the chosen tab
//! raised; [`line_list`] and [`line_trigger`] are the underlined row that sits on a panel's edge. A trigger is a
//! focusable `Button`; the caller attaches `.on_click`.

use gpui::{Div, FontWeight, InteractiveElement, ParentElement, SharedString, Styled, div, prelude::FluentBuilder, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Button;

use crate::panels::scale;
use crate::panels::theme::theme;
use crate::panels::ui::button::focusable;
use crate::panels::widgets::icon;

/// The sunken track the [`trigger`]s sit in.
pub fn list() -> Div {
    div().flex().flex_none().items_center().h(px(30.0)).p(px(2.0)).gap(px(2.0)).rounded(px(7.0)).bg(theme().inset).border_1().border_color(theme().border)
}

/// One tab in a [`list`], raised when `active`.
pub fn trigger(id: impl Into<SharedString>, glyph: Option<IconName>, label: impl Into<SharedString>, active: bool) -> Button {
    focusable(id.into())
        .flex()
        .items_center()
        .gap(px(6.0))
        .h_full()
        .px(px(10.0))
        .rounded(px(5.0))
        .text_size(scale::px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .focus_visible(|s| s.border_color(theme().ring))
        .when(active, |b| b.bg(theme().secondary_hover).border_color(theme().panel_border).text_color(theme().foreground).shadow_sm())
        .when(!active, |b| b.text_color(theme().muted_foreground).hover(|s| s.text_color(theme().foreground)))
        .children(glyph.map(|g| icon(g, 13.0)))
        .child(label.into())
}

/// The row the [`line_trigger`]s sit in, with the rule they stand on.
pub fn line_list() -> Div {
    div().flex().flex_none().items_end().gap(px(2.0)).px(px(8.0)).border_b_1().border_color(theme().border)
}

/// One tab in a [`line_list`], underlined when `active`.
pub fn line_trigger(id: impl Into<SharedString>, label: impl Into<SharedString>, active: bool) -> Button {
    Button::new(id.into())
        .flex()
        .items_center()
        .gap(px(6.0))
        .h(px(32.0))
        .px(px(10.0))
        .mb(px(-1.0))
        .border_b_2()
        .border_color(if active { theme().primary } else { gpui::transparent_black() })
        .text_size(scale::px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(if active { theme().foreground } else { theme().muted_foreground })
        .cursor_pointer()
        .hover(|s| s.text_color(theme().foreground))
        .focus_visible(|s| s.bg(theme().secondary_hover))
        .child(label.into())
}

/// A count after a tab's label.
pub fn count(n: impl ToString) -> Div {
    div()
        .flex_none()
        .min_w(px(18.0))
        .h(px(16.0))
        .px(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme().accent)
        .text_size(scale::px(10.5))
        .text_color(theme().muted_foreground)
        .child(n.to_string())
}
