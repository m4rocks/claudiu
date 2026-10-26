//! Main area (the shown session / home), overlays (menu, modal, toast), and the root `Render`.

use gpui::{
    AnyElement, Entity, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, prelude::FluentBuilder,
    px, rgba,
};

use crate::claude::defaults::{Effort, PermissionMode};
use crate::claude::mcp::Feature;
use crate::claude::trim::Trim;
use crate::app::{
    Act, CloseSession, SettingsPage, GitOp, FocusSearch, ImportProject, Main, MenuItem, Modal, NewClaude, NextSession, OpenGithub, OpenSettings, PrevSession, ShellDrag, SwapFocus,
    ToggleEditor, ToggleRight, ToggleShell, ToggleSidebar, Workspace, SHELL_HEIGHT,
};
use gpui_kit::component::{TitleBar, input::{Input, InputState}};
use crate::store::{SessionKind, now};
use crate::panels::theme::{self, hsla};
use crate::updater::Status;
use gpui_kit::assets::IconName;
use crate::panels::widgets::{Trigger, ago, button, clawd, danger_button, fade_in, icon, icon_button, key_hint, section_label, short_path, shortcut, spinner, tick};

impl Workspace {
    // ------------------------------------------------------------------ top bar

    fn render_topbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let crate::panels::inspector::Shown { rec, cwd, project } = self.shown();
        // Git actions live in the right sidebar; the branch pill here needs the repository of the session's own folder.
        let repo = project.as_ref().filter(|p| p.git.is_some()).and(cwd.clone());
        let busy = self.git_busy;
        let hints = self.shortcuts_shown;

