//! Main area (the shown session / home), overlays (menu, modal, toast), and the root `Render`.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, prelude::FluentBuilder,
    px, rgba,
};

use crate::claude::defaults::Effort;
use crate::claude::mcp::Feature;
use crate::app::{
    Act, CloseSession, FocusSearch, GitOp, GitPickerTarget, ImportProject, Main, Modal, NewClaude, NextSession, OpenGithub, OpenSettings, PrevSession, ProjectPickerMode, SessionView,
    SettingsPage, ShellDrag, SkillScope, SwapFocus, ToggleEditor, ToggleRight, ToggleShell, ToggleSidebar, Workspace, WorkspacePicker, SHELL_HEIGHT,
};
use gpui_kit::component::{TitleBar, input::{Input, Textarea}};
use crate::panels::{composer, logos, picker};
use crate::panels::theme::{self, hsla};
use crate::updater::Status;
use gpui_kit::assets::IconName;
use crate::panels::settings_pages::{flip, settings_group, toggle_switch};
use crate::panels::widgets::{
    Anchor, Dropdown, Trigger, button, button_disabled, danger_button, effort_slider, fade_in, icon, icon_button, icon_button_disabled, key_badge, key_hint, section_label, short_path, spinner,
};

impl Workspace {
    // ------------------------------------------------------------------ top bar

    fn render_topbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let crate::panels::inspector::Shown { rec, cwd, .. } = self.shown();
        let hints = self.shortcuts_shown;
        let session_active = self.active_id().is_some();

