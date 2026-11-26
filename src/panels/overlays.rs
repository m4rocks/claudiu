//! Overlays drawn over the workspace: the context menu, the active modal and the toast.
//! The Settings modal's content is in `settings_view`, and the anchored dropdowns are in `workspace_pickers`.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, StatefulInteractiveElement, Styled, Window, deferred, div,
    prelude::FluentBuilder, px,
};

use crate::app::{
    Act, Modal, Workspace,
};
use gpui_kit::component::input::Input;
use crate::panels::theme::theme;
use gpui_kit::assets::IconName;
use crate::panels::widgets::{
    button, button_colored, button_disabled_leading, danger_button, dialog_card, dropdown_card, fade_in, icon, icon_button,
};
use crate::panels::ui;
use crate::panels::settings_view::render_settings_card;

impl Workspace {
    /// The open context or dropdown menu, on the kit's `menu`.
    pub(super) fn render_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let pos = menu.pos;
        let entries = menu
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let act = item.act.clone();
                let remove = item.remove.clone().map(|remove| {
                    icon_button(gpui::SharedString::from(format!("menu-remove-{i}")), IconName::X)
                        .invisible()
                        .group_hover(gpui::SharedString::from(format!("menu-{i}")), |s| s.visible())
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            ws.act(remove.clone(), window, cx);
                        }))
                        .into_any_element()
                });
                ui::menu::Entry::new(item.label.clone(), move |ws: &mut Self, window, cx| ws.act(act.clone(), window, cx))
                    .detail(item.detail.clone())
                    .icon(item.icon)
                    .shortcut(item.hint.clone())
                    .trailing(remove)
                    .destructive(item.danger)
                    .disabled(item.disabled)
                    .separator_before(item.separator_before)
            })
            .collect();
        let card = ui::menu::menu("menu", px(232.0).max(menu.min_w), &self.menu_nav, entries, |ws, window, cx| ws.close_menu(window, cx), window, cx);
        Some(ui::menu::popup(pos, card, 25, |ws: &mut Self, window, cx| ws.close_menu(window, cx), cx))
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
                    div().text_size(crate::panels::scale::px(13.0)).text_color(theme().muted_foreground).child(body.clone()).into_any_element(),
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
            dialog_card("modal-card")
                .track_focus(&self.modal_focus)
                .w(px(440.0))
                .p(px(22.0))
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
                    .bg(theme().backdrop)
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
            div().mt(px(8.0)).flex().child(button("toast-action", label, true).on_click(cx.listener(
                move |ws, _: &ClickEvent, window, cx| {
                    ws.toast = None;
                    ws.act(act.clone(), window, cx);
                },
            )))
        });
        Some(
            deferred(
                div().absolute().bottom(px(18.0)).right(px(18.0)).child(
                    fade_in(
                    dropdown_card("toast")
                        .max_w(px(420.0))
                        .px(px(14.0))
                        .py(px(10.0))
                        .text_size(crate::panels::scale::px(12.5))
                        .text_color(theme().foreground)
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

    /// Floating popover next to a hovered sidebar session that has a pending question, allowing the user to answer
    /// the ask/permission prompt directly without switching sessions.
    pub(super) fn render_question_popover(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pop = self.question_popover.as_ref()?;
        let id = pop.id.clone();
        let thread = self.live_view(&id)?.thread()?;
        let question = thread.read(cx).pending_question()?;
        let question_id = question.id;
        let session_title = self.store.session(&id).map(|r| r.display_title().to_string()).unwrap_or_else(|| "Session".into());

        let viewport = window.viewport_size();
        let pop_w = 340.0_f32;
        let est_h = (130.0 + (question.options.len() as f32) * 52.0).min(380.0);
        let left = (f32::from(pop.row_bounds.right()) + 8.0).min((f32::from(viewport.width) - pop_w - 12.0).max(12.0));
        let top = f32::from(pop.row_bounds.top()).clamp(12.0, (f32::from(viewport.height) - est_h - 12.0).max(12.0));
        let scaled = crate::panels::scale::px;

        let multi = question.multi_select;
        let selected_count = question.selected.len();
        let mut options = div().flex().flex_col().gap(px(5.0)).w_full();
        for (i, opt) in question.options.iter().enumerate() {
            let checked = multi && question.selected.contains(&i);
            let primary = (!multi && opt.kind == "allow_once" && i == 0) || checked;
            let answer_id = id.clone();
            let row = div()
                .id(gpui::SharedString::from(format!("sidebar-q-opt-{question_id}-{i}")))
                .w_full()
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(6.0))
                .bg(if primary { theme().row_active } else { theme().background })
                .border_1()
                .border_color(if primary { theme().ring } else { theme().panel_border })
                .hover(|s| s.bg(theme().accent))
                .cursor_pointer()
                .flex()
                .items_start()
                .gap(px(8.0))
                .when(multi, |row| {
                    row.child(
                        div()
                            .mt(px(2.0))
                            .flex_none()
                            .size(px(14.0))
                            .rounded(px(3.5))
                            .border_1()
                            .border_color(if checked { theme().primary } else { theme().panel_border })
                            .bg(if checked { theme().primary } else { theme().background })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(checked, |box_el| box_el.child(icon(IconName::Check, 10.0).text_color(theme().background))),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().text_size(scaled(12.0)).font_weight(FontWeight::MEDIUM).text_color(theme().foreground).child(opt.name.clone()))
                        .children(opt.description.clone().map(|desc| div().text_size(scaled(11.0)).text_color(theme().muted_foreground).child(desc))),
                )
                .on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
                    cx.stop_propagation();
                    if multi {
                        ws.toggle_session_question_option(&answer_id, question_id, i, cx);
                    } else {
                        ws.answer_session_question(&answer_id, question_id, Some(i), cx);
                    }
                }));
            options = options.child(row);
        }

        let decline_id = id.clone();
        let submit_id = id.clone();
        let open_id = id.clone();
        let ws_weak = cx.entity().downgrade();
        let card = div()
            .id("sidebar-question-popover")
            .occlude()
            .cursor_default()
            .w(px(pop_w))
            .p(px(12.0))
            .rounded(px(10.0))
            .bg(theme().card)
            .border_1()
            .border_color(theme().ring)
            .flex()
            .flex_col()
            .gap(px(8.0))
            .on_hover(move |inside, _w, cx| {
                let _ = ws_weak.update(cx, |ws, cx| {
                    ws.question_popover_hovered = *inside;
                    if *inside {
                        ws.question_popover_seq = ws.question_popover_seq.wrapping_add(1);
                    } else {
                        ws.schedule_close_question_popover(cx);
                    }
                });
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(div().min_w_0().flex_1().truncate().text_size(scaled(11.0)).text_color(theme().muted_foreground).child(session_title))
                    .child(
                        div()
                            .flex_none()
                            .px(px(6.0))
                            .py(px(1.5))
                            .rounded(px(4.0))
                            .bg(theme().row_active)
                            .text_size(scaled(10.0))
                            .text_color(theme().primary)
                            .child("Needs answer"),
                    ),
            )
            .child(div().text_size(scaled(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme().foreground).child(question.title))
            .child(self.sidebar_question_scroll.wire(div().id("sidebar-q-options").max_h(px(220.0)), window).child(options))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pt(px(4.0))
                    .border_t_1()
                    .border_color(theme().border)
                    .child(
                        button("sidebar-q-decline", "Decline", false).h(px(24.0)).on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
                            cx.stop_propagation();
                            ws.answer_session_question(&decline_id, question_id, None, cx);
                        })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .when(multi, |d| {
                                let label = if selected_count > 0 { format!("Submit ({selected_count})") } else { "Submit".into() };
                                let btn = if selected_count > 0 {
                                    button_colored(
                                        "sidebar-q-submit",
                                        Some(IconName::Check),
                                        &label,
                                        theme().primary,
                                        theme().background,
                                        theme().primary_hover,
                                        None,
                                        true,
                                    )
                                    .on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
                                        cx.stop_propagation();
                                        ws.submit_session_question(&submit_id, question_id, cx);
                                    }))
                                    .h(px(24.0))
                                    .px(px(8.0))
                                    .into_any_element()
                                } else {
                                    button_disabled_leading("sidebar-q-submit", Some(IconName::Check), &label)
                                        .h(px(24.0))
                                        .px(px(8.0))
                                        .into_any_element()
                                };
                                d.child(btn)
                            })
                            .child(
                                button("sidebar-q-open", "Open session", false).h(px(24.0)).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    ws.question_popover = None;
                                    ws.question_popover_hovered = false;
                                    ws.act(Act::Select(open_id.clone()), window, cx);
                                })),
                            ),
                    ),
            );

        Some(
            deferred(div().absolute().left(px(left)).top(px(top)).child(card))
                .with_priority(25)
                .into_any_element(),
        )
    }
}
