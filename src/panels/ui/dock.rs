//! The strip above the message box. Anything there is one of three parts: a [`notice`](super::notice) for a message or
//! a decision, a [`chip`] that folds one kind of live state (the plan, background work, the queue) into a pill, and the
//! one [`tray`] of [`row`]s a chip opens. They share the notice's surface and radius, so the strip reads as one thing.

use gpui::{
    Div, InteractiveElement, IntoElement, ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled, div, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::Button;

use super::button::icon_button;
use crate::panels::{scale, theme::theme};

/// The corner radius of every surface above the message box, the notice's.
pub const RADIUS: f32 = 6.0;
/// The space between the parts of the strip.
pub const GAP: f32 = 6.0;

/// A pill that stands for one kind of live state: `lead` (an icon or spinner), then a short `label`. Lit while its
/// tray is `open`; add the click handler.
pub fn chip(id: impl Into<SharedString>, lead: impl IntoElement, label: impl Into<SharedString>, open: bool) -> Stateful<Div> {
    let t = theme();
    let (hover_bg, hover_fg) = (t.row_hover, t.foreground);
    div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.0))
        .h(px(22.0))
        .px(px(8.0))
        .rounded_full()
        .border_1()
        .border_color(if open { t.ring } else { t.panel_border })
        .bg(if open { t.accent } else { t.background })
        .text_size(scale::px(11.5))
        .text_color(if open { t.foreground } else { t.muted_foreground })
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
        .child(div().flex_none().flex().items_center().child(lead))
        .child(div().max_w(px(280.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(label.into()))
}

/// The panel a chip opens: rows on the notice's surface.
pub fn tray() -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(1.0))
        .p(px(4.0))
        .rounded(px(RADIUS))
        .bg(theme().background)
        .border_1()
        .border_color(theme().panel_border)
        .text_size(scale::px(12.5))
}

/// A small heading inside a tray, over the rows it names.
pub fn caption(text: impl Into<SharedString>) -> Div {
    div().px(px(8.0)).pt(px(3.0)).pb(px(1.0)).text_size(scale::px(11.0)).text_color(theme().subtle_foreground).child(text.into())
}

/// One entry of a tray: `lead`, the `label` cut to one line, then dim `meta`. Add [`actions`] as a child; they show
/// while the row is hovered.
pub fn row(id: impl Into<SharedString>, lead: impl IntoElement, label: impl IntoElement, meta: Option<SharedString>) -> Stateful<Div> {
    let id = id.into();
    div()
        .id(id.clone())
        .group(id)
        .flex()
        .items_center()
        .gap(px(8.0))
        .h(px(26.0))
        .px(px(8.0))
        .rounded(px(RADIUS - 1.0))
        .hover(|s| s.bg(theme().row_hover))
        .child(div().flex_none().w(px(14.0)).flex().justify_center().text_color(theme().muted_foreground).child(lead))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme().foreground).child(label))
        .children(meta.map(|meta| div().flex_none().text_size(scale::px(11.0)).text_color(theme().subtle_foreground).child(meta)))
}

/// The buttons of the row `row_id`, hidden until the row is hovered unless `shown`.
pub fn actions(row_id: impl Into<SharedString>, shown: bool) -> Div {
    let d = div().flex_none().flex().items_center().gap(px(2.0));
    if shown { d } else { d.invisible().group_hover(row_id.into(), |s| s.visible()) }
}

/// An icon button of a row, named by its tooltip.
pub fn action(id: impl Into<SharedString>, glyph: IconName, label: impl Into<SharedString>) -> Button {
    let label = label.into();
    icon_button(id, glyph).accessibility_label(label.clone()).tooltip(super::tooltip::tooltip(label))
}
