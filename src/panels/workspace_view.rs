//! The root `Render`: the top bar, the update banner, the shown session and its shell panel, and where the sidebars,
//! the Welcome screen (`home`), the overlays (`overlays`) and the dropdowns (`workspace_pickers`) go.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px,
};

use crate::claude::defaults::Effort;
use crate::app::{
    Act, CloseSession, FocusSearch, GitOp, GitPickerTarget, ImportProject, Main, NewClaude, NextSession, OpenGithub, OpenSettings, PrevSession, SessionView, ShellDrag, SwapFocus, ToggleEditor, ToggleRight, ToggleShell, ToggleSidebar, Workspace, WorkspacePicker, SHELL_HEIGHT,
};
use gpui_kit::component::TitleBar;
use crate::panels::theme::{self, hsla};
use crate::updater::Status;
use gpui_kit::assets::IconName;
use crate::panels::widgets::{
    button, fade_in, icon_button, icon_button_disabled, short_path,
};

impl Workspace {
    // ------------------------------------------------------------------ top bar

    fn render_topbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let crate::panels::inspector::Shown { rec, cwd, .. } = self.shown();
        let editor_available = self.editor_owner().is_some();

        let title = rec.as_ref().map(|r| r.display_title().to_string()).unwrap_or_else(|| "m4code".into());
        // `.occlude()` on both groups: the bar is a Windows drag (HTCAPTION) area, which would otherwise swallow clicks on the buttons inside it.
        // The top bar spans the window and is drawn as the window background, not as a panel. macOS keeps its
        // traffic lights at the left (they are hidden in fullscreen); Windows/Linux get min/max/close at the right.
        let left_pad = if cfg!(target_os = "macos") && !window.is_fullscreen() { 80.0 } else { 10.0 };
        TitleBar::new()
            // Linux draws its own close button, which would otherwise drop the window without asking about working
            // sessions or saving the layout. Going through Quit is the same path as Ctrl+Q and the OS close.
            .on_close_window(|_, window, cx| window.dispatch_action(Box::new(crate::app::Quit), cx))
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
                        d.child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(crate::panels::scale::px(11.5))
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(short_path(&p)),
                        )
                    })
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .occlude()
                    .child(if editor_available {
                        icon_button("toggle-editor", IconName::FileCode).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleEditor, window, cx)))
                    } else {
                        icon_button_disabled("toggle-editor", IconName::FileCode)
                    })
                    .child(icon_button("toggle-shell", IconName::PanelBottom).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.toggle_shell_here(window, cx))))
                    .child(icon_button("toggle-right", IconName::PanelRight).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.toggle_right(window, cx)))),
            ))
    }

    fn render_update_banner(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (text, action) = match &self.update.status {
            Status::Available { version } => (format!("m4code {version} is available"), Some(("Download", Act::InstallUpdate))),
            Status::Downloading => ("Downloading update…".to_string(), None),
            Status::Ready { version } => (format!("m4code {version} is ready"), Some(("Restart to update", Act::InstallUpdate))),
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

    /// Keep the shown session's branch chip in step with its folder, git work and the open dialog.
    fn sync_git_chip(&mut self, cx: &mut Context<Self>) {
        let crate::panels::inspector::Shown { rec, cwd, project } = self.shown();
        let Some(thread) = rec.as_ref().and_then(|r| self.live_view(&r.id)).and_then(|v| v.thread()).cloned() else { return };
        let chip = project.as_ref().zip(cwd.as_deref()).and_then(|(p, c)| {
            let g = p.git.as_ref()?;
            let linked = (!g.is_main(c)).then(|| crate::git::worktree::label(g, c)).flatten();
            let primary = g.branch_at(c).map(str::to_string).or_else(|| crate::git::worktree::label(g, c))?;
            Some(crate::thread::GitChip {
                primary,
                linked,
                busy: matches!(self.git_busy, Some(GitOp::Branch | GitOp::Worktree)),
                open: matches!(self.git_picker(), Some(gp) if gp.target == GitPickerTarget::Session),
            })
        });
        if thread.read(cx).git != chip {
            thread.update(cx, |t, cx| {
                t.git = chip;
                cx.notify();
            });
        }
    }

    fn render_session(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.sync_git_chip(cx);
        let blank = || div().size_full().bg(hsla(theme::TERMINAL_BG)).into_any_element();
        let Some(id) = self.active.clone() else { return blank() };
        let Some(view) = self.live_view(&id).cloned() else { return blank() };
        let term = match view {
            SessionView::Thread(v) => v.into_any_element(),
            SessionView::Terminal(v) => v.into_any_element(),
        };
        let strip = self.subagent_strip(&id, cx);

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
                .children(strip.map(|strip| div().flex_none().px(px(6.0)).pt(px(6.0)).child(strip)))
                .child(div().flex_1().min_h_0().p(px(6.0)).child(term)),
            format!("pane-in-{id}"),
            150,
            0.4,
        );
        pane.into_any_element()
    }

    /// Above a subagent's chat: the way back to its parent, then which agent this is and where it stands, so its chat
    /// is never mistaken for the parent's.
    fn subagent_strip(&self, id: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let record = self.store.session(id)?;
        let meta = record.claude.as_ref()?;
        let parent_id = meta.parent_session.clone()?;
        let parent = self.store.session(&parent_id).map(|p| p.display_title().to_string());
        let state = self.subagent_state(id, cx);
        let ellipsis = |d: gpui::Div| d.min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis();
        let back = div()
            .id("subagent-back")
            .flex()
            .items_center()
            .gap(px(6.0))
            .min_w_0()
            .max_w(gpui::relative(0.45))
            .h(px(24.0))
            .px(px(6.0))
            .rounded(px(6.0))
            .text_color(hsla(theme::TEXT_DIM))
            .child(crate::panels::widgets::icon(IconName::ArrowLeft, 13.0))
            .child(ellipsis(div()).child(parent.clone().unwrap_or_else(|| "Parent session".into())))
            .when(parent.is_some(), |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(hsla(theme::ROW_HOVER)).text_color(hsla(theme::TEXT)))
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Select(parent_id.clone()), window, cx)))
            });
        let failed = state == crate::app::SubagentState::Failed;
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .h(px(32.0))
                .px(px(4.0))
                .rounded(px(theme::PANEL_RADIUS))
                .border_1()
                .border_color(hsla(theme::PANEL_BORDER))
                .bg(hsla(theme::PANEL_BG))
                .text_size(crate::panels::scale::px(12.0))
                .child(back)
                .child(div().flex_none().text_color(hsla(theme::TEXT_FAINT)).child("/"))
                .child(crate::panels::logos::draw(crate::panels::logos::for_harness(meta.harness), 12.0, hsla(theme::TEXT_DIM)))
                .child(ellipsis(div().flex_1()).text_color(hsla(theme::TEXT)).child(record.display_title().to_string()))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .pr(px(6.0))
                        .text_color(hsla(if failed { theme::DANGER } else { theme::TEXT_FAINT }))
                        .child(crate::panels::widgets::subagent_state_icon(state, 12.0))
                        .child(state.label()),
                )
                .into_any_element(),
        )
    }

    /// The session's shell as its own panel under the main one: same gutter, rounding, grip and slide as the sidebars.
    fn render_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = self.shell_owner();
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
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Window title follows the active session.
        let title = match self.active_id().and_then(|id| self.store.session(id)) {
            Some(s) => format!("{} — m4code", s.display_title()),
            None => "m4code".to_string(),
        };
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }
        self.remember_window(window);

        // Editor mode hides both sidebars without changing the remembered layout, so leaving it brings them back.
        self.start_spin(window, cx);
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
            if let Some(view) = l.view.terminal() {
                view.update(cx, |v, _| {
                    v.visible = visible;
                    v.foreground = window.is_window_active();
                });
            }
        }
        let shell_owner = self.shell_owner();
        for (owner, p) in &self.shells {
            let visible = p.open && *owner == shell_owner;
            p.view.update(cx, |v, _| {
                v.visible = visible;
                v.foreground = window.is_window_active();
            });
        }
        let right = self.render_right(f32::from(window.viewport_size().width), window, cx);
        let shell = self.render_shell(window, cx);
        let editor = self.render_editor_pane(window, cx);
        let window_w = f32::from(window.viewport_size().width);
        let main: AnyElement = match &self.main {
            Main::Terminals => self.render_session(cx),
            Main::Home => self.render_home(window, cx).into_any_element(),
        };
        let menu = self.render_menu(cx);
        let picker_dialog = self.render_picker(window, cx);
        let modal = self.render_modal(window, cx);
        let agent_dialog = self.render_harness_dialog(window, cx);
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
            .capture_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| {
                let m = &ev.keystroke.modifiers;
                let plain = !m.control && !m.alt && !m.platform && !m.shift && !m.function;
                if ev.keystroke.key == "escape" && plain && ws.picker.is_some() {
                    ws.dismiss_picker(window, cx);
                    cx.stop_propagation();
                    return;
                }
                // Alt+P / Option+P toggles the Welcome model picker when on the New Session page.
                if ev.keystroke.key == "p" && m.alt && !m.control && !m.platform && !m.shift && matches!(ws.main, Main::Home) && ws.modal.is_none() {
                    if matches!(ws.picker_kind(), Some(WorkspacePicker::HomeModel { .. })) {
                        ws.dismiss_picker(window, cx);
                    } else {
                        let at = ws.home.model_anchor.below();
                        ws.open_home_model_picker(at, window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                // Keyboard navigation inside the Welcome model picker: Up/Down moves model, Left/Right steps effort, Enter confirms.
                if plain && let Some(WorkspacePicker::HomeModel { search }) = ws.picker_kind() {
                    let query = search.read(cx).value().to_lowercase();
                    let words: Vec<&str> = query.split_whitespace().collect();
                    let filtered: Vec<crate::harness::CatalogModel> = ws
                        .store
                        .data
                        .settings
                        .aggregated_models()
                        .into_iter()
                        .filter(|m| {
                            let hay = format!("{} {} {}", m.name, m.id, m.harness.name()).to_lowercase();
                            words.iter().all(|w| hay.contains(w))
                        })
                        .collect();
                    match ev.keystroke.key.as_str() {
                        "up" | "down" => {
                            if !filtered.is_empty() {
                                let cur = ws.home.model_highlight.min(filtered.len() - 1);
                                ws.home.model_highlight = if ev.keystroke.key == "up" {
                                    if cur == 0 { filtered.len() - 1 } else { cur - 1 }
                                } else {
                                    (cur + 1) % filtered.len()
                                };
                                ws.home.model_scroll.scroll_to_item(ws.home.model_highlight);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }
                        "left" | "right" => {
                            let cur = Effort::ALL.iter().position(|e| *e == ws.home.effort).unwrap_or(1) as isize;
                            let delta = if ev.keystroke.key == "left" { -1 } else { 1 };
                            let next = (cur + delta).clamp(0, Effort::ALL.len() as isize - 1) as usize;
                            ws.act(Act::HomeSetEffort(Effort::ALL[next]), window, cx);
                            cx.stop_propagation();
                        }
                        "enter" => {
                            if let Some(choice) = filtered.get(ws.home.model_highlight.min(filtered.len().saturating_sub(1))).cloned() {
                                ws.act(Act::HomeSelectModel(choice), window, cx);
                            }
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }
            }))
            .on_action(cx.listener(|ws, _: &NewClaude, window, cx| {
                let cwd = ws.new_session_cwd();
                ws.act(Act::NewClaude { cwd }, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &ToggleShell, window, cx| ws.toggle_shell_here(window, cx)))
            .on_action(cx.listener(|ws, _: &crate::app::ReopenClosed, window, cx| {
                ws.reopen_closed(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &CloseSession, window, cx| {
                if let Some(id) = ws.active.clone() {
                    ws.act(Act::CloseLive(id), window, cx)
                }
            }))
            .on_action(cx.listener(|ws, _: &NextSession, window, cx| ws.cycle(1, window, cx)))
            .on_action(cx.listener(|ws, a: &crate::app::ShowSession, window, cx| ws.show_numbered(a.0, window, cx)))
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
                if ws.active_id().is_some() {
                    let cwd = ws.active_cwd();
                    ws.act(Act::OpenGithub { cwd }, window, cx)
                }
            }))
            .on_action(cx.listener(|ws, _: &OpenSettings, window, cx| ws.act(Act::OpenSettings, window, cx)))
            .on_action(cx.listener(|ws, _: &SwapFocus, window, cx| ws.swap_focus(window, cx)))
            .on_action(cx.listener(|ws, _: &ToggleEditor, window, cx| {
                if ws.editor_owner().is_some() {
                    ws.act(Act::ToggleEditor, window, cx)
                }
            }))
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
                    // Over a session, files go to it as paths.
                    match view {
                        SessionView::Thread(v) => v.update(cx, |v, cx| v.paste_paths(paths.paths(), window, cx)),
                        SessionView::Terminal(v) => v.update(cx, |v, cx| v.paste_paths(paths.paths(), cx)),
                    }
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
            .children(picker_dialog)
            .children(agent_dialog)
            .children(toast)
    }
}

