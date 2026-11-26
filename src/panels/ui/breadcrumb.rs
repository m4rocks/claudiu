//! A path after shadcn's Breadcrumb: muted segments with chevrons between them, the last one in the foreground.

use gpui::{Div, ParentElement, SharedString, Styled, div, px};
use gpui_kit::assets::IconName;

use crate::panels::scale;
use crate::panels::{theme::theme, widgets::icon};

/// `segments` in order, outermost first.
pub fn breadcrumb(segments: impl IntoIterator<Item = impl Into<SharedString>>) -> Div {
    let segments: Vec<SharedString> = segments.into_iter().map(Into::into).collect();
    let last = segments.len().saturating_sub(1);
    let mut row = div().flex().items_center().gap(px(3.0)).min_w_0().overflow_hidden().whitespace_nowrap().text_size(scale::px(11.5));
    for (i, s) in segments.into_iter().enumerate() {
        if i > 0 {
            row = row.child(icon(IconName::ChevronRight, 11.0).text_color(theme().subtle_foreground));
        }
        row = row.child(div().flex_none().text_color(if i == last { theme().foreground } else { theme().muted_foreground }).child(s));
    }
    row
}
