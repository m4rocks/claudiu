//! Overlays drawn over the workspace: the context menu, the active modal and the toast.
//! The Settings modal's content is in `settings_view`, and the anchored dropdowns are in `workspace_pickers`.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, StatefulInteractiveElement, Styled, Window, anchored, deferred, div,
    px, rgba,
};

use crate::app::{
    Act, Modal, Workspace,
};
use gpui_kit::component::input::Input;
use crate::panels::theme::{self, hsla};
use gpui_kit::assets::IconName;
use crate::panels::widgets::{
    button, danger_button, fade_in, icon, icon_button,
};
use crate::panels::settings_view::render_settings_card;

impl Workspace {
    pub(super) fn render_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let pos = menu.pos;
        let mut card = div()
            .id("menu")
            .occlude()
            .cursor_default()
            .w(px(232.0))
            .min_w(menu.min_w)
            .p(px(4.0))
            .rounded(px(8.0))
            .bg(hsla(0x181818))
            .border_1()
            .border_color(hsla(0x2c2c2c))
            .shadow_lg()
            .flex()
            .flex_col();
        for (i, item) in menu.items.iter().enumerate() {
            let act = item.act.clone();
            if item.separator_before {
                card = card.child(div().h(px(1.0)).my(px(4.0)).bg(hsla(theme::BORDER)));
            }
            let color = if item.disabled { theme::TEXT_FAINT } else if item.danger { theme::DANGER } else { theme::TEXT };
            let row = div()
                .id(gpui::SharedString::from(format!("menu-{i}")))
                .h(px(if item.detail.is_some() { 42.0 } else { 28.0 }))
                .px(px(10.0))
                .flex()
                .items_center()
                .rounded(px(5.0))
                .text_size(crate::panels::scale::px(12.5))
                .text_color(hsla(color))
                .gap(px(8.0))
                .group(gpui::SharedString::from(format!("menu-row-{i}")))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(item.label.clone()))
                        .children(item.detail.clone().map(|d| div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(d))),
                )
                .children(item.icon.map(|name| icon(name, 12.0).text_color(hsla(theme::TEXT_FAINT))))
                .children(item.hint.clone().map(|h| div().flex_none().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(h)))
                .children(item.remove.clone().map(|remove| {
                    icon_button(gpui::SharedString::from(format!("menu-remove-{i}")), IconName::X)
                        .invisible()
                        .group_hover(gpui::SharedString::from(format!("menu-row-{i}")), |s| s.visible())
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            ws.act(remove.clone(), window, cx);
                        }))
                }));
            card = card.child(if item.disabled {
                row
            } else {
                row.cursor_pointer()
                    .hover(|s| s.bg(hsla(0x252525)))
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(act.clone(), window, cx)))
            });
        }
        let close = |ws: &mut Workspace, _: &MouseDownEvent, _w: &mut Window, cx: &mut Context<Workspace>| {
            ws.menu = None;
            cx.notify();
        };
        Some(
            deferred(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .on_mouse_down(MouseButton::Left, cx.listener(close))
                    .on_mouse_down(MouseButton::Right, cx.listener(close))
                    .child(anchored().position(pos).snap_to_window_with_margin(px(8.0)).child(fade_in(card, "menu-in", 110, 0.0))),
            )
            .with_priority(25)
            .into_any_element(),
        )
    }

    /// Active modal overlay (Settings uses its own split-pane layout; other modals share the compact 440px card).
    pub(super) fn render_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.modal.as_ref()?;
        let viewport = window.viewport_size();
        let card = if matches!(modal, Modal::Settings { .. }) {
            render_settings_card(self, viewport, window, cx)
        } else {
            let (title, body, confirm_label, danger): (String, gpui::AnyElement, String, bool) = match modal {
                Modal::Confirm { title, body, confirm, danger, .. } => (
                    title.clone(),
                    div().text_size(crate::panels::scale::px(13.0)).text_color(hsla(theme::TEXT_DIM)).child(body.clone()).into_any_element(),
                    confirm.clone(),
                    *danger,
                ),
                Modal::Settings { .. } => unreachable!(),
                Modal::FileName { purpose, input, .. } => (
                    match purpose {
                        crate::app::NameFor::FileIn(_) => "New file",
                        crate::app::NameFor::FolderIn(_) => "New folder",
                        crate::app::NameFor::Rename(_) => "Rename",
                    }
                    .into(),
                    Input::new(input).into_any_element(),
                    match purpose {
                        crate::app::NameFor::FileIn(_) | crate::app::NameFor::FolderIn(_) => "Create",
                        crate::app::NameFor::Rename(_) => "Rename",
                    }
                    .into(),
                    false,
                ),
                Modal::GoToLine { input, .. } => ("Go to line".into(), Input::new(input).into_any_element(), "Go".into(), false),
                Modal::Rename { input, .. } | Modal::NewBranch { input, .. } => (
                    if matches!(modal, Modal::NewBranch { .. }) { "New branch" } else { "Rename session" }.into(),
                    Input::new(input).into_any_element(),
                    if matches!(modal, Modal::NewBranch { .. }) { "Create" } else { "Rename" }.into(),
                    false,
                ),
            };
            div()
                .id("modal-card")
                .occlude()
                .track_focus(&self.modal_focus)
                .w(px(440.0))
                .p(px(22.0))
                .rounded(px(12.0))
                .bg(hsla(theme::PANEL_BG))
                .border_1()
                .border_color(hsla(theme::PANEL_BORDER))
                .shadow_lg()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .on_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| {
                    let key = ev.keystroke.key.as_str();
                    match (key, ws.modal.as_ref()) {
                        ("escape", _) => ws.act(Act::Dismiss, window, cx),
                        ("enter", Some(Modal::Confirm { .. })) => ws.submit_modal(window, cx),
                        _ => {}
                    }
                }))
                .child(div().text_size(crate::panels::scale::px(15.0)).font_weight(FontWeight::SEMIBOLD).child(title))
                .child(body)
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.0))
                        .mt(px(4.0))
                        .child(button("modal-cancel", "Cancel", false).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::Dismiss, window, cx))))
                        .child({
                            let b = if danger { danger_button("modal-ok", confirm_label) } else { button("modal-ok", confirm_label, true) };
                            b.on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.submit_modal(window, cx)))
                        }),
                )
        };
        Some(
            deferred(
                div()
                    .id("modal-backdrop")
                    .occlude()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x000000b0))
                    .on_mouse_down(MouseButton::Left, cx.listener(|ws, _: &MouseDownEvent, window, cx| ws.act(Act::Dismiss, window, cx)))
                    .child(card),
            )
            .with_priority(20)
            .into_any_element(),
        )
    }

    pub(super) fn render_toast(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.toast.as_ref()?;
        let action = t.action.clone().map(|(label, act)| {
            div().mt(px(8.0)).flex().gap(px(6.0)).child(button("toast-action", label, true).on_click(cx.listener(
                move |ws, _: &ClickEvent, window, cx| {
                    ws.toast = None;
                    ws.act(act.clone(), window, cx);
                },
            ))).child(button("toast-dismiss", "Dismiss", false).on_click(cx.listener(|ws, _: &ClickEvent, _w, cx| {
                ws.toast = None;
                cx.notify();
            })))
        });
        Some(
            deferred(
                div().absolute().bottom(px(18.0)).right(px(18.0)).child(
                    fade_in(
                    div()
                        .occlude()
                        .cursor_default()
                        .max_w(px(420.0))
                        .px(px(14.0))
                        .py(px(10.0))
                        .rounded(px(8.0))
                        .bg(hsla(0x1a1a1a))
                        .border_1()
                        .border_color(hsla(if t.error { 0x5a2327 } else { 0x2c2c2c }))
                        .text_size(crate::panels::scale::px(12.5))
                        .text_color(hsla(if t.error { theme::DANGER } else { theme::TEXT }))
                        .child(t.text.clone())
                        .children(action),
                        format!("toast-{}", t.serial),
                        160,
                        0.0,
                    ),
                ),
            )
            .with_priority(30)
            .into_any_element(),
        )
    }
}