        let title = rec.as_ref().map(|r| r.display_title().to_string()).unwrap_or_else(|| "Claudiu".into());
        let branch = project.as_ref().and_then(|p| p.git.as_ref()).and_then(|g| g.branch.clone());
        // `.occlude()` on both groups: the bar is a Windows drag (HTCAPTION) area, which would otherwise swallow clicks on the buttons inside it.
        // The top bar spans the window and is drawn as the window background, not as a panel. macOS keeps its
        // traffic lights at the left (they are hidden in fullscreen); Windows/Linux get min/max/close at the right.
        let left_pad = if cfg!(target_os = "macos") && !window.is_fullscreen() { 80.0 } else { 10.0 };
        TitleBar::new()
            .h(px(theme::TOPBAR_HEIGHT))
            .pl(px(left_pad))
            .bg(hsla(theme::APP_BG))
            .border_b_0()
            // TitleBar's inner row can't shrink below its in-flow content's width, so a long title would push
            // the right-hand buttons out of view. An absolute overlay takes the content out of that sizing.
            .child(div().absolute().size_full().flex().items_center().justify_between().pr(px(theme::PANEL_GAP)).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    // No `.occlude()` here: the title and the empty space stay part of the drag area; the buttons occlude themselves.
                    .child(
                        icon_button("toggle-sidebar", IconName::PanelLeft)
                            .occlude()
                            .on_click(cx.listener(|ws, _: &ClickEvent, _w, cx| ws.toggle_left(cx))),
                    )
                    .when(hints, |d| d.child(key_hint("Shift+B")))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(crate::panels::scale::px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .when_some(cwd, |d, p| {
                        d.child(div().flex_none().text_size(crate::panels::scale::px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(short_path(&p)))
                    })
                    .when_some(branch.zip(repo), |d, (b, repo)| {
                        // Click: switch branch or create one (see Workspace::open_branch_menu).
                        let tail: AnyElement = if busy == Some(GitOp::Branch) { spinner("branch-busy", 12.0).into_any_element() } else { icon(IconName::ChevronDown, 12.0).into_any_element() };
                        d.child(
                            div()
                                .id("branch")
                                .occlude()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .px(px(7.0))
                                .py(px(1.0))
                                .rounded(px(10.0))
                                .bg(hsla(0x26262a))
                                .text_size(crate::panels::scale::px(11.0))
                                .text_color(hsla(theme::TEXT_DIM))
                                .cursor_pointer()
                                .hover(|s| s.bg(hsla(0x323236)).text_color(hsla(theme::TEXT)))
                                .child(icon(IconName::GitBranch, 12.0))
                                .child(b)
                                .child(tail)
                                .dropdown(cx, move |ws, at, _w, cx| ws.open_branch_menu(repo.clone(), at, cx)),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(if hints { 4.0 } else { 0.0 }))
                    .occlude()
                    .when(hints, |d| d.child(key_hint("Shift+E")))
                    .child(icon_button("toggle-editor", IconName::FileCode).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleEditor, window, cx))))
                    .when(hints, |d| d.child(key_hint("J")))
                    .child(icon_button("toggle-shell", IconName::PanelBottom).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                        if let Some(id) = ws.active.clone() {
                            ws.act(Act::ToggleShell(id), window, cx)
                        }
                    })))
                    .when(hints, |d| d.child(key_hint("Shift+R")))
                    .child(icon_button("toggle-right", IconName::PanelRight).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.toggle_right(window, cx)))),
            ))
    }

    fn render_update_banner(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (text, action) = match &self.update.status {
            Status::Available { version } => (format!("Claudiu {version} is available"), Some(("Download", Act::InstallUpdate))),
            Status::Downloading => ("Downloading update…".to_string(), None),
            Status::Ready { version } => (format!("Claudiu {version} is ready"), Some(("Restart to update", Act::InstallUpdate))),
            _ => return None,
        };
        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(28.0))
                .flex_none()
                .px(px(10.0))
                .rounded(px(6.0))
                .bg(hsla(0x1d1512))
                .border_1()
                .border_color(hsla(0x3a2a24))
                .child(div().text_size(crate::panels::scale::px(12.0)).text_color(hsla(theme::ACCENT)).child(text))
                .when_some(action, |d, (label, act)| {
                    d.child(button("update-action", label, true).h(px(22.0)).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        ws.act(act.clone(), window, cx)
                    })))
                })
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------------ the shown session

    fn render_session(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let blank = || div().size_full().bg(hsla(theme::TERMINAL_BG)).into_any_element();
        let Some(id) = self.active.clone() else { return blank() };
        let Some(term) = self.live_view(&id).cloned() else { return blank() };
        let rec = self.store.session(&id).cloned();
        let exit = term.read(cx).exit_code;
        let is_claude = rec.as_ref().is_some_and(|r| r.kind == SessionKind::Claude);
        let rid = id.clone();

        let banner = exit.map(|code| {
            let label = match code {
                Some(0) | None => "Session ended".to_string(),
                Some(c) => format!("Session ended (exit code {c})"),
            };
            div()
                .absolute()
                .bottom(px(12.0))
                .left(px(12.0))
                .right(px(12.0))
                .flex()
                .items_center()
                .justify_between()
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(hsla(0x181818))
                .border_1()
                .border_color(hsla(0x2a2a2a))
                .child(div().text_size(crate::panels::scale::px(12.5)).text_color(hsla(theme::TEXT_DIM)).child(label))
                .children(is_claude.then(|| {
                    button("ended-act", "Resume", true).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Resume(rid.clone()), window, cx)))
                }))
        });

        let pane = fade_in(
            div()
                .id("pane")
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .bg(hsla(theme::TERMINAL_BG))
                .child(div().flex_1().min_h_0().p(px(6.0)).child(term))
                .children(banner),
            format!("pane-in-{id}"),
            150,
            0.4,
        );
        pane.into_any_element()
    }

    /// The session's shell as its own panel under the main one: same gutter, rounding, grip and slide as the sidebars.
    fn render_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = self.active.clone().filter(|_| matches!(self.main, Main::Terminals))?;
        let (view, full, open) = self.shells.get(&id).map(|p| (p.view.clone(), p.height, p.open))?;
        let target = if open { full } else { 0.0 };
        if self.shell_drag.is_some() {
            self.shell_h.snap(target);
        }
        let height = self.shell_h.to(target, window);
        if height < 0.5 {
            return None;
        }
        // The top edge is the drag handle, with a small grip in the gutter.
        let handle_center = theme::PANEL_GAP / 2.0 + 3.0;
        let handle = div()
            .id("shell-resize")
            .group("shell-resize")
            .absolute()
            .left_0()
            .top(px(-handle_center))
            .w_full()
            .h(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_row_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                    let height = ws.shells.get(&id).map_or(SHELL_HEIGHT, |p| p.height);
                    ws.shell_drag = Some(ShellDrag { id: id.clone(), y: f32::from(ev.position.y), height });
                    cx.notify();
                }),
            )
            .child(div().w(px(28.0)).h(px(2.0)).rounded_full().bg(hsla(0x2b2b30)).group_hover("shell-resize", |s| s.bg(hsla(0x4a4a52))));
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .h(px(height + theme::PANEL_GAP))
                .overflow_hidden()
                .justify_end()
                .child(div().flex_none().h(px(theme::PANEL_GAP)))
                .child(
                    div()
                        .id("shell-panel")
                        .relative()
                        .flex_none()
                        .h(px(full))
                        .child(
                            div()
                                .size_full()
                                .rounded(px(theme::PANEL_RADIUS))
                                .overflow_hidden()
                                .border_1()
                                .border_color(hsla(theme::PANEL_BORDER))
                                .bg(hsla(theme::TERMINAL_BG))
                                .p(px(6.0))
                                .child(view),
                        )
                        .child(handle),
                )
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------------ details & home

    fn render_home(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shortcut_row = |keys: &str, what: &str| {
            div()
                .flex()
                .justify_between()
                .gap(px(28.0))
                .child(div().text_size(crate::panels::scale::px(12.0)).text_color(hsla(theme::TEXT_DIM)).child(what.to_string()))
                .child(div().text_size(crate::panels::scale::px(12.0)).text_color(hsla(theme::TEXT_FAINT)).child(keys.to_string()))
        };
        let mut recent = div().flex().flex_col().gap(px(2.0)).mt(px(26.0)).w(px(420.0));
        // Newest first: (id, title, last active).
        let mut sessions: Vec<(String, String, i64)> = self
            .store
            .data
            .sessions
            .iter()
            .filter(|s| s.kind == SessionKind::Claude && !s.hidden)
            .map(|s| (s.id.clone(), s.display_title().to_string(), s.last_active))
            .collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.2));
        let t = now();
        if !sessions.is_empty() {
            recent = recent.child(div().mb(px(4.0)).child(section_label("RECENT")));
        }
        for (i, (id, title, last_active)) in sessions.into_iter().take(5).enumerate() {
            recent = recent.child(
                div()
                    .id(gpui::SharedString::from(format!("recent-{i}")))
                    .flex()
                    .justify_between()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(|st| st.bg(hsla(theme::ROW_HOVER)))
                    .child(div().text_size(crate::panels::scale::px(13.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                    .child(div().text_size(crate::panels::scale::px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(ago(last_active, t)))
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Select(id.clone()), window, cx))),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(clawd(72.0))
            .child(div().mt(px(20.0)).text_size(crate::panels::scale::px(22.0)).font_weight(FontWeight::SEMIBOLD).child("Claudiu"))
            .child(div().mt(px(4.0)).text_size(crate::panels::scale::px(13.0)).text_color(hsla(theme::TEXT_DIM)).child("A workspace for your Claude Code sessions"))
            .child(
                div()
                    .mt(px(22.0))
                    .flex()
                    .gap(px(8.0))
                    .child(button("home-claude", "New", true).dropdown(
                        cx,
                        |ws, at, _w, cx| {
                            let item = |label: String, act: Act, sep: bool| crate::app::MenuItem { label, hint: None, act, danger: false, disabled: false, separator_before: sep };
                            let mut items: Vec<_> = ws.store.data.projects.iter().map(|p| item(p.name.clone(), Act::NewClaude { cwd: p.path.clone() }, false)).collect();
                            let sep = !items.is_empty();
                            items.push(item("Add project…".into(), Act::ImportProject, sep));
                            ws.open_dropdown(at, items, cx);
                        },
                    ))
                    .child(button("home-import", "Add project", false).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ImportProject, window, cx)))),
            )
            .child(recent)
            .child(
                div()
                    .mt(px(30.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(shortcut_row(&shortcut("Shift+N"), "New code session"))
                    .child(shortcut_row(&shortcut("J"), "Show or hide shell"))
                    .child(shortcut_row("Ctrl+Tab", "Next session"))
                    .child(shortcut_row(&shortcut("Shift+W"), "End session")),
            )
    }

    // ------------------------------------------------------------------ overlays

    fn render_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
                .h(px(28.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .rounded(px(5.0))
                .text_size(crate::panels::scale::px(12.5))
                .text_color(hsla(color))
                .gap(px(8.0))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(item.label.clone()))
                .children(item.hint.clone().map(|h| div().flex_none().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(h)));
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

    /// One labelled text field of the settings modal; `hint` is the default shown while it is empty.
    fn render_modal(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.modal.as_ref()?;
        let (title, body, confirm_label, danger): (String, gpui::AnyElement, String, bool) = match modal {
            Modal::Confirm { title, body, confirm, danger, .. } => (
                title.clone(),
                div().text_size(crate::panels::scale::px(13.0)).text_color(hsla(theme::TEXT_DIM)).child(body.clone()).into_any_element(),
                confirm.clone(),
                *danger,
            ),
            Modal::Settings { .. } => ("Settings".into(), settings_body(self.settings_page, modal, cx), "Save".into(), false),
            Modal::Advisor { input, .. } => (
                "Advisor model".into(),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(div().text_size(crate::panels::scale::px(12.0)).text_color(hsla(theme::TEXT_DIM)).child("A model alias or id, such as opus, sonnet or claude-opus-5-5, or “off”. Claude Code saves it as your default."))
                    .child(Input::new(input))
                    .into_any_element(),
                "Set".into(),
                false,
            ),
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
        let card = div()
            .id("modal-card")
            .occlude()
            .track_focus(&self.modal_focus)
            .when(matches!(modal, Modal::Settings { .. }), |c| {
                // One size for every page, growing with Scale.
                c.w(crate::panels::scale::px(720.0)).h(crate::panels::scale::px(480.0))
            })
            .when(!matches!(modal, Modal::Settings { .. }), |c| c.w(px(420.0)))
            .p(px(20.0))
            .rounded(px(12.0))
            .bg(hsla(0x151515))
            .border_1()
            .border_color(hsla(0x2c2c2c))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .capture_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| ws.settings_key(ev, window, cx)))
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
            );
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

    fn render_toast(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
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

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Window title follows the active session.
        let title = match self.active_id().and_then(|id| self.store.session(id)) {
            Some(s) => format!("{} — Claudiu", s.display_title()),
            None => "Claudiu".to_string(),
        };
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }

        // Editor mode hides both sidebars without changing the remembered layout, so leaving it brings them back.
        let left_open = self.left_reveal.step(self.store.data.layout.sidebar_visible && !self.editing(), window);
        let left_w = self.left_width(f32::from(window.viewport_size().width));
        let sidebar = (left_open > 0.0).then(|| {
            div()
                .flex()
                .flex_none()
                .w(px((left_w + theme::PANEL_GAP) * left_open))
                .h_full()
                .overflow_hidden()
                .child(self.render_sidebar(window, cx))
                .child(div().flex_none().w(px(theme::PANEL_GAP)))
        });
        let banner = self.render_update_banner(cx);
        let topbar = self.render_topbar(window, cx);
        // Terminals that aren't on screen keep running but must not trigger window redraws on every output burst.
        let showing = if matches!(self.main, Main::Terminals) { self.active.as_ref() } else { None };
        for l in &self.live {
            let visible = showing == Some(&l.id);
            l.view.update(cx, |v, _| v.visible = visible);
        }
        for (owner, p) in &self.shells {
            let visible = p.open && showing == Some(owner);
            p.view.update(cx, |v, _| v.visible = visible);
        }
        let right = self.render_right(f32::from(window.viewport_size().width), window, cx);
        let shell = self.render_shell(window, cx);
        let editor = self.render_editor_pane(cx);
        let window_w = f32::from(window.viewport_size().width);
        let main: AnyElement = match &self.main {
            Main::Terminals => self.render_session(cx),
            Main::Home => self.render_home(cx).into_any_element(),
        };
        let menu = self.render_menu(cx);
        let modal = self.render_modal(cx);
        let toast = self.render_toast(cx);
        // The top bar's own background is the gutter above the panels; only an update banner needs real space.
        let content_top = if banner.is_some() { theme::PANEL_GAP } else { 0.0 };

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.root_focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(theme::APP_BG))
            .text_color(hsla(theme::TEXT))
            .on_action(cx.listener(|ws, _: &NewClaude, window, cx| {
                let cwd = ws.new_session_cwd();
                ws.act(Act::NewClaude { cwd }, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &ToggleShell, window, cx| {
                if let Some(id) = ws.active.clone() {
                    ws.act(Act::ToggleShell(id), window, cx)
                }
            }))
            .on_action(cx.listener(|ws, _: &CloseSession, window, cx| {
                if let Some(id) = ws.active.clone() {
                    ws.act(Act::CloseLive(id), window, cx)
                }
            }))
            .on_action(cx.listener(|ws, _: &NextSession, window, cx| ws.cycle(1, window, cx)))
            .on_action(cx.listener(|ws, a: &crate::app::ShowSession, window, cx| ws.show_numbered(a.0, window, cx)))
            .on_modifiers_changed(cx.listener(|ws, ev: &gpui::ModifiersChangedEvent, _w, cx| ws.modifiers_changed(&ev.modifiers, cx)))
            .on_action(cx.listener(|ws, _: &PrevSession, window, cx| ws.cycle(-1, window, cx)))
            .on_action(cx.listener(|ws, _: &ToggleSidebar, _w, cx| ws.toggle_left(cx)))
            .on_action(cx.listener(|ws, _: &ToggleRight, window, cx| ws.toggle_right(window, cx)))
            .on_action(cx.listener(|ws, _: &FocusSearch, window, cx| {
                // In editor mode the search that fits is the session's files.
                if ws.editing() {
                    ws.open_picker(crate::app::picker::PickerKind::Text, window, cx);
                    return;
                }
                ws.store.data.layout.sidebar_visible = true;
                ws.search_input.update(cx, |s, cx| s.focus(window, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &ImportProject, window, cx| ws.act(Act::ImportProject, window, cx)))
            .on_action(cx.listener(|ws, _: &OpenGithub, window, cx| {
                let cwd = ws.active_cwd();
                ws.act(Act::OpenGithub { cwd }, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &OpenSettings, window, cx| ws.act(Act::OpenSettings, window, cx)))
            .on_action(cx.listener(|ws, _: &SwapFocus, window, cx| ws.swap_focus(window, cx)))
            .on_action(cx.listener(|ws, _: &ToggleEditor, window, cx| ws.act(Act::ToggleEditor, window, cx)))
            .on_mouse_move(cx.listener(|ws, ev: &gpui::MouseMoveEvent, window, cx| {
                if ws.shell_drag.is_none() && ws.right_drag.is_none() && ws.left_drag.is_none() {
                    return;
                }
                // The button was let go outside the window: stop following the pointer.
                if ev.pressed_button != Some(MouseButton::Left) {
                    ws.shell_drag = None;
                    ws.right_drag = None;
                    ws.left_drag = None;
                } else {
                    let size = window.viewport_size();
                    ws.drag_shell(f32::from(ev.position.y), f32::from(size.height));
                    ws.drag_right(f32::from(ev.position.x), f32::from(size.width));
                    ws.drag_left(f32::from(ev.position.x), f32::from(size.width));
                }
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|ws, _: &gpui::MouseUpEvent, _w, cx| {
                if ws.shell_drag.take().is_some() | ws.right_drag.take().is_some() | ws.left_drag.take().is_some() {
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|ws, paths: &gpui::ExternalPaths, window, cx| {
                let focused = ws.active.as_ref().and_then(|id| ws.live_view(id)).cloned();
                if let (Some(view), Main::Terminals) = (focused, &ws.main) {
                    // Over a terminal, files go to the session as paths (Claude Code attaches images).
                    view.update(cx, |v, cx| v.paste_paths(paths.paths(), cx));
                    return;
                }
                for p in paths.paths().iter().filter(|p| p.is_dir()) {
                    ws.act(Act::AddProject(p.clone()), window, cx);
                }
            }))
            .child(topbar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(theme::PANEL_GAP))
                    .px(px(theme::PANEL_GAP))
                    .pt(px(content_top))
                    .pb(px(theme::PANEL_GAP))
                    .children(banner)
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .children(sidebar)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .h_full()
                                    // In editor mode the terminal narrows to the left and the editor takes the rest.
                                    .map(|d| match editor.is_some() {
                                        true => d.flex_none().w(px((window_w * 0.36).clamp(380.0, 640.0))),
                                        false => d.flex_1().min_w_0(),
                                    })
                                    .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_h_0()
                                        .w_full()
                                        .rounded(px(theme::PANEL_RADIUS))
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(hsla(theme::PANEL_BORDER))
                                        .bg(hsla(theme::PANEL_BG))
                                        .child(main),
                                )
                                .children(shell),
                            )
                            .when_some(editor, |d, editor| d.child(div().flex_none().w(px(theme::PANEL_GAP))).child(editor))
                            .children(right),
                    ),
            )
            .children(menu)
            .children(modal)
            .children(toast)
    }
}

/// The Settings dialog's body: the category list on the left and the chosen page on the right.
/// `toggles` are Check for updates, Background compaction and Claude in Chrome, in that order.
fn settings_body(page: SettingsPage, modal: &Modal, cx: &mut Context<Workspace>) -> AnyElement {
    let Modal::Settings { shell, claude, model, effort, permission, updates, background, chrome, scale, trimmed, claudiu_off, list_focus } = modal else {
        return div().into_any_element();
    };
    let (updates, background, chrome, scale) = (*updates, *background, *chrome, *scale);
    let scaled = crate::panels::scale::px;
    let faint = |text: String| div().text_size(scaled(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(text);
    let field = |label: &str, input: &Entity<InputState>| {
        div().flex().flex_col().gap(px(4.0)).child(section_label(label.to_string())).child(Input::new(input))
    };
    let content = match page {
        SettingsPage::General => div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(field("SHELL", shell))
            .child(field("CLAUDE CODE PATH", claude))
            .child(
                div()
                    .h(px(32.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .rounded(px(6.0))
                    .bg(hsla(0x111111))
                    .border_1()
                    .border_color(hsla(0x242424))
                    .text_size(px(13.0))
                    .child("Scale (text and terminal)")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(scale_step("scale-down", "−", -(crate::panels::scale::STEP as i32), cx))
                            .child(div().w(px(40.0)).flex().justify_center().child(format!("{scale}%")))
                            .child(scale_step("scale-up", "+", crate::panels::scale::STEP as i32, cx)),
                    ),
            )
            .child(settings_toggle("settings-updates", "Check for updates", updates, |m| match m {
                Modal::Settings { updates, .. } => Some(updates),
                _ => None,
            }, cx)),
        SettingsPage::Sessions => div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(field("MODEL FOR NEW SESSIONS", model))
            .child(picker_row("settings-effort", "Effort", *effort, &Effort::ALL, Effort::name, Act::PickEffort, cx))
            .child(picker_row("settings-permission", "Permission mode", *permission, &PermissionMode::ALL, PermissionMode::name, Act::PickPermission, cx))
            .children((*permission == Some(PermissionMode::BypassPermissions)).then(|| faint("Every new session will run commands and edit files without asking.".to_string())))
            .children((*permission == Some(PermissionMode::Plan) && trimmed.contains(&Trim::PlanMode)).then(|| {
                faint("Plan mode is off under Claude Code tools, so new sessions start in Claude Code's default mode.".to_string())
            }))
            .child(settings_toggle("settings-chrome", "Claude in Chrome for new sessions", chrome, |m| match m {
                Modal::Settings { chrome, .. } => Some(chrome),
                _ => None,
            }, cx))
            .child(settings_toggle("settings-background-compact", "Background compaction", background, |m| match m {
                Modal::Settings { background, .. } => Some(background),
                _ => None,
            }, cx))
            .child(faint(
                "Drafts the compaction summary before the context fills, so auto-compact doesn't pause the session.                  Used where Claude Code doesn't offer its own; an unused draft still costs tokens."
                    .to_string(),
            )),
        SettingsPage::ClaudeTools => {
            let plan_trimmed = trimmed.contains(&Trim::PlanMode);
            let mut list = div().flex().flex_col().gap(px(8.0));
            list = list.child(faint(
                "Applies to new sessions. Running sessions keep their tools. Resumed sessions get the new tools, and the old instructions stay until their next compaction.                  Claudiu only turns things off. Anything off in your Claude Code settings stays off."
                    .to_string(),
            ));
            for (heading, items) in Trim::GROUPS {
                list = list.child(div().mt(px(4.0)).child(section_label(heading.to_string())));
                for &item in items {
                    list = list.child(switch_row(("settings-trim", item as usize), item.label(), !trimmed.contains(&item), move |ws| {
                        if let Some(Modal::Settings { trimmed, .. }) = ws.modal.as_mut() {
                            flip(trimmed, item);
                        }
                    }, cx));
                }
            }
            if plan_trimmed {
                list = list.child(faint("With Plan mode off, Claude can't leave plan mode itself; press Shift+Tab.".to_string()));
            }
            list
        }
        SettingsPage::ClaudiuTools => {
            let mut list = div().flex().flex_col().gap(px(8.0)).child(faint(
                "Applies to new sessions. Running sessions keep their tools. Resumed sessions get the new tools, and the old instructions stay until their next compaction."
                    .to_string(),
            ));
            for feature in Feature::ALL {
                list = list.child(switch_row(("settings-claudiu", feature as usize), feature.label(), !claudiu_off.contains(&feature), move |ws| {
                    if let Some(Modal::Settings { claudiu_off, .. }) = ws.modal.as_mut() {
                        flip(claudiu_off, feature);
                    }
                }, cx));
            }
            list
        }
        SettingsPage::About => div().child(faint(format!(
            "Claudiu {} · An independent project, not affiliated with, endorsed by or sponsored by Anthropic. Claude is a trademark of Anthropic.",
            env!("CARGO_PKG_VERSION")
        ))),
    };
    let categories = SettingsPage::ALL.into_iter().enumerate().fold(
        div().id("settings-categories").track_focus(list_focus).w(scaled(160.0)).flex_none().flex().flex_col().gap(px(2.0)),
        |list, (i, p)| {
            let selected = p == page;
            let focus = list_focus.clone();
            list.child(
                div()
                    .id(("settings-category", i))
                    .h(scaled(28.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(6.0))
                    .text_size(scaled(13.0))
                    .text_color(hsla(if selected { theme::ACCENT } else { theme::TEXT_DIM }))
                    .when(selected, |d| d.bg(hsla(0x1f1f1f)))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(0x1f1f1f)))
                    .child(p.label())
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        ws.settings_page = p;
                        window.focus(&focus, cx);
                        cx.notify();
                    })),
            )
        },
    );
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .gap(px(16.0))
        .child(categories)
        .child(div().id("settings-page").flex_1().min_w_0().overflow_y_scroll().child(content))
        .into_any_element()
}

/// An On/Off row in the Settings modal; `field` picks the flag it flips.
fn settings_toggle(id: &'static str, label: &'static str, on: bool, field: fn(&mut Modal) -> Option<&mut bool>, cx: &mut Context<Workspace>) -> impl IntoElement {
    switch_row(id, label, on, move |ws| {
        if let Some(flag) = ws.modal.as_mut().and_then(field) {
            *flag = !*flag;
        }
    }, cx)
}

/// A row that opens a menu of `options` under itself; the first entry, "Claude Code default", clears the choice.
fn picker_row<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: &'static str,
    current: Option<T>,
    options: &'static [T],
    name: fn(T) -> &'static str,
    pick: fn(Option<T>) -> Act,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    const DEFAULT: &str = "Claude Code default";
    div()
        .id(id)
        .h(px(32.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(6.0))
        .bg(hsla(0x111111))
        .border_1()
        .border_color(hsla(0x242424))
        .text_size(crate::panels::scale::px(13.0))
        .cursor_pointer()
        .child(label)
        .child(div().text_color(hsla(if current.is_some() { theme::ACCENT } else { theme::TEXT_FAINT })).child(current.map_or(DEFAULT, name)))
        .dropdown(cx, move |ws, at, _window, cx| {
            let items = std::iter::once(MenuItem::new(format!("{}{DEFAULT}", tick(current.is_none())), pick(None)))
                .chain(options.iter().map(|&o| MenuItem::new(format!("{}{}", tick(current == Some(o)), name(o)), pick(Some(o)))))
                .collect();
            ws.open_dropdown(at, items, cx);
        })
}

/// Adds `item` to `items`, or removes it when it is there.
fn flip<T: PartialEq>(items: &mut Vec<T>, item: T) {
    match items.iter().position(|i| *i == item) {
        Some(at) => {
            items.remove(at);
        }
        None => items.push(item),
    }
}

/// An On/Off row that runs `flip` on the workspace when clicked.
fn switch_row(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    on: bool,
    flip: impl Fn(&mut Workspace) + 'static,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(32.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(6.0))
        .bg(hsla(0x111111))
        .border_1()
        .border_color(hsla(0x242424))
        .text_size(crate::panels::scale::px(13.0))
        .cursor_pointer()
        .child(label)
        .child(div().text_color(hsla(if on { theme::ACCENT } else { theme::TEXT_FAINT })).child(if on { "On" } else { "Off" }))
        .on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
            flip(ws);
            cx.notify();
        }))
}

/// A − / + button in the Settings modal that moves the scale by `by` percent, within the allowed range.
fn scale_step(id: &'static str, label: &'static str, by: i32, cx: &mut Context<Workspace>) -> impl IntoElement {
    div().id(id).w(px(22.0)).h(px(22.0)).flex().items_center().justify_center().rounded(px(4.0)).cursor_pointer().hover(|s| s.bg(hsla(0x242424))).child(label).on_click(
        cx.listener(move |ws, _: &ClickEvent, _w, cx| {
            if let Some(Modal::Settings { scale, .. }) = ws.modal.as_mut() {
                *scale = (*scale as i32 + by).clamp(crate::panels::scale::MIN as i32, crate::panels::scale::MAX as i32) as u32;
            }
            cx.notify();
        }),
    )
}
