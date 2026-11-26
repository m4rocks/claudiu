//! Shared item and setting layouts for the sidebar and inspector.
use gpui::{Div, ElementId, InteractiveElement, IntoElement, ParentElement, Stateful, Styled, div, px, prelude::FluentBuilder};
use crate::panels::{scale, theme::theme};

/// A session or inspector item. Callers add content and retain their drag and context-menu handlers.
pub fn item(id: impl Into<ElementId>, selected: bool) -> Stateful<Div> {
    div().id(id.into()).flex().items_center().gap(px(8.0)).rounded(px(6.0))
        .text_size(scale::px(12.5)).text_color(theme().foreground).cursor_pointer()
        .when(selected, |d| d.bg(theme().row_active))
        .hover(|s| s.bg(theme().row_hover))
}

/// Label and value align identically in static settings and their menu triggers.
pub fn setting(label: impl Into<String>, value: impl IntoElement) -> Div {
    div().flex().items_center().justify_between().gap(px(10.0)).min_h(px(24.0)).w_full()
        .text_size(scale::px(12.0))
        .child(div().flex_none().text_color(theme().muted_foreground).child(label.into()))
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(value))
}

/// Status at the leading edge, a title with optional metadata and footer, and a trailing value or action.
pub fn content(lead: impl IntoElement, title: impl IntoElement, metadata: Option<gpui::AnyElement>, footer: Option<String>, trailing: impl IntoElement) -> Div {
    div().flex_1().min_w_0().flex().items_center().gap(px(8.0))
        .child(div().flex_none().child(lead))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.0))
            .children(metadata)
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis()
                .text_size(scale::px(13.0)).line_height(scale::px(17.0)).child(title))
            .children(footer.map(|text| div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis()
                .text_size(scale::px(11.0)).line_height(scale::px(14.0)).text_color(theme().subtle_foreground).child(text))))
        .child(div().flex_none().max_w(px(76.0)).overflow_hidden().whitespace_nowrap().text_ellipsis()
            .text_size(scale::px(11.0)).text_color(theme().subtle_foreground).child(trailing))
}
