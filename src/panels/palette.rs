//! Command Palette (`Ctrl+K` / `Cmd+K`) overlay rendering.

use gpui::{AnyElement, ClickEvent, Context, KeyDownEvent, MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled, InteractiveElement, div, px, prelude::*};
use gpui_kit::component::input::Input;

use crate::app::Workspace;
use crate::panels::scale;
use crate::panels::theme::{self, hsla};

impl Workspace {
    pub(super) fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = self.palette.as_ref()?;
        let mut list = div().flex().flex_col().gap(px(2.0)).p(px(6.0));
        if palette.filtered.is_empty() {
            list = list.child(
                div()
                    .px(px(10.0))
                    .py(px(14.0))
                    .text_size(scale::px(12.5))
                    .text_color(hsla(theme::TEXT_DIM))
                    .child("No matching commands, sessions, or files."),
            );
        } else {
            let total = palette.filtered.len();
            let visible = 12usize;
            let start = if total <= visible {
                0
            } else {
                palette.selected.saturating_sub(visible - 1).min(total - visible)
            };
            for (row_ix, &item_ix) in palette.filtered.iter().enumerate().skip(start).take(visible) {
                let item = &palette.items[item_ix];
                let active = row_ix == palette.selected;
                list = list.child(
                    div()
                        .id(SharedString::from(format!("palette-row-{row_ix}")))
                        .w_full()
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .cursor_pointer()
                        .when(active, |d| d.bg(hsla(theme::SURFACE_HOVER)))
                        .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
                        .on_mouse_move(cx.listener(move |ws, _, _, cx| ws.pick_palette(row_ix, cx)))
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                            ws.pick_palette(row_ix, cx);
                            ws.run_palette_selected(window, cx);
                        }))
                        .child(
                            div()
                                .flex_none()
                                .px(px(6.0))
                                .py(px(1.5))
                                .rounded(px(4.0))
                                .bg(hsla(theme::PANEL_BG))
                                .border_1()
                                .border_color(hsla(theme::PANEL_BORDER))
                                .text_size(scale::px(10.5))
                                .text_color(hsla(theme::TEXT_DIM))
                                .child(item.badge),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(scale::px(13.0))
                                .text_color(hsla(theme::TEXT))
                                .child(item.title.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .max_w(px(210.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(scale::px(11.5))
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(item.subtitle.clone()),
                        ),
                );
            }
        }

        let box_el = div()
            .id("command-palette-dialog")
            .w(px(580.0))
            .max_w(gpui::relative(0.9))
            .rounded(px(10.0))
            .bg(hsla(theme::SURFACE_ELEVATED))
            .border_1()
            .border_color(hsla(theme::FOCUS_BORDER))
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| match ev.keystroke.key.as_str() {
                "escape" => {
                    ws.close_palette(window, cx);
                    cx.stop_propagation();
                }
                "up" => {
                    ws.move_palette(-1, cx);
                    cx.stop_propagation();
                }
                "down" => {
                    ws.move_palette(1, cx);
                    cx.stop_propagation();
                }
                _ => {}
            }))
            .child(
                div()
                    .p(px(8.0))
                    .border_b_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .child(Input::new(&palette.input)),
            )
            .child(list);

        Some(
            div()
                .id("command-palette-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(84.0))
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .on_mouse_down(MouseButton::Left, cx.listener(|ws, _, window, cx| ws.close_palette(window, cx)))
                .child(box_el)
                .into_any_element(),
        )
    }
}
