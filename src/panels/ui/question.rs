//! Question options and submission controls shared by chat and sidebar previews.
use gpui::{FontWeight, InteractiveElement, ParentElement, SharedString, Styled, div, prelude::FluentBuilder, px};
use gpui_kit::{assets::IconName, base::Button};
use crate::panels::{scale, theme::theme};

pub fn option(id: impl Into<SharedString>, title: String, description: Option<String>, multi: bool, checked: bool, highlighted: bool, number: Option<usize>) -> Button {
    super::button::focusable(id.into()).w_full().px(px(10.0)).py(px(8.0)).rounded(px(6.0))
        .bg(if checked || highlighted { theme().row_active } else { theme().background })
        .border_color(if checked || highlighted { theme().ring } else { theme().panel_border })
        .hover(|s| s.bg(theme().accent)).cursor_pointer().flex().items_start().gap(px(8.0))
        .accessibility_label(title.clone())
        .when(multi, |row| row.child(super::checkbox::checkbox(checked).mt(px(2.0))))
        .children(number.map(|n| div().pt(px(1.0)).child(super::kbd::kbd(n.to_string()))))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.0))
            .child(div().text_size(scale::px(12.5)).font_weight(FontWeight::MEDIUM).text_color(theme().foreground).child(title))
            .children(description.map(|text| div().text_size(scale::px(11.5)).text_color(theme().muted_foreground).child(text))))
}

pub fn submit(id: impl Into<SharedString>, count: usize) -> Button {
    let label = if count == 0 { "Submit".into() } else { format!("Submit ({count})") };
    super::button::leading(id, IconName::Check, label, true).disabled(count == 0)
}
