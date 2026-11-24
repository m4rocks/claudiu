//! The Settings modal: its navigation rail and the General, Worktrees and feature pages with their rows.
//! The pages that list things to act on (Projects, Harnesses) are in `settings_pages`.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px,
};

use crate::mcp::Feature;
use crate::harness::permission::Permission;
use crate::app::{
    Act, Modal,
    SettingsPage, Workspace, WorkspacePicker,
};
use gpui_kit::component::input::Input;
use crate::panels::logos;
use crate::panels::theme::{self, hsla};
use crate::updater::Status;
use gpui_kit::assets::IconName;
use crate::panels::settings_pages::{flip, settings_group, toggle_switch};
use crate::panels::widgets::{
    Anchor, Dropdown, button, button_disabled, danger_button, dialog_card, icon, key_badge, section_label,
};

/// Split-pane Settings window: left navigation rail with icons + version footer, right scrollable page with grouped
/// section cards and a dedicated Save/Cancel action bar.
pub(super) fn render_settings_card(
    ws: &Workspace,
    viewport: gpui::Size<gpui::Pixels>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::Stateful<gpui::Div> {
    let (page, worktrees, scroll) = (ws.settings_page, &ws.worktrees_page, &ws.settings_scroll);
    let Some(Modal::Settings { shell, default_model, text_generation_model, effort, permission, updates, scale, follow_up, auto_compact, auto_compact_threshold, m4code_off, skill_search, list_focus, renaming, .. }) = &ws.modal else {
        return div().id("modal-card");
    };
    let (updates, scale, follow_up, auto_compact, auto_compact_threshold) = (*updates, *scale, *follow_up, *auto_compact, *auto_compact_threshold);
    let scaled = crate::panels::scale::px;
    let faint = |text: String| div().text_size(scaled(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(text);

    let content: AnyElement = match page {
        SettingsPage::General => {
            let model_open = matches!(ws.picker_kind(), Some(WorkspacePicker::SettingsModel { text_generation: false, .. }));
            let generation_open = matches!(ws.picker_kind(), Some(WorkspacePicker::SettingsModel { text_generation: true, .. }));
            let generation_label = text_generation_model.as_ref().map_or_else(|| "Local summary".into(), |model| {
                let name = ws.store.data.settings.harness_models(model.harness).iter()
                    .find(|item| item.id == model.id)
                    .map_or(model.id.as_str(), |item| item.name.as_str());
                format!("{} · {name}", model.harness.name())
            });
            let effort_open = matches!(ws.picker_kind(), Some(WorkspacePicker::SettingsEffort));
            let perm_open = matches!(ws.picker_kind(), Some(WorkspacePicker::SettingsPermission));
            let perm_display = permission.map_or("Harness default", |p| p.label()).to_string();
            let mut session_rows: Vec<AnyElement> = vec![
                default_model_row(default_model, model_open, cx).into_any_element(),
                settings_picker_row("settings-effort", ("Effort", "Reasoning depth for new sessions"), effort.label().to_string(), effort_open, |ws, at, window, cx| ws.open_settings_effort_picker(at, window, cx), cx).into_any_element(),
                settings_picker_row("settings-permission", ("Permission mode", "Approval policy for tool calls and edits"), perm_display, perm_open, |ws, at, window, cx| ws.open_settings_permission_picker(at, window, cx), cx).into_any_element(),
            ];
            session_rows.push(
                row_shell(
                    "Messages sent while the agent works",
                    Some("Steer reaches the agent in its turn; Queue waits for it to end. Cmd/Ctrl+Enter does the other."),
                    div()
                        .flex_none()
                        .flex()
                        .gap(px(4.0))
                        .child(follow_up_button(crate::store::FollowUp::Queue, "Queue", follow_up, cx))
                        .child(follow_up_button(crate::store::FollowUp::Steer, "Steer", follow_up, cx))
                        .into_any_element(),
                )
                .into_any_element(),
            );
            session_rows.push(
                div()
                    .id("settings-auto-compact")
                    .px(px(14.0))
                    .py(px(11.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                    .child(row_labels("Auto-compact", Some("Automatically compact conversation context between turns")))
                    .child(toggle_switch(auto_compact))
                    .on_click(cx.listener(|ws, _: &ClickEvent, _, cx| {
                        if let Some(Modal::Settings { auto_compact, .. }) = ws.modal.as_mut() {
                            *auto_compact = !*auto_compact;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            if auto_compact {
                session_rows.push(
                    row_shell(
                        "Auto-compact threshold",
                        Some("Context usage percentage that triggers automatic compaction"),
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .p(px(2.0))
                            .rounded(px(6.0))
                            .bg(hsla(theme::PANEL_BG))
                            .border_1()
                            .border_color(hsla(theme::PANEL_BORDER))
                            .child(compaction_step("compact-threshold-down", "−", -(crate::harness::compaction::THRESHOLD_STEP as i64), cx))
                            .child(div().w(px(42.0)).flex().justify_center().text_size(scaled(12.0)).text_color(hsla(theme::TEXT)).child(format!("{auto_compact_threshold}%")))
                            .child(compaction_step("compact-threshold-up", "+", crate::harness::compaction::THRESHOLD_STEP as i64, cx))
                            .into_any_element(),
                    )
                    .into_any_element(),
                );
            }
            if *permission == Some(Permission::FullAccess) {
                session_rows.push(
                    div()
                        .px(px(14.0))
                        .py(px(8.0))
                        .bg(hsla(0x191512))
                        .text_size(scaled(11.5))
                        .text_color(hsla(theme::WARN))
                        .child("Every new session will run commands and edit files without asking.")
                        .into_any_element(),
                );
            }
            let env_rows: Vec<AnyElement> = vec![
                settings_picker_row("settings-text-generation", ("Text generation model", "Writes session titles and commit messages"), generation_label, generation_open, |ws, at, window, cx| ws.open_settings_model_picker(at, true, window, cx), cx).into_any_element(),
                row_shell(
                    "Interface scale",
                    Some("Adjusts UI text and terminal font size"),
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .p(px(2.0))
                        .rounded(px(6.0))
                        .bg(hsla(theme::PANEL_BG))
                        .border_1()
                        .border_color(hsla(theme::PANEL_BORDER))
                        .child(scale_step("scale-down", "−", -(crate::panels::scale::STEP as i32), cx))
                        .child(div().w(px(42.0)).flex().justify_center().text_size(scaled(12.0)).text_color(hsla(theme::TEXT)).child(format!("{scale}%")))
                        .child(scale_step("scale-up", "+", crate::panels::scale::STEP as i32, cx))
                        .into_any_element(),
                )
                .into_any_element(),
                row_shell(
                    "Shell",
                    Some("Program used for the bottom shell panel"),
                    div().w(px(230.0)).flex_none().child(Input::new(shell).h(px(28.0)).text_size(scaled(12.0))).into_any_element(),
                )
                .into_any_element(),
                row_shell(
                    "Environment setup",
                    Some("Verify installed CLIs, Node, and agent bridges"),
                    button("settings-check-setup", "Check setup", false)
                        .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::OpenSetup, window, cx)))
                        .into_any_element(),
                )
                .into_any_element(),
            ];

            let check_btn = if ws.update.status == Status::Checking {
                button_disabled("settings-check-updates", "Checking…").into_any_element()
            } else {
                button("settings-check-updates", "Check now", false)
                    .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::CheckForUpdates, window, cx)))
                    .into_any_element()
            };
            let updates_row = div()
                .id("settings-updates")
                .px(px(14.0))
                .py(px(11.0))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.0))
                .cursor_pointer()
                .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                .child(row_labels("Automatic updates", Some("Check for new m4code releases in the background")))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(check_btn))
                        .child(toggle_switch(updates)),
                )
                .on_click(cx.listener(|ws, _: &ClickEvent, _, cx| {
                    if let Some(Modal::Settings { updates, .. }) = ws.modal.as_mut() {
                        *updates = !*updates;
                    }
                    cx.notify();
                }))
                .into_any_element();

            div()
                .flex()
                .flex_col()
                .gap(px(18.0))
                .child(settings_group("NEW SESSIONS", None, session_rows))
                .child(settings_group("INTERFACE & ENVIRONMENT", None, env_rows))
                .child(settings_group("UPDATES", None, vec![updates_row]))
                .into_any_element()
        }
        SettingsPage::Harnesses => ws.harnesses_page(cx).into_any_element(),
        SettingsPage::Skills => ws.skills_page(skill_search, cx).into_any_element(),
        SettingsPage::M4codeTools => {
            let rows: Vec<AnyElement> = Feature::ALL
                .into_iter()
                .map(|feature| {
                    let on = !m4code_off.contains(&feature);
                    div()
                        .id(("settings-m4code", feature as usize))
                        .px(px(14.0))
                        .py(px(11.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                        .child(row_labels(feature.label(), Some(feature.description())))
                        .child(toggle_switch(on))
                        .on_click(cx.listener(move |ws, _: &ClickEvent, _, cx| {
                            if let Some(Modal::Settings { m4code_off, .. }) = ws.modal.as_mut() {
                                flip(m4code_off, feature);
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(settings_group("BUILT-IN MCP TOOLS", None, rows))
                .child(faint("Running sessions keep their current tools. Resumed sessions pick up changes on their next compaction.".into()))
                .into_any_element()
        }
        SettingsPage::Keyboard => keyboard_page().into_any_element(),
        SettingsPage::Projects => div()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(ws.projects_section(renaming.as_ref(), cx))
            .child(worktrees_section(worktrees, cx))
            .into_any_element(),
    };

    let nav = SettingsPage::ALL.into_iter().enumerate().fold(
        div().id("settings-categories").track_focus(list_focus).flex().flex_col().gap(px(2.0)),
        |list, (i, p)| {
            let selected = p == page;
            let focus = list_focus.clone();
            list.child(
                div()
                    .id(("settings-category", i))
                    .h(scaled(30.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .rounded(px(6.0))
                    .text_size(scaled(13.0))
                    .font_weight(if selected { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                    .text_color(hsla(if selected { theme::TEXT } else { theme::TEXT_DIM }))
                    .when(selected, |d| d.bg(hsla(theme::ROW_ACTIVE)))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
                    .child(icon(p.icon(), 13.5).text_color(hsla(if selected { theme::ACCENT } else { theme::TEXT_FAINT })))
                    .child(p.label())
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        ws.set_settings_page(p, cx);
                        window.focus(&focus, cx);
                    })),
            )
        },
    );

    let sidebar = div()
        .w(scaled(204.0))
        .flex_none()
        .h_full()
        .bg(hsla(0x09090a))
        .border_r_1()
        .border_color(hsla(theme::BORDER))
        .p(px(14.0))
        .flex()
        .flex_col()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(div().px(px(10.0)).pt(px(4.0)).text_size(scaled(14.5)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(theme::TEXT)).child("Settings"))
                .child(nav),
        )
        .child(
            div()
                .px(px(10.0))
                .pt(px(12.0))
                .border_t_1()
                .border_color(hsla(theme::BORDER))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().text_size(scaled(11.5)).font_weight(FontWeight::MEDIUM).text_color(hsla(theme::TEXT_DIM)).child(format!("m4code {}", env!("CARGO_PKG_VERSION"))))
                .child(div().text_size(scaled(10.5)).text_color(hsla(theme::TEXT_FAINT)).child("Independent project")),
        );

    let page_header = div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .pb(px(14.0))
        .border_b_1()
        .border_color(hsla(theme::BORDER))
        .child(div().text_size(scaled(17.0)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(theme::TEXT)).child(page.label()))
        .child(div().text_size(scaled(12.0)).text_color(hsla(theme::TEXT_DIM)).child(page.subtitle()));

    let footer = div()
        .flex_none()
        .px(px(24.0))
        .py(px(12.0))
        .border_t_1()
        .border_color(hsla(theme::BORDER))
        .bg(hsla(theme::PANEL_BG))
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(scaled(11.0))
                .text_color(hsla(theme::TEXT_FAINT))
                .child(key_badge("Esc"))
                .child("close"),
        )
        .child(
            div()
                .text_size(scaled(11.0))
                .text_color(hsla(theme::TEXT_FAINT))
                .child("Changes save automatically"),
        );

    let main_pane = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(hsla(theme::PANEL_BG))
        .child(
            scroll.wire(div().id("settings-page").flex_1().min_h_0(), window).child(
                div()
                    .px(px(26.0))
                    .py(px(20.0))
                    .flex()
                    .flex_col()
                    .gap(px(18.0))
                    .child(page_header)
                    .child(content),
            ),
        )
        .child(footer);

    dialog_card("modal-card")
        .track_focus(&ws.modal_focus)
        .w(scaled(880.0).min(viewport.width - px(48.0)))
        .h(scaled(620.0).min(viewport.height - px(48.0)))
        .overflow_hidden()
        .flex()
        .capture_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| ws.settings_key(ev, window, cx)))
        .on_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| {
            if ev.keystroke.key.as_str() == "escape" {
                ws.act(Act::Dismiss, window, cx);
            }
        }))
        .child(sidebar)
        .child(main_pane)
}

fn row_labels(title: &str, subtitle: Option<&str>) -> gpui::Div {
    let scaled = crate::panels::scale::px;
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(2.0))
        .child(div().text_size(scaled(13.0)).text_color(hsla(theme::TEXT)).child(title.to_string()))
        .children(subtitle.map(|s| div().text_size(scaled(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(s.to_string())))
}

/// One side of the Queue / Steer switch; the chosen one is drawn as the primary button.
fn follow_up_button(choice: crate::store::FollowUp, label: &'static str, current: crate::store::FollowUp, cx: &mut Context<Workspace>) -> impl IntoElement {
    button(format!("settings-follow-up-{label}"), label, choice == current).on_click(cx.listener(move |ws, _: &ClickEvent, _, cx| {
        if let Some(Modal::Settings { follow_up, .. }) = ws.modal.as_mut() {
            *follow_up = choice;
        }
        cx.notify();
    }))
}

fn row_shell(title: &str, subtitle: Option<&str>, trailing: AnyElement) -> gpui::Div {
    div()
        .px(px(14.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .child(row_labels(title, subtitle))
        .child(trailing)
}

/// Settings → Keyboard: each group of shortcuts in its own card, what it does on the left and its keys on the right.
fn keyboard_page() -> gpui::Div {
    let rows = crate::app::keymap::shortcuts();
    let mut groups: Vec<(&str, Vec<AnyElement>)> = vec![];
    for row in rows {
        let line = row_shell(row.what, None, key_badge(crate::app::keymap::label(&row.keys)).into_any_element()).into_any_element();
        match groups.last_mut() {
            Some((group, lines)) if *group == row.group => lines.push(line),
            _ => groups.push((row.group, vec![line])),
        }
    }
    groups.into_iter().fold(div().flex().flex_col().gap(px(18.0)), |page, (group, lines)| page.child(settings_group(group.to_uppercase(), None, lines)))
}

/// The Worktrees section on the Projects page: worktree folders m4code made, grouped by repository.
fn worktrees_section(page: &crate::app::WorktreesPage, cx: &mut Context<Workspace>) -> gpui::Div {
    let scaled = crate::panels::scale::px;
    let faint = |text: String| div().text_size(scaled(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(text);
    let reveal = if cfg!(target_os = "macos") { "Show in Finder" } else { "Show in File Explorer" };
    let empty_card = |msg: &str| {
        settings_group(
            "WORKTREES",
            None,
            vec![
                div()
                    .px(px(14.0))
                    .py(px(14.0))
                    .text_size(scaled(12.0))
                    .text_color(hsla(theme::TEXT_FAINT))
                    .child(msg.to_string())
                    .into_any_element(),
            ],
        )
    };
    let Some(stored) = &page.stored else {
        return empty_card("Looking for worktrees…");
    };
    if stored.is_empty() {
        return empty_card("No worktrees yet. Create one from the branch & worktree dialog in the top bar or welcome screen.");
    }
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut group: Option<&std::path::Path> = None;
    for (i, item) in stored.iter().enumerate() {
        let repo_folder = item.folder.parent();
        if group != repo_folder {
            group = repo_folder;
            let heading = match &item.main_checkout {
                Some(main) => main.to_string_lossy().into_owned(),
                None => format!("{} (repository not found)", repo_folder.and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy()),
            };
            rows.push(
                div()
                    .px(px(14.0))
                    .py(px(7.0))
                    .bg(hsla(0x101012))
                    .child(section_label(heading))
                    .into_any_element(),
            );
        }
        let name = item.folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let (folder, remove) = (item.folder.clone(), item.main_checkout.clone());
        let revealed = folder.clone();
        let asked = page.asking.as_ref().filter(|a| a.folder == item.folder);
        let ask = match remove {
            Some(main_checkout) => Act::RemoveWorktree { main_checkout, folder: folder.clone() },
            None => Act::AskDeleteUnlisted(folder.clone()),
        };
        let row = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(14.0))
            .py(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(div().text_size(scaled(13.0)).font_weight(FontWeight::MEDIUM).child(name))
                            .child(faint(item.folder.to_string_lossy().into_owned()).overflow_hidden().text_ellipsis().whitespace_nowrap()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .gap(px(6.0))
                            .child(button(format!("worktree-reveal-{i}"), reveal, false).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Reveal(revealed.clone()), window, cx))))
                            .child(button(format!("worktree-remove-{i}"), "Remove", false).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(ask.clone(), window, cx)))),
                    ),
            );
        rows.push(
            match asked {
                None => row,
                Some(asked) => {
                    let act = asked.act.clone();
                    let confirm = if asked.danger { danger_button(format!("worktree-confirm-{i}"), asked.confirm.clone()) } else { button(format!("worktree-confirm-{i}"), asked.confirm.clone(), true) };
                    row.child(faint(asked.body.clone())).child(
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child(button(format!("worktree-cancel-{i}"), "Cancel", false).on_click(cx.listener(|ws, _: &ClickEvent, _window, cx| {
                                ws.worktrees_page.asking = None;
                                cx.notify();
                            })))
                            .child(confirm.on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(act.clone(), window, cx)))),
                    )
                }
            }
            .into_any_element(),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(settings_group("WORKTREES", None, rows))
        .child(faint("Worktrees m4code makes live in its own data folder, outside your repositories.".into()))
}

/// Default model row inside the New sessions card; clicking opens the universal model dropdown anchored to its pill.
fn default_model_row(current: &crate::harness::CatalogModel, open: bool, cx: &mut Context<Workspace>) -> impl IntoElement {
    let scaled = crate::panels::scale::px;
    let accent = hsla(theme::ACCENT);
    let row_anchor = Anchor::default();
    let pill_anchor = Anchor::default();
    div()
        .id("settings-default-model")
        .relative()
        .px(px(14.0))
        .py(px(11.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
        .child(row_anchor.probe())
        .child(row_labels("Default model", Some("Model used when starting a new session")))
        .child(
            div()
                .relative()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(9.0))
                .h(px(26.0))
                .rounded(px(6.0))
                .bg(hsla(if open { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
                .border_1()
                .border_color(hsla(if open { theme::ACCENT } else { theme::PANEL_BORDER }))
                .child(pill_anchor.probe())
                .child(logos::draw(logos::for_harness(current.harness), 11.5, hsla(theme::TEXT_DIM)))
                .child(div().max_w(px(220.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(scaled(12.0)).text_color(accent).child(current.name.clone()))
                .child(icon(IconName::ChevronDown, 10.5).text_color(hsla(theme::TEXT_FAINT))),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                ws.open_settings_model_picker(pill_anchor.below_right().with_hit(row_anchor.bounds()), false, window, cx);
            }),
        )
}

fn settings_picker_row(
    id: &'static str,
    (label, subtitle): (&'static str, &'static str),
    current: String,
    open: bool,
    on_open: impl Fn(&mut Workspace, Dropdown, &mut Window, &mut Context<Workspace>) + 'static,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let scaled = crate::panels::scale::px;
    let row_anchor = Anchor::default();
    let pill_anchor = Anchor::default();
    div()
        .id(id)
        .relative()
        .px(px(14.0))
        .py(px(11.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
        .child(row_anchor.probe())
        .child(row_labels(label, Some(subtitle)))
        .child(
            div()
                .relative()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(9.0))
                .h(px(26.0))
                .rounded(px(6.0))
                .bg(hsla(if open { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
                .border_1()
                .border_color(hsla(if open { theme::ACCENT } else { theme::PANEL_BORDER }))
                .text_size(scaled(12.0))
                .child(pill_anchor.probe())
                .child(div().max_w(px(220.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(hsla(theme::ACCENT)).child(current))
                .child(icon(IconName::ChevronDown, 10.5).text_color(hsla(theme::TEXT_FAINT))),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                on_open(ws, pill_anchor.below_right().with_hit(row_anchor.bounds()), window, cx);
            }),
        )
}

/// A − / + button in the Settings modal that moves the scale by `by` percent, within the allowed range.
fn scale_step(id: &'static str, label: &'static str, by: i32, cx: &mut Context<Workspace>) -> impl IntoElement {
    div()
        .id(id)
        .w(px(22.0))
        .h(px(22.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .text_color(hsla(theme::TEXT_DIM))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)).text_color(hsla(theme::TEXT)))
        .child(label)
        .on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
            if let Some(Modal::Settings { scale, .. }) = ws.modal.as_mut() {
                *scale = (*scale as i32 + by).clamp(crate::panels::scale::MIN as i32, crate::panels::scale::MAX as i32) as u32;
                crate::panels::scale::set(*scale);
            }
            cx.notify();
        }))
}

/// A − / + button in the Settings modal that moves the auto-compact threshold by `by` percent.
fn compaction_step(id: &'static str, label: &'static str, by: i64, cx: &mut Context<Workspace>) -> impl IntoElement {
    div()
        .id(id)
        .w(px(22.0))
        .h(px(22.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .text_color(hsla(theme::TEXT_DIM))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)).text_color(hsla(theme::TEXT)))
        .child(label)
        .on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
            if let Some(Modal::Settings { auto_compact_threshold, .. }) = ws.modal.as_mut() {
                let next = (*auto_compact_threshold as i64 + by).max(0) as u64;
                *auto_compact_threshold = crate::harness::compaction::clamp_threshold(next);
            }
            cx.notify();
        }))
}
