//! Tooltips after shadcn's Tooltip, drawn by gpui-kit's tooltip on the popover colors `theme::install` gives it.

use gpui::{AnyView, App, SharedString, Window};
use gpui_kit::component::tooltip::Tooltip;

/// The builder `.tooltip(..)` takes, showing `text`.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}
