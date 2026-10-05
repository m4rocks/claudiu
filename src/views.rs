//! Main area (terminal panes / session details / home), overlays (menu, modal, toast), and the root `Render`.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, prelude::FluentBuilder, px, rgba,
};

use crate::app::{
    Act, CloseSession, FocusNextPane, FocusSearch, ImportProject, Main, Modal, NewClaude, NewShell, NextSession, PrevSession, SplitPane,
    ToggleSidebar, Workspace,
};
use crate::platform::Editor;
use crate::sidebar::editor_available;
use crate::store::{SessionKind, now};
use crate::theme::{self, hsla};
use crate::updater::Status;
use crate::widgets::{ago, button, clawd, danger_button, icon_button, section_label, short_path};

impl Workspace {
    // ------------------------------------------------------------------ top bar

    fn render_topbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rec = self.active_id().and_then(|id| self.store.session(id)).cloned();
        let project = rec.as_ref().and_then(|r| r.project_id.as_ref()).and_then(|p| self.store.project(p)).cloned();
        let cwd = rec.as_ref().map(|r| r.cwd.clone());
        let in_terminals = matches!(self.main, Main::Terminals);

        let mut right = div().flex().items_center().gap(px(6.0));
        if in_terminals {
            right = right.child(
                icon_button("split", "◫").on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.split(window, cx))),
            );
            if self.panes.len() > 1 {
                right = right.child(
                    icon_button("orient", if self.split_vertical { "▤" } else { "▥" }).on_click(cx.listener(|ws, _: &ClickEvent, _w, cx| {
                        ws.split_vertical = !ws.split_vertical;
                        cx.notify();
                    })),
                );
            }
        }
        if let Some(path) = cwd.clone() {
            let target = project.as_ref().map(|p| p.path.clone()).unwrap_or(path);
            for editor in Editor::ALL {
                if editor_available(self, editor) {
                    let t = target.clone();
                    right = right.child(
                        button(
                            gpui::SharedString::from(format!("editor-{:?}", editor)),
                            match editor {
                                Editor::VsCode => "VS Code",
                                Editor::Zed => "Zed",
                            },
                            false,
                        )
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::OpenEditor(editor, t.clone()), window, cx))),
                    );
                }
            }
            let t = target.clone();
            right = right.child(
                button("reveal", if cfg!(target_os = "macos") { "Finder" } else { "Folder" }, false)
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Reveal(t.clone()), window, cx))),
            );
        }

        let title = rec.as_ref().map(|r| r.display_title().to_string()).unwrap_or_else(|| "Claudiu".into());
        let branch = project.as_ref().and_then(|p| p.git.as_ref()).and_then(|g| g.branch.clone());
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(40.0))
            .flex_none()
            .px(px(14.0))
            .border_b_1()
            .border_color(hsla(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .min_w_0()
                    .child(
                        icon_button("toggle-sidebar", "☰")
                            .on_click(cx.listener(|ws, _: &ClickEvent, _w, cx| {
                                ws.store.data.layout.sidebar_visible = !ws.store.data.layout.sidebar_visible;
                                ws.store.mark_dirty();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .when_some(cwd, |d, p| {
                        d.child(div().text_size(px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(short_path(&p)))
                    })
                    .when_some(branch, |d, b| {
                        d.child(
                            div()
                                .px(px(7.0))
                                .py(px(1.0))
                                .rounded(px(10.0))
                                .bg(hsla(0x1a1a1a))
                                .text_size(px(11.0))
                                .text_color(hsla(theme::TEXT_DIM))
                                .child(format!("⎇ {b}")),
                        )
                    }),
            )
            .child(right)
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
                .h(px(32.0))
                .flex_none()
                .px(px(14.0))
                .bg(hsla(0x1d1512))
                .border_b_1()
                .border_color(hsla(0x3a2a24))
                .child(div().text_size(px(12.0)).text_color(hsla(theme::ACCENT)).child(text))
                .when_some(action, |d, (label, act)| {
                    d.child(button("update-action", label, true).h(px(22.0)).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        ws.act(act.clone(), window, cx)
                    })))
                })
                .into_any_element(),
        )
    }

    // ------------------------------------------------------------------ panes

    fn render_panes(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let n = self.panes.len();
        let mut container = div().flex().size_full().gap(px(1.0)).bg(hsla(theme::BORDER));
        container = if self.split_vertical { container.flex_col() } else { container.flex_row() };

        for (i, id) in self.panes.clone().into_iter().enumerate() {
            let Some(view) = self.live_view(&id).cloned() else { continue };
            let rec = self.store.session(&id).cloned();
            let exit = view.read(cx).exit_code;
            let focused = i == self.focused;
            let title = rec.as_ref().map(|r| r.display_title().to_string()).unwrap_or_default();
            let is_claude = rec.as_ref().is_some_and(|r| r.kind == SessionKind::Claude);
            let cwd = rec.as_ref().map(|r| r.cwd.clone());
            let (rid, cid) = (id.clone(), id.clone());

            let header = (n > 1).then(|| {
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(26.0))
                    .flex_none()
                    .px(px(10.0))
                    .bg(hsla(theme::APP_BG))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(hsla(if focused { theme::TEXT } else { theme::TEXT_DIM }))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(title.clone()),
                    )
                    .child(icon_button(gpui::SharedString::from(format!("pane-x-{i}")), "×").on_click(cx.listener(
                        move |ws, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            ws.act(Act::CloseLive(cid.clone()), window, cx)
                        },
                    )))
            });

            let banner = exit.map(|code| {
                let label = match code {
                    Some(0) | None => "Session ended".to_string(),
                    Some(c) => format!("Session ended (exit code {c})"),
                };
                let primary = if is_claude { "Resume" } else { "New shell here" };
                let act = if is_claude {
                    Act::Resume(rid.clone())
                } else {
                    Act::NewShell { cwd: cwd.clone().unwrap_or_default() }
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
                    .child(div().text_size(px(12.5)).text_color(hsla(theme::TEXT_DIM)).child(label))
                    .child(div().flex().gap(px(6.0)).child(
                        button(gpui::SharedString::from(format!("ended-act-{i}")), primary, true).on_click(cx.listener(
                            move |ws, _: &ClickEvent, window, cx| ws.act(act.clone(), window, cx),
                        )),
                    ))
            });

            container = container.child(
                div()
                    .id(gpui::SharedString::from(format!("pane-{i}")))
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .bg(hsla(theme::TERMINAL_BG))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                            if ws.focused != i {
                                ws.focus_pane(i, window, cx);
                            }
                        }),
                    )
                    .children(header)
                    .child(div().flex_1().min_h_0().p(px(8.0)).child(view))
                    .children(banner),
            );
        }
        container
    }

    // ------------------------------------------------------------------ details & home

    fn render_home(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cwd = self.active_cwd();
        let (c1, c2) = (cwd.clone(), cwd);
        let shortcut = |keys: &str, what: &str| {
            div()
                .flex()
                .justify_between()
                .gap(px(28.0))
                .child(div().text_size(px(12.0)).text_color(hsla(theme::TEXT_DIM)).child(what.to_string()))
                .child(div().text_size(px(12.0)).text_color(hsla(theme::TEXT_FAINT)).child(keys.to_string()))
        };
        let mut recent = div().flex().flex_col().gap(px(2.0)).mt(px(26.0)).w(px(420.0));
        let mut sessions: Vec<_> = self.store.data.sessions.iter().filter(|s| s.kind == SessionKind::Claude && !s.hidden).cloned().collect();
        sessions.sort_by(|a, b| b.last_active.cmp(&a.last_active));
        let t = now();
        if !sessions.is_empty() {
            recent = recent.child(div().mb(px(4.0)).child(section_label("RECENT")));
        }
        for (i, s) in sessions.into_iter().take(5).enumerate() {
            let id = s.id.clone();
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
                    .child(div().text_size(px(13.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(s.display_title().to_string()))
                    .child(div().text_size(px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(ago(s.last_active, t)))
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
            .child(div().mt(px(20.0)).text_size(px(22.0)).font_weight(FontWeight::SEMIBOLD).child("Claudiu"))
            .child(div().mt(px(4.0)).text_size(px(13.0)).text_color(hsla(theme::TEXT_DIM)).child("A workspace for your Claude Code sessions"))
            .child(
                div()
                    .mt(px(22.0))
                    .flex()
                    .gap(px(8.0))
                    .child(button("home-claude", "New Claude session", true).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        ws.act(Act::NewClaude { cwd: c1.clone() }, window, cx)
                    })))
                    .child(button("home-shell", "New shell", false).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        ws.act(Act::NewShell { cwd: c2.clone() }, window, cx)
                    })))
                    .child(button("home-import", "Add project", false).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ImportProject, window, cx)))),
            )
            .child(recent)
            .child(
                div()
                    .mt(px(30.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(shortcut("Ctrl+Shift+N", "New Claude session"))
                    .child(shortcut("Ctrl+Shift+T", "New shell"))
                    .child(shortcut("Ctrl+Shift+D", "Split pane"))
                    .child(shortcut("Ctrl+Tab", "Next session"))
                    .child(shortcut("Ctrl+Shift+W", "End session")),
            )
    }

    // ------------------------------------------------------------------ overlays

    fn render_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let pos = menu.pos;
        let mut card = div()
            .id("menu")
            .occlude()
            .w(px(232.0))
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
            card = card.child(
                div()
                    .id(gpui::SharedString::from(format!("menu-{i}")))
                    .h(px(28.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(5.0))
                    .text_size(px(12.5))
                    .text_color(hsla(if item.danger { theme::DANGER } else { theme::TEXT }))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(0x252525)))
                    .child(item.label.clone())
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(act.clone(), window, cx))),
            );
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
                    .child(anchored().position(pos).snap_to_window_with_margin(px(8.0)).child(card)),
            )
            .with_priority(10)
            .into_any_element(),
        )
    }

    fn render_modal(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.modal.as_ref()?;
        let (title, body, confirm_label, danger): (String, gpui::AnyElement, String, bool) = match modal {
            Modal::Confirm { title, body, confirm, danger, .. } => (
                title.clone(),
                div().text_size(px(13.0)).text_color(hsla(theme::TEXT_DIM)).child(body.clone()).into_any_element(),
                confirm.clone(),
                *danger,
            ),
            Modal::Rename { text, .. } => (
                "Rename session".into(),
                div()
                    .h(px(32.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(6.0))
                    .bg(hsla(0x111111))
                    .border_1()
                    .border_color(hsla(0x3a2a24))
                    .text_size(px(13.0))
                    .child(text.clone())
                    .child(div().w(px(1.0)).h(px(15.0)).ml(px(1.0)).bg(hsla(theme::ACCENT)))
                    .into_any_element(),
                "Rename".into(),
                false,
            ),
        };
        let card = div()
            .id("modal-card")
            .occlude()
            .track_focus(&self.modal_focus)
            .w(px(420.0))
            .p(px(20.0))
            .rounded(px(12.0))
            .bg(hsla(0x151515))
            .border_1()
            .border_color(hsla(0x2c2c2c))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .on_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| {
                let key = ev.keystroke.key.as_str();
                match (key, ws.modal.as_mut()) {
                    ("escape", _) => ws.act(Act::Dismiss, window, cx),
                    ("enter", Some(Modal::Confirm { act, .. })) => {
                        let act = act.clone();
                        ws.act(act, window, cx);
                    }
                    ("enter", Some(Modal::Rename { id, text })) => {
                        let (id, text) = (id.clone(), text.trim().to_string());
                        if let Some(rec) = ws.store.session_mut(&id) {
                            rec.custom_title = if text.is_empty() { None } else { Some(text) };
                        }
                        ws.act(Act::Dismiss, window, cx);
                    }
                    (_, Some(Modal::Rename { text, .. })) => {
                        crate::widgets::edit_text(text, &ev.keystroke, cx);
                        cx.notify();
                    }
                    _ => {}
                }
            }))
            .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(title))
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
                        b.on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                            let act = match ws.modal.as_ref() {
                                Some(Modal::Confirm { act, .. }) => Some(act.clone()),
                                Some(Modal::Rename { id, text }) => {
                                    let (id, text) = (id.clone(), text.trim().to_string());
                                    if let Some(rec) = ws.store.session_mut(&id) {
                                        rec.custom_title = if text.is_empty() { None } else { Some(text) };
                                    }
                                    Some(Act::Dismiss)
                                }
                                None => None,
                            };
                            if let Some(act) = act {
                                ws.act(act, window, cx);
                            }
                        }))
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

    fn render_toast(&self) -> Option<AnyElement> {
        let t = self.toast.as_ref()?;
        Some(
            deferred(
                div().absolute().bottom(px(18.0)).right(px(18.0)).child(
                    div()
                        .max_w(px(420.0))
                        .px(px(14.0))
                        .py(px(10.0))
                        .rounded(px(8.0))
                        .bg(hsla(0x1a1a1a))
                        .border_1()
                        .border_color(hsla(if t.error { 0x5a2327 } else { 0x2c2c2c }))
                        .text_size(px(12.5))
                        .text_color(hsla(if t.error { theme::DANGER } else { theme::TEXT }))
                        .child(t.text.clone()),
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

        let sidebar = self.store.data.layout.sidebar_visible.then(|| self.render_sidebar(window, cx));
        let banner = self.render_update_banner(cx);
        let topbar = self.render_topbar(cx);
        let main: AnyElement = match &self.main {
            Main::Terminals => self.render_panes(cx).into_any_element(),
            Main::Home => self.render_home(cx).into_any_element(),
        };
        let menu = self.render_menu(cx);
        let modal = self.render_modal(cx);
        let toast = self.render_toast();

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.root_focus)
            .relative()
            .size_full()
            .flex()
            .bg(hsla(theme::TERMINAL_BG))
            .text_color(hsla(theme::TEXT))
            .on_action(cx.listener(|ws, _: &NewClaude, window, cx| {
                let cwd = ws.active_cwd();
                ws.act(Act::NewClaude { cwd }, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &NewShell, window, cx| {
                let cwd = ws.active_cwd();
                ws.act(Act::NewShell { cwd }, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &CloseSession, window, cx| {
                if let Some(id) = ws.panes.get(ws.focused).cloned() {
                    ws.act(Act::CloseLive(id), window, cx)
                }
            }))
            .on_action(cx.listener(|ws, _: &NextSession, window, cx| ws.cycle(1, window, cx)))
            .on_action(cx.listener(|ws, _: &PrevSession, window, cx| ws.cycle(-1, window, cx)))
            .on_action(cx.listener(|ws, _: &SplitPane, window, cx| ws.split(window, cx)))
            .on_action(cx.listener(|ws, _: &ToggleSidebar, _w, cx| {
                ws.store.data.layout.sidebar_visible = !ws.store.data.layout.sidebar_visible;
                ws.store.mark_dirty();
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &FocusSearch, window, cx| {
                ws.store.data.layout.sidebar_visible = true;
                window.focus(&ws.search_focus);
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &ImportProject, window, cx| ws.act(Act::ImportProject, window, cx)))
            .on_action(cx.listener(|ws, _: &FocusNextPane, window, cx| {
                if !ws.panes.is_empty() {
                    let next = (ws.focused + 1) % ws.panes.len();
                    ws.focus_pane(next, window, cx);
                }
            }))
            .on_drop(cx.listener(|ws, paths: &gpui::ExternalPaths, window, cx| {
                for p in paths.paths().iter().filter(|p| p.is_dir()) {
                    ws.act(Act::AddProject(p.clone()), window, cx);
                }
            }))
            .children(sidebar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(topbar)
                    .children(banner)
                    .child(div().flex_1().min_h_0().child(main)),
            )
            .children(menu)
            .children(modal)
            .children(toast)
    }
}
