//! A segmented control for a two- or three-way choice, after shadcn's Tabs list and ToggleGroup: one bordered,
//! sunken track holding the options, the chosen one raised. Built on gpui-base's `ToggleGroup` and `Toggle`, so each
//! segment is a pressed/unpressed toggle that Tab reaches and Enter or Space flips.

use gpui::{Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, Window, px};
use gpui_kit::base::{Toggle, ToggleGroup};

use super::token;
use crate::panels::scale;

/// The control for `options`, with `current` raised. Choosing a segment runs `on_pick` with its value; choosing the
/// raised one again does nothing.
pub fn toggle_group<V: 'static, T: Copy + PartialEq + 'static>(
    id: impl Into<SharedString>,
    options: &[(T, &'static str)],
    current: T,
    on_pick: impl Fn(&mut V, T, &mut Window, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let id = id.into();
    let on_pick = std::rc::Rc::new(on_pick);
    let segments = options.iter().map(|&(value, label)| {
        let on = value == current;
        let pick = on_pick.clone();
        let view = cx.entity().downgrade();
        Toggle::new(SharedString::from(format!("{id}-{label}")))
            .pressed(on)
            .accessibility_label(label)
            .h_full()
            .px(px(10.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(gpui::transparent_black())
            .text_size(scale::px(12.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(token::muted_foreground())
            .cursor_pointer()
            .hover(|s| s.text_color(token::foreground()))
            .focus_visible(|s| s.border_color(token::ring()))
            .styles(|s| s.pressed(|s| s.bg(token::raised()).border_color(token::border()).text_color(token::foreground()).shadow_sm()))
            .child(label)
            .on_change(move |_, _, window, cx| {
                if !on {
                    let pick = pick.clone();
                    let _ = view.update(cx, |v, cx| {
                        pick(v, value, window, cx);
                        cx.notify();
                    });
                }
            })
    });
    ToggleGroup::new(id.clone())
        .flex_none()
        .flex()
        .items_center()
        .h(px(28.0))
        .p(px(2.0))
        .gap(px(2.0))
        .rounded(px(6.0))
        .bg(token::muted())
        .border_1()
        .border_color(token::border())
        .children(segments)
}