        let title = rec.as_ref().map(|r| r.display_title().to_string()).unwrap_or_else(|| "Claudiu".into());
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
                    .gap(px(if hints { 4.0 } else { 0.0 }))
                    .occlude()
                    .when(hints && session_active, |d| d.child(key_hint("Shift+E")))
                    .child(if session_active {
                        icon_button("toggle-editor", IconName::FileCode).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleEditor, window, cx)))
                    } else {
                        icon_button_disabled("toggle-editor", IconName::FileCode)
                    })
                    .when(hints, |d| d.child(key_hint("J")))
                    .child(icon_button("toggle-shell", IconName::PanelBottom).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.toggle_shell_here(window, cx))))
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
                .child(div().flex_1().min_h_0().p(px(6.0)).child(term)),
            format!("pane-in-{id}"),
            150,
            0.4,
        );
        pane.into_any_element()
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

    // ------------------------------------------------------------------ details & home

    fn render_home(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        use gpui::Focusable;
        let scaled = crate::panels::scale::px;
        let chosen_model = self.home_selected_model();

        let project = self.home_selected_project().cloned();
        let target_cwd = self.home_target_cwd();
        let git = project.as_ref().and_then(|p| p.git.clone());
        let branch = git.as_ref().and_then(|g| g.branch_at(&target_cwd)).map(str::to_string);
        let worktree_label = git.as_ref().and_then(|g| crate::git::worktree::label(g, &target_cwd));

        let project_open = matches!(self.picker_kind(), Some(WorkspacePicker::Project { mode: ProjectPickerMode::Home, .. }));
        let project_label = project.as_ref().map_or_else(|| "Project".to_string(), |p| p.name.clone());
        let project_chip = composer::chip("home-project", project_open)
            .text_color(hsla(if project.is_some() { theme::TEXT } else { theme::TEXT_DIM }))
            .child(icon(IconName::FolderOpen, 12.0).text_color(hsla(theme::TEXT_DIM)))
            .child(div().max_w(px(150.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(project_label))
            .child(icon(IconName::ChevronDown, 10.5).text_color(hsla(theme::TEXT_FAINT)))
            .dropdown(cx, |ws, at, window, cx| {
                ws.open_project_picker(ProjectPickerMode::Home, at, window, cx);
            });

        let git_chip = project.as_ref().and_then(|p| {
            let g = p.git.as_ref()?;
            let primary = branch.clone().or_else(|| worktree_label.clone()).unwrap_or_else(|| "main".into());
            let linked = (!g.is_main(&target_cwd)).then(|| worktree_label.clone()).flatten();
            let pid = p.id.clone();
            let cwd = target_cwd.clone();
            let open = matches!(self.picker_kind(), Some(WorkspacePicker::Git(gp)) if gp.target == GitPickerTarget::Home);
            Some(
                composer::git_chip("home-git", open, primary, linked, None)
                    .dropdown(cx, move |ws, at, window, cx| {
                        ws.open_git_picker(pid.clone(), cwd.clone(), GitPickerTarget::Home, at, window, cx);
                    }),
            )
        });

        let model_open = matches!(self.picker_kind(), Some(WorkspacePicker::HomeModel { .. }));
        let effort_badge = crate::thread::short_effort_label(self.home.effort.name());
        let model_anchor = self.home.model_anchor.clone();
        let model_chip = composer::chip("home-model", model_open)
            .relative()
            .child(model_anchor.probe())
            .child(logos::draw(logos::for_harness(chosen_model.harness), 11.0, hsla(theme::TEXT_DIM)))
            .child(div().max_w(px(190.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(hsla(theme::TEXT)).child(chosen_model.name.clone()))
            .child(div().px(px(5.0)).rounded(px(4.0)).bg(hsla(0x1f1f22)).text_size(scaled(10.0)).text_color(hsla(theme::TEXT_DIM)).child(effort_badge))
            .child(icon(IconName::ChevronDown, 10.5).text_color(hsla(theme::TEXT_FAINT)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    ws.open_home_model_picker(model_anchor.below(), window, cx);
                }),
            );

        let start_btn = button("home-launch", "Start", true)
            .h(px(26.0))
            .px(px(12.0))
            .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::HomeLaunch, window, cx)));

        let focused = self.home.prompt.focus_handle(cx).is_focused(window);
        let input = composer::input_area()
            .min_h(px(74.0))
            .on_mouse_down(MouseButton::Left, cx.listener(|ws, _, window, cx| ws.home.prompt.update(cx, |p, cx| p.focus(window, cx))))
            .child(Textarea::new(&self.home.prompt).appearance(false))
            .into_any_element();
        let chips: Vec<AnyElement> = [Some(model_chip.into_any_element()), Some(project_chip.into_any_element()), git_chip.map(IntoElement::into_any_element)].into_iter().flatten().collect();
        let composer = composer::frame(focused, None, input, chips, vec![start_btn.into_any_element()]).max_w(px(680.0));

        let shortcut_chip = |keys: &str, what: &str| {
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .text_size(scaled(11.0))
                .text_color(hsla(theme::TEXT_FAINT))
                .child(key_hint(keys))
                .child(what.to_string())
        };

        div()
            .id("home-root")
            .relative()
            .size_full()
            .px(px(28.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(20.0))
            .child(div().text_size(scaled(24.0)).font_weight(FontWeight::MEDIUM).text_color(hsla(theme::TEXT)).child("What are we working on?"))
            .child(composer)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(16.0))
                    .child(shortcut_chip("Shift+N", "New session"))
                    .child(shortcut_chip("Shift+E", "Editor"))
                    .child(shortcut_chip("J", "Shell"))
                    .child(shortcut_chip(",", "Settings")),
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
    fn render_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
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

    /// Universal anchored dropdown picker (based on the Model Picker) used for model, project, branch & worktree,
    /// Settings selectors, Skills scope, and editor selection.
    fn render_picker(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let active = self.picker.as_ref()?;
        let at = active.at;
        let scaled = crate::panels::scale::px;
        let accent = hsla(theme::ACCENT);
        let priority = if self.modal.is_some() { 25 } else { 22 };

        let (id, title, body, choices): (&'static str, &'static str, AnyElement, Option<picker::Choices<Self>>) = match &active.kind {
            WorkspacePicker::HomeModel { search } => {
                let query = search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let chosen_model = self.home_selected_model();
                let filtered: Vec<crate::harness::CatalogModel> = self
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
                let hi = self.home.model_highlight.min(filtered.len().saturating_sub(1));
                let mut list = div().id("home-model-list").max_h(px(176.0)).overflow_y_scroll().track_scroll(&self.home.model_scroll).flex().flex_col().gap(px(2.0));
                for (n, m) in filtered.iter().enumerate() {
                    let selected = chosen_model.harness == m.harness && chosen_model.id == m.id;
                    let focused = n == hi;
                    let choice = m.clone();
                    let logo = logos::draw(logos::for_harness(m.harness), 11.5, if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    list = list.child(
                        picker::row(gpui::SharedString::from(format!("home-model-opt-{n}")), Some(logo), m.name.clone(), None, None, selected, false)
                            .when(focused && !selected, |d| d.bg(hsla(theme::SURFACE_HOVER)))
                            .when(focused, |d| d.border_1().border_color(accent.opacity(0.45)))
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::HomeSelectModel(choice.clone()), window, cx))),
                    );
                }
                if filtered.is_empty() {
                    list = list.child(picker::empty_note("No models match."));
                }

                let effort_labels: Vec<String> = vec!["Low".into(), "Med".into(), "High".into(), "XHigh".into(), "Max".into()];
                let effort_ix = Effort::ALL.iter().position(|x| *x == self.home.effort).unwrap_or(1);
                let on_effort: crate::panels::widgets::EffortPick<Self> = std::rc::Rc::new(|ws, ix, window, cx| {
                    if let Some(&e) = Effort::ALL.get(ix) {
                        ws.act(Act::HomeSetEffort(e), window, cx);
                    }
                });
                let effort_trailing = div().text_size(scaled(10.5)).text_color(hsla(theme::TEXT_FAINT)).child("←/→ or drag").into_any_element();
                let loading = self.model_lookups.any_loading(&self.store.data.settings.enabled_harnesses_list());
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(picker::search_input(search))
                    .child(list)
                    .when(loading, |d| {
                        d.child(
                            div()
                                .px(px(6.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(scaled(11.0))
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(spinner("home-models-loading", 11.0))
                                .child("Loading models…"),
                        )
                    })
                    .child(picker::divider())
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .child(picker::section_header("EFFORT", Some(effort_trailing)))
                            .child(effort_slider(
                                "home-effort",
                                &effort_labels,
                                Some(effort_ix),
                                accent,
                                self.home.effort_bounds.clone(),
                                self.home.effort_dragging.clone(),
                                on_effort,
                                cx,
                            )),
                    );
                ("home-model-dialog", "MODEL", body.into_any_element(), None)
            }
            WorkspacePicker::Project { mode, search } => {
                let mode = *mode;
                let query = search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let matches = |hay: &str| {
                    let low = hay.to_lowercase();
                    words.iter().all(|w| low.contains(w))
                };
                let current_id = match mode {
                    ProjectPickerMode::Home => self.home_selected_project().map(|p| p.id.clone()),
                    ProjectPickerMode::SidebarFilter => self.project_filter().map(|p| p.id.clone()),
                };
                let mut choices = picker::Choices::new(&self.picker_nav, &query);
                let mut list = choices.list("project-picker-list").max_h(px(260.0));
                let mut shown = 0usize;
                if mode == ProjectPickerMode::SidebarFilter && matches("all projects") {
                    let selected = current_id.is_none();
                    let lead = icon(IconName::FolderOpen, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row("project-picker-all", Some(lead), "All projects", Some("Show sessions from every project".into()), None, selected, false);
                    list = list.child(choices.add(row, |ws, window, cx| ws.act(Act::FilterProject(None), window, cx), cx));
                    shown += 1;
                }
                for (i, p) in self.store.data.projects.iter().enumerate() {
                    let path_str = short_path(&p.path);
                    if !matches(&format!("{} {path_str}", p.name)) {
                        continue;
                    }
                    let selected = current_id.as_ref() == Some(&p.id);
                    let lead = icon(IconName::FolderOpen, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let act = match mode {
                        ProjectPickerMode::Home => Act::HomeSelectProject(p.id.clone()),
                        ProjectPickerMode::SidebarFilter => Act::FilterProject(Some(p.id.clone())),
                    };
                    let row = picker::row(gpui::SharedString::from(format!("project-picker-{i}")), Some(lead), p.name.clone(), Some(path_str), None, selected, false);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(act.clone(), window, cx), cx));
                    shown += 1;
                }
                if shown == 0 {
                    list = list.child(picker::empty_note("No projects match."));
                }
                let add_btn = picker::action_pill("project-picker-add", "+ Add project", |ws, window, cx| ws.act(Act::ImportProject, window, cx), cx).into_any_element();
                let title = "PROJECT";
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(picker::search_input(search))
                    .child(picker::section_header("PROJECTS", Some(add_btn)))
                    .child(list);
                ("project-picker-dialog", title, body.into_any_element(), Some(choices))
            }
            WorkspacePicker::Git(gp) => {
                let query = gp.search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let matches = |hay: &str| {
                    let low = hay.to_lowercase();
                    words.iter().all(|w| low.contains(w))
                };
                let target = gp.target;
                let can_switch = gp.can_switch_worktree;
                let new_wt_disabled = gp.new_worktree_reason.is_some() || !can_switch;
                let main_checkout = gp.main_checkout.clone();
                let cwd = gp.cwd.clone();

                let mut choices = picker::Choices::new(&self.picker_nav, &query);
                // The quickest way to start work: a worktree named after what was typed (or a random name), made at once.
                let typed = gp.search.read(cx).value().trim().to_string();
                let quick = |base: crate::git::worktree::Base| Act::QuickWorktree { main_checkout: main_checkout.clone(), name: typed.clone(), base, for_home: target == GitPickerTarget::Home };
                let new_wt_title = if typed.is_empty() { "New worktree".to_string() } else { format!("New worktree “{typed}”") };
                let new_wt_row: AnyElement = if new_wt_disabled {
                    let why = gp.new_worktree_reason.clone().unwrap_or_else(|| "Resume the session to create one".into());
                    picker::row("git-picker-new-worktree", Some(icon(IconName::Plus, 12.0).into_any_element()), new_wt_title, Some(why), None, false, true).into_any_element()
                } else {
                    let detail = match (typed.is_empty(), target) {
                        (true, GitPickerTarget::Session) => "Random name, from main. The session moves into it; type above to name it",
                        (true, GitPickerTarget::Home) => "Random name, from main. Type above to name it",
                        (false, GitPickerTarget::Session) => "New branch from main. The session moves into it",
                        (false, GitPickerTarget::Home) => "New branch from main, for this session",
                    };
                    let from_main = quick(crate::git::worktree::Base::Main);
                    let from_current = quick(crate::git::worktree::Base::Current);
                    let current_pill = picker::action_pill("git-picker-new-worktree-current", "from current", move |ws, window, cx| ws.act(from_current.clone(), window, cx), cx)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .into_any_element();
                    let row = picker::row("git-picker-new-worktree", Some(icon(IconName::Plus, 12.0).into_any_element()), new_wt_title, Some(detail.into()), Some(current_pill), false, false)
                        .border_1()
                        .border_color(hsla(theme::PANEL_BORDER));
                    choices.add(row, move |ws, window, cx| ws.act(from_main.clone(), window, cx), cx)
                };

                let mut wt_list: Vec<AnyElement> = Vec::new();
                let mut wt_shown = 0usize;
                for (i, wt) in gp.worktrees.iter().enumerate() {
                    if !matches(&format!("{} {}", wt.label, wt.branch.as_deref().unwrap_or(""))) {
                        continue;
                    }
                    let current = wt.current;
                    let folder = wt.folder.clone();
                    let remove = wt.remove.clone();
                    let row_group = gpui::SharedString::from(format!("gp-wt-row-{i}"));
                    let lead = icon(IconName::FolderGit2, 12.0).text_color(if current { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let trailing = div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .when(wt.running, |d| d.child(div().w(px(5.0)).h(px(5.0)).rounded_full().bg(hsla(theme::OK))))
                        .when_some(wt.branch.clone(), |d, b| {
                            d.child(div().max_w(px(110.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(scaled(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(b))
                        })
                        .children(remove.map(|act| {
                            icon_button(gpui::SharedString::from(format!("git-picker-wt-rm-{i}")), IconName::X)
                                .invisible()
                                .group_hover(row_group.clone(), |s| s.visible())
                                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    ws.act(act.clone(), window, cx);
                                }))
                        }))
                        .into_any_element();
                    let row = picker::row(gpui::SharedString::from(format!("git-picker-wt-{i}")), Some(lead), wt.label.clone(), None, Some(trailing), current, !can_switch).group(row_group);
                    wt_list.push(if can_switch {
                        choices.add(
                            row,
                            move |ws, window, cx| {
                                if current {
                                    ws.dismiss_picker(window, cx);
                                } else {
                                    let act = match target {
                                        GitPickerTarget::Home => Act::HomeSelectWorktree(folder.clone()),
                                        GitPickerTarget::Session => Act::SwitchWorktree { folder: folder.clone(), base: Default::default() },
                                    };
                                    ws.act(act, window, cx);
                                }
                            },
                            cx,
                        )
                    } else {
                        row.into_any_element()
                    });
                    wt_shown += 1;
                }
                if wt_shown == 0 {
                    wt_list.push(picker::empty_note("No matching worktrees.").into_any_element());
                }

                let new_branch_cwd = cwd.clone();
                let new_branch_btn = picker::action_pill("git-picker-new-branch", "+ New branch", move |ws, window, cx| ws.act(Act::NewBranch(new_branch_cwd.clone()), window, cx), cx).into_any_element();
                // Spaces aren't allowed in branch names, as in the New branch dialog.
                let typed_branch = typed.split_whitespace().collect::<Vec<_>>().join("-");
                let branch_exists = matches!(&gp.branches, Some(Ok(list)) if list.iter().any(|b| b.name == typed_branch));
                let create_branch_row = (!typed_branch.is_empty() && !branch_exists).then(|| {
                    let act = Act::CreateBranch(cwd.clone(), typed_branch.clone());
                    let row = picker::row("git-picker-create-branch", Some(icon(IconName::Plus, 12.0).into_any_element()), format!("Create branch “{typed_branch}”"), Some("Switches this checkout to it".into()), None, false, false);
                    choices.add(
                        row,
                        move |ws, window, cx| {
                            ws.dismiss_picker(window, cx);
                            ws.act(act.clone(), window, cx);
                        },
                        cx,
                    )
                });

                let mut br_list: Vec<AnyElement> = Vec::new();
                match &gp.branches {
                    None => {
                        br_list.push(
                            div()
                                .px(px(8.0))
                                .py(px(8.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(scaled(11.5))
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(spinner("git-picker-br-loading", 12.0))
                                .child("Loading branches…")
                                .into_any_element(),
                        );
                    }
                    Some(Err(why)) => {
                        br_list.push(div().px(px(8.0)).py(px(6.0)).text_size(scaled(11.5)).text_color(hsla(theme::DANGER)).child(why.clone()).into_any_element());
                    }
                    Some(Ok(branches)) => {
                        let mut br_shown = 0usize;
                        for (i, br) in branches.iter().enumerate() {
                            let hay = match &br.held_tree {
                                Some((tree, _)) => format!("{} {tree}", br.name),
                                None => br.name.clone(),
                            };
                            if !matches(&hay) {
                                continue;
                            }
                            let current = br.current;
                            let held = br.held_tree.clone();
                            let name = br.name.clone();
                            let dir = cwd.clone();
                            let remove = br.remove.clone();
                            let row_group = gpui::SharedString::from(format!("gp-br-row-{i}"));
                            let act = match &held {
                                Some((_, folder)) => match target {
                                    GitPickerTarget::Home => Act::HomeSelectWorktree(folder.clone()),
                                    GitPickerTarget::Session => Act::SwitchWorktree { folder: folder.clone(), base: Default::default() },
                                },
                                None => Act::SwitchBranch(dir, name),
                            };
                            let lead = icon(IconName::GitBranch, 12.0).text_color(if current { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                            let trailing = (held.is_some() || remove.is_some()).then(|| {
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .when_some(held.map(|(tree, _)| tree), |d, tree| {
                                        d.child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(4.0))
                                                .text_size(scaled(11.0))
                                                .text_color(hsla(theme::TEXT_FAINT))
                                                .child(icon(IconName::FolderGit2, 11.0))
                                                .child(format!("opens {tree}")),
                                        )
                                    })
                                    .children(remove.map(|rm_act| {
                                        icon_button(gpui::SharedString::from(format!("git-picker-br-rm-{i}")), IconName::X)
                                            .invisible()
                                            .group_hover(row_group.clone(), |s| s.visible())
                                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                                                cx.stop_propagation();
                                                ws.act(rm_act.clone(), window, cx);
                                            }))
                                    }))
                                    .into_any_element()
                            });
                            let row = picker::row(gpui::SharedString::from(format!("git-picker-br-{i}")), Some(lead), br.name.clone(), None, trailing, current, false).group(row_group);
                            br_list.push(choices.add(
                                row,
                                move |ws, window, cx| {
                                    if current {
                                        ws.dismiss_picker(window, cx);
                                    } else {
                                        ws.act(act.clone(), window, cx);
                                    }
                                },
                                cx,
                            ));
                            br_shown += 1;
                        }
                        if br_shown == 0 {
                            br_list.push(picker::empty_note("No matching branches.").into_any_element());
                        }
                    }
                }

                // One scrolling list, so Up and Down move through worktrees and branches alike.
                let list = choices
                    .list("git-picker-list")
                    .max_h(px(320.0))
                    .child(new_wt_row)
                    .child(div().pt(px(6.0)).child(picker::section_header("WORKTREES", None)))
                    .when(!can_switch, |d| d.child(div().text_size(scaled(11.0)).text_color(hsla(theme::TEXT_FAINT)).child("Resume the session to switch worktrees.")))
                    .children(wt_list)
                    .child(div().py(px(4.0)).child(picker::divider()))
                    .child(picker::section_header("BRANCHES", Some(new_branch_btn)))
                    .children(create_branch_row)
                    .children(br_list);
                let body = div().flex().flex_col().gap(px(8.0)).child(picker::search_input(&gp.search)).child(list);
                ("git-picker-dialog", "BRANCH & WORKTREE", body.into_any_element(), Some(choices))
            }
            WorkspacePicker::SettingsModel { search } => {
                let Some(Modal::Settings { default_model, subagents, .. }) = &self.modal else { return None };
                let current = default_model.clone();
                let query = search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let mut all = self.store.data.settings.aggregated_models_for(&subagents.enabled);
                if !all.iter().any(|m| m.harness == current.harness && m.id == current.id) {
                    all.insert(0, current.clone());
                }
                let mut choices = picker::Choices::new(&self.picker_nav, &query);
                let mut list = choices.list("settings-model-list").max_h(px(260.0));
                let mut shown = 0usize;
                for (n, m) in all.into_iter().enumerate() {
                    let hay = format!("{} {} {}", m.name, m.id, m.harness.name()).to_lowercase();
                    if !words.iter().all(|w| hay.contains(w)) {
                        continue;
                    }
                    let selected = m.harness == current.harness && m.id == current.id;
                    let choice = m.clone();
                    let logo = logos::draw(logos::for_harness(m.harness), 11.5, if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row(gpui::SharedString::from(format!("settings-model-opt-{n}")), Some(logo), m.name.clone(), None, None, selected, false);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::PickDefaultModel(choice.clone()), window, cx), cx));
                    shown += 1;
                }
                if shown == 0 {
                    list = list.child(picker::empty_note("No models match."));
                }
                let loading = self.model_lookups.any_loading(&subagents.enabled);
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(picker::search_input(search))
                    .child(list)
                    .when(loading, |d| {
                        d.child(
                            div()
                                .px(px(6.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(scaled(11.0))
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(spinner("settings-models-loading", 11.0))
                                .child("Loading models…"),
                        )
                    });
                ("settings-model-dialog", "DEFAULT MODEL", body.into_any_element(), Some(choices))
            }
            WorkspacePicker::SettingsEffort => {
                let Some(Modal::Settings { effort, .. }) = &self.modal else { return None };
                let current = *effort;
                let mut choices = picker::Choices::new(&self.picker_nav, "");
                let mut list = choices.list("settings-effort-list");
                for (i, &opt) in Effort::ALL.iter().enumerate() {
                    let selected = current == opt;
                    let row = picker::row(gpui::SharedString::from(format!("settings-effort-{i}")), None, opt.label(), None, None, selected, false);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::PickEffort(opt), window, cx), cx));
                }
                ("settings-effort-dialog", "EFFORT", list.into_any_element(), Some(choices))
            }
            WorkspacePicker::SettingsPermission => {
                let Some(Modal::Settings { default_model, permission, .. }) = &self.modal else { return None };
                let current = permission.clone();
                let modes = self.store.data.settings.modes_for(default_model.harness);
                let mut choices = picker::Choices::new(&self.picker_nav, "");
                let mut list = choices.list("settings-perm-list").max_h(px(340.0));
                for (i, (id, name)) in modes.into_iter().enumerate() {
                    let selected = current == id;
                    let display = picker::format_mode_name(&id, &name);
                    let desc = picker::permission_description(&id).to_string();
                    let lead = icon(IconName::Lock, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let pick_id = id.clone();
                    let row = picker::row(
                            gpui::SharedString::from(format!("settings-perm-{i}")),
                            Some(lead),
                            display,
                            Some(desc),
                            None,
                            selected,
                            false,
                        );
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::PickPermission(pick_id.clone()), window, cx), cx));
                }
                ("settings-perm-dialog", "PERMISSION MODE", list.into_any_element(), Some(choices))
            }
            WorkspacePicker::SkillsScope { search } => {
                let query = search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let matches = |hay: &str| {
                    let low = hay.to_lowercase();
                    words.iter().all(|w| low.contains(w))
                };
                let current = self.effective_skill_scope();
                let mut choices = picker::Choices::new(&self.picker_nav, &query);
                let mut list = choices.list("skills-scope-list").max_h(px(250.0));
                let mut shown = 0usize;
                if matches("global all workspaces") {
                    let selected = current == SkillScope::Global;
                    let lead = icon(IconName::FolderOpen, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row("skills-scope-global", Some(lead), "Global (all workspaces)", Some("Symlink into global harness skills folders".into()), None, selected, false);
                    list = list.child(choices.add(row, |ws, window, cx| ws.act(Act::SetSkillScope(SkillScope::Global), window, cx), cx));
                    shown += 1;
                }
                for (i, p) in self.store.data.projects.iter().enumerate() {
                    let path_str = short_path(&p.path);
                    if !matches(&format!("{} {path_str}", p.name)) {
                        continue;
                    }
                    let scope = SkillScope::Project(p.id.clone());
                    let selected = current == scope;
                    let lead = icon(IconName::FolderOpen, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row(gpui::SharedString::from(format!("skills-scope-{i}")), Some(lead), p.name.clone(), Some(path_str), None, selected, false);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::SetSkillScope(scope.clone()), window, cx), cx));
                    shown += 1;
                }
                if shown == 0 {
                    list = list.child(picker::empty_note("No matching scope."));
                }
                let body = div().flex().flex_col().gap(px(8.0)).child(picker::search_input(search)).child(list);
                ("skills-scope-dialog", "SKILL SCOPE", body.into_any_element(), Some(choices))
            }
            WorkspacePicker::Editor { target } => {
                let current = self.preferred_editor();
                let target = target.clone();
                let mut choices = picker::Choices::new(&self.picker_nav, "");
                let mut list = choices.list("editor-picker-list");
                let mut shown = 0usize;
                for (i, &(ed, found)) in self.editors.iter().enumerate() {
                    if !found {
                        continue;
                    }
                    let selected = current == Some(ed);
                    let path = target.clone();
                    let lead = icon(IconName::ExternalLink, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row(gpui::SharedString::from(format!("editor-picker-{i}")), Some(lead), ed.label(), None, None, selected, false);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::OpenEditor(ed, path.clone()), window, cx), cx));
                    shown += 1;
                }
                if shown == 0 {
                    list = list.child(picker::empty_note("No editors found."));
                }
                ("editor-picker-dialog", "OPEN IN EDITOR", list.into_any_element(), Some(choices))
            }
        };

        Some(picker::dropdown_overlay((id, title), at, priority, body, choices, |ws: &mut Self, window, cx| ws.dismiss_picker(window, cx), window, cx))
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
                if ws.active_id().is_some() {
                    let cwd = ws.active_cwd();
                    ws.act(Act::OpenGithub { cwd }, window, cx)
                }
            }))
            .on_action(cx.listener(|ws, _: &OpenSettings, window, cx| ws.act(Act::OpenSettings, window, cx)))
            .on_action(cx.listener(|ws, _: &SwapFocus, window, cx| ws.swap_focus(window, cx)))
            .on_action(cx.listener(|ws, _: &ToggleEditor, window, cx| {
                if ws.active_id().is_some() {
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

/// Split-pane Settings window: left navigation rail with icons + version footer, right scrollable page with grouped
/// section cards and a dedicated Save/Cancel action bar.
fn render_settings_card(
    ws: &Workspace,
    viewport: gpui::Size<gpui::Pixels>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> gpui::Stateful<gpui::Div> {
    let (page, worktrees, scroll) = (ws.settings_page, &ws.worktrees_page, &ws.settings_scroll);
    let Some(Modal::Settings { shell, default_model, effort, permission, updates, scale, trimmed, claudiu_off, skill_search, list_focus, renaming, .. }) = &ws.modal else {
        return div().id("modal-card");
    };
    let (updates, scale) = (*updates, *scale);
    let scaled = crate::panels::scale::px;
    let faint = |text: String| div().text_size(scaled(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(text);

    let content: AnyElement = match page {
        SettingsPage::General => {
            let model_open = matches!(ws.picker_kind(), Some(WorkspacePicker::SettingsModel { .. }));
            let effort_open = matches!(ws.picker_kind(), Some(WorkspacePicker::SettingsEffort));
            let perm_open = matches!(ws.picker_kind(), Some(WorkspacePicker::SettingsPermission));
            let modes = ws.store.data.settings.modes_for(default_model.harness);
            let perm_raw = modes.iter().find(|(id, _)| id == permission).map_or(permission.as_str(), |(_, n)| n.as_str());
            let perm_display = picker::format_mode_name(permission, perm_raw);
            let mut session_rows: Vec<AnyElement> = vec![
                default_model_row(default_model, model_open, cx).into_any_element(),
                settings_picker_row("settings-effort", ("Effort", "Reasoning depth for new sessions"), effort.label().to_string(), effort_open, |ws, at, _, cx| ws.open_settings_effort_picker(at, cx), cx).into_any_element(),
                settings_picker_row("settings-permission", ("Permission mode", "Approval policy for tool calls and edits"), perm_display, perm_open, |ws, at, _, cx| ws.open_settings_permission_picker(at, cx), cx).into_any_element(),
            ];
            if matches!(permission.as_str(), "bypassPermissions" | "fullAccess" | "full_access" | "yolo") {
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
            } else if permission == "plan" && trimmed.contains(&crate::claude::trim::Trim::PlanMode) {
                session_rows.push(
                    div()
                        .px(px(14.0))
                        .py(px(8.0))
                        .text_size(scaled(11.5))
                        .text_color(hsla(theme::TEXT_FAINT))
                        .child("Plan mode is off under Claude Code's tools, so new Claude Code sessions start in default mode.")
                        .into_any_element(),
                );
            }
            let env_rows: Vec<AnyElement> = vec![
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
                .child(row_labels("Automatic updates", Some("Check for new Claudiu releases in the background")))
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
        SettingsPage::ClaudiuTools => {
            let rows: Vec<AnyElement> = Feature::ALL
                .into_iter()
                .map(|feature| {
                    let on = !claudiu_off.contains(&feature);
                    div()
                        .id(("settings-claudiu", feature as usize))
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
                            if let Some(Modal::Settings { claudiu_off, .. }) = ws.modal.as_mut() {
                                flip(claudiu_off, feature);
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
                .child(div().text_size(scaled(11.5)).font_weight(FontWeight::MEDIUM).text_color(hsla(theme::TEXT_DIM)).child(format!("Claudiu {}", env!("CARGO_PKG_VERSION"))))
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
        .bg(hsla(0x09090a))
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
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(button("modal-cancel", "Cancel", false).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::Dismiss, window, cx))))
                .child(button("modal-ok", "Save", true).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.submit_modal(window, cx)))),
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

    div()
        .id("modal-card")
        .occlude()
        .track_focus(&ws.modal_focus)
        .w(scaled(880.0).min(viewport.width - px(48.0)))
        .h(scaled(620.0).min(viewport.height - px(48.0)))
        .rounded(px(12.0))
        .overflow_hidden()
        .bg(hsla(theme::PANEL_BG))
        .border_1()
        .border_color(hsla(theme::PANEL_BORDER))
        .shadow_lg()
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

/// The Worktrees section on the Projects page: worktree folders Claudiu made, grouped by repository.
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
        .child(faint("Worktrees Claudiu makes live in its own data folder, outside your repositories.".into()))
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
                ws.open_settings_model_picker(pill_anchor.below_right().with_hit(row_anchor.bounds()), window, cx);
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
                .child(div().text_color(hsla(theme::ACCENT)).child(current))
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
            }
            cx.notify();
        }))
}
