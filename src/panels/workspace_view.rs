//! The window's root view: the top bar, the sidebars around the shown session (or the home page), the shell panel
//! under it, and the overlays (`overlays.rs`) above everything.

use rune::tok::motion;
use rune::{Color, Cursor, Cx, El, Phase, Transition, Tx, col, el, row, text};

use crate::app::{Act, GitOp, Main, MenuAnchor, MenuItem, SHELL_HEIGHT, SideDrag, Workspace};
use crate::panels::icons;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{ago, button, button_leading, clawd, font, icon, icon_button, icon_button_disabled, key_hint, label, section_label, short_path, shortcut, spinner};
use crate::store::{SessionKind, now};
use crate::updater::Status;

/// Run `act` from a click, then hand its effects to the frame.
fn act(ws: &mut Workspace, a: Act, tx: &mut Tx) {
    ws.act(a);
    ws.after_input(tx);
}

/// The root of the main window.
pub fn view(ws: &Workspace, cx: &mut Cx<Workspace>) -> El {
    crate::panels::scale::set_screen(cx.env().scale);
    let insets = cx.chrome_insets();
    let banner = ws.update_banner();
    let content_top = if banner.is_some() { theme::PANEL_GAP } else { 0.0 };
    let main_panel = col()
        .key("main-panel")
        .grow(1.0)
        .shrink()
        .min_h(0.0)
        .w_full()
        .radius(theme::PANEL_RADIUS)
        .clip_rounded(theme::PANEL_RADIUS)
        .border(1.0, rgb(theme::PANEL_BORDER))
        .fill(rgb(theme::PANEL_BG))
        .child(match ws.main {
            Main::Terminals => ws.render_session(),
            Main::Home => ws.render_home(),
        });
    let editor = ws.render_editor_pane();
    let mut center = col().key("center").h_full().motion(motion::GLIDE).child(main_panel).child(ws.render_shell());
    // In editor mode the terminal narrows to the left and the editor takes the rest.
    center = match editor {
        Some(_) => center.w((ws.window_size.0 * 0.36).clamp(380.0, 640.0)),
        None => center.grow(1.0).shrink().min_w(0.0),
    };
    let mut body = row().key("body").grow(1.0).shrink().min_h(0.0).w_full().child(ws.render_sidebar()).child(center);
    if let Some(editor) = editor {
        body = body.child(el().key("editor-gap").w(theme::PANEL_GAP)).child(editor);
    }
    let body = body.child(ws.render_right());
    let mut stage = col().key("stage").grow(1.0).shrink().min_h(0.0).w_full().gap(theme::PANEL_GAP).px(theme::PANEL_GAP).pt(content_top).pb(theme::PANEL_GAP);
    if let Some(b) = banner {
        stage = stage.child(b);
    }
    let mut root = col()
        .key("workspace")
        .w_full()
        .h_full()
        .fill(rgb(theme::APP_BG))
        .on_layout(|ws: &mut Workspace, r: [f32; 4]| ws.window_size = (r[2], r[3]))
        .on_window_focus(|ws: &mut Workspace, active: bool, tx: &mut Tx| ws.on_window_focus(active, tx))
        .on_drop_files(|ws: &mut Workspace, paths: &[std::path::PathBuf], tx: &mut Tx| ws.on_drop(paths, tx))
        // A paste no text field took (Edit › Paste from the menu bar) goes to the terminal with the keyboard.
        .on_paste_content(|ws: &mut Workspace, content: &rune::ClipboardContent, tx: &mut Tx| ws.on_paste(content, tx))
        .child(ws.render_topbar(insets.leading, insets.trailing))
        .child(stage.child(body));
    for overlay in ws.overlays() {
        root = root.child(overlay);
    }
    root
}

impl Workspace {
    // ------------------------------------------------------------------ top bar

    fn render_topbar(&self, leading: f32, trailing: f32) -> El {
        let crate::panels::inspector::Shown { rec, cwd, project } = self.shown();
        // Git actions live in the right sidebar; the branch pill here needs the repository of the session's own folder.
        let repo = project.as_ref().filter(|p| p.git.is_some()).and(cwd.clone());
        let busy = self.git_busy;
        let hints = self.shortcuts_shown;
        let session_active = self.active_id().is_some();
        let title = rec.as_ref().map(|r| r.display_title().to_string()).unwrap_or_else(|| "Claudiu".into());
        // The branch and the worktree of the session's own folder, which is not the project's folder for a worktree session.
        let git = project.as_ref().and_then(|p| p.git.as_ref());
        let branch = git.and_then(|g| cwd.as_deref().map_or(g.branch.as_deref(), |c| g.branch_at(c))).map(str::to_string);
        let worktree = project.as_ref().zip(git).zip(cwd.as_deref()).and_then(|((p, g), c)| Some((p.id.clone(), c.to_path_buf(), crate::git::worktree::label(g, c)?)));

        let mut left = row().grow(1.0).shrink().min_w(0.0).center_y().gap(6.0).child(
            icon_button(&icons::PANEL_LEFT).key("toggle-sidebar").help("Toggle sidebar").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                ws.toggle_left();
                ws.after_input(tx);
            }),
        );
        if hints {
            left = left.child(key_hint("Shift+B").key("hint-sidebar"));
        }
        // The title fades across when the shown session changes.
        left = left.child(text(title.clone()).key(format!("title-{title}")).font(font(13.0, 500.0)).color(rgb(theme::TEXT)).truncate().shrink().transition(Transition::opacity()));
        if let Some(p) = &cwd {
            left = left.child(label(short_path(p), 11.5, theme::TEXT_FAINT).truncate().shrink());
        }
        if let (Some(b), Some(repo)) = (branch, repo) {
            // Click: switch branch or create one (see Workspace::open_branch_menu).
            let tail = if busy == Some(GitOp::Branch) { spinner(12.0, theme::TEXT_DIM) } else { icon(&icons::CHEVRON_DOWN, 12.0, theme::TEXT_FAINT) };
            left = left.child(pill("branch", &icons::GIT_BRANCH, b, tail).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                ws.open_branch_menu(repo.clone(), MenuAnchor::Key("branch".into()));
                ws.after_input(tx);
            }));
        }
        if let Some((project, cwd, name)) = worktree {
            // Click: the repository's worktrees and a new one (see Workspace::open_worktree_menu).
            left = left.child(pill("worktree", &icons::FOLDER_GIT_2, name, icon(&icons::CHEVRON_DOWN, 12.0, theme::TEXT_FAINT)).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                ws.open_worktree_menu(project.clone(), cwd.clone(), MenuAnchor::Key("worktree".into()));
                ws.after_input(tx);
            }));
        }
        let mut right = row().center_y().gap(if hints { 4.0 } else { 0.0 });
        if hints && session_active {
            right = right.child(key_hint("Shift+E").key("hint-editor"));
        }
        right = right.child(if session_active {
            icon_button(&icons::FILE_CODE).key("toggle-editor").help("Editor mode").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| act(ws, Act::ToggleEditor, tx))
        } else {
            icon_button_disabled(&icons::FILE_CODE).key("toggle-editor")
        });
        if hints {
            right = right.child(key_hint("J").key("hint-shell"));
        }
        right = right.child(if session_active {
            icon_button(&icons::PANEL_BOTTOM).key("toggle-shell").help("Shell").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                if let Some(id) = ws.active.clone() {
                    ws.act(Act::ToggleShell(id));
                }
                ws.after_input(tx);
            })
        } else {
            icon_button_disabled(&icons::PANEL_BOTTOM).key("toggle-shell")
        });
        if hints {
            right = right.child(key_hint("Shift+R").key("hint-right"));
        }
        right = right.child(icon_button(&icons::PANEL_RIGHT).key("toggle-right").help("Toggle right sidebar").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
            ws.toggle_right();
            ws.after_input(tx);
        }));
        // The bar is the window's drag area; buttons inside keep their presses. macOS keeps its traffic lights at the
        // left (`leading`), Windows its caption buttons at the right (`trailing`).
        row()
            .key("topbar")
            .h(theme::TOPBAR_HEIGHT)
            .w_full()
            .center_y()
            .pl(leading.max(0.0) + 10.0)
            .pr(trailing + theme::PANEL_GAP)
            .gap(12.0)
            .window_drag()
            .child(left)
            .child(right)
    }

    fn update_banner(&self) -> Option<El> {
        let (msg, action) = match &self.update.status {
            Status::Available { version } => (format!("Claudiu {version} is available"), Some(("Download", Act::InstallUpdate))),
            Status::Downloading => ("Downloading update…".to_string(), None),
            Status::Ready { version } => (format!("Claudiu {version} is ready"), Some(("Restart to update", Act::InstallUpdate))),
            _ => return None,
        };
        let mut b = row()
            .key("update-banner")
            .h(28.0)
            .w_full()
            .px(10.0)
            .center_y()
            .justify_between()
            .radius(6.0)
            .fill(rgb(0x1d1512))
            .border(1.0, rgb(0x3a2a24))
            .transition(Transition::opacity().combined(Transition::offset(0.0, -6.0)))
            .child(label(msg, 12.0, theme::ACCENT));
        if let Some((l, a)) = action {
            b = b.child(button(l, true).key("update-action").h(22.0).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, a.clone(), tx)));
        } else {
            b = b.child(spinner(12.0, theme::ACCENT));
        }
        Some(b)
    }

    // ------------------------------------------------------------------ the shown session

    fn render_session(&self) -> El {
        let blank = || el().key("pane-blank").w_full().h_full().fill(rgb(theme::TERMINAL_BG));
        let Some(id) = self.active.clone() else { return blank() };
        let Some(view) = self.live_view(&id) else { return blank() };
        let focused = self.terminal_focused(&id);
        // Each session's pane is its own keyed element, so switching sessions cross-fades them.
        col()
            .key(format!("pane-{id}"))
            .w_full()
            .h_full()
            .fill(rgb(theme::TERMINAL_BG))
            .padding(6.0)
            .transition(Transition::opacity())
            .child(view.render(focused, crate::panels::scale::screen()))
    }

    /// The session's shell as its own panel under the main one: same gutter, rounding and grip as the sidebars. It
    /// slides up from the bottom edge.
    fn render_shell(&self) -> El {
        let shown = self.active.clone().filter(|_| self.main == Main::Terminals);
        let panel = shown.as_ref().and_then(|id| self.shells.get(id).map(|p| (id.clone(), p)));
        let Some((id, p)) = panel else { return el().key("shell-slot").h(0.0) };
        let height = if p.open { p.height } else { 0.0 };
        let key = crate::app::shell_key(&id);
        let focused = self.terminal_focused(&key);
        // The top edge is the drag handle, with a small grip in the gutter.
        let handle = row()
            .key("shell-resize")
            .pin(Some(0.0), Some(-(theme::PANEL_GAP / 2.0 + 3.0)), Some(0.0), None)
            .h(6.0)
            .center()
            .cursor(Cursor::ResizeRow)
            .hover_group()
            .on_drag(|ws: &mut Workspace, d: &mut rune::Drag, tx: &mut Tx| {
                match d.phase {
                    Phase::Down => {
                        let height = ws.active.as_ref().and_then(|id| ws.shells.get(id)).map_or(SHELL_HEIGHT, |p| p.height);
                        ws.shell_drag = Some(SideDrag { size: height });
                    }
                    Phase::Move => ws.drag_shell(d.dy),
                    Phase::Up => ws.shell_drag = None,
                }
                ws.after_input(tx);
            })
            .child(el().w(28.0).h(2.0).radius(1.0).fill(rgb(0x2b2b30)).group_color(rgb(0x4a4a52)));
        let inner = col()
            .key("shell-panel")
            .w_full()
            .h(p.height)
            .radius(theme::PANEL_RADIUS)
            .clip_rounded(theme::PANEL_RADIUS)
            .border(1.0, rgb(if focused { 0x34343a } else { theme::PANEL_BORDER }))
            .fill(rgb(theme::TERMINAL_BG))
            .padding(6.0)
            .child(p.view.render(focused, crate::panels::scale::screen()));
        let mut slot = col()
            .key("shell-slot")
            .w_full()
            .h(if p.open { height + theme::PANEL_GAP } else { 0.0 })
            .clip_rounded(0.0)
            .justify_end()
            .motion(motion::GLIDE)
            .child(col().w_full().pt(theme::PANEL_GAP).child(inner).child(handle));
        if self.shell_drag.is_some() {
            slot = slot.instant_changes();
        }
        slot
    }

    // ------------------------------------------------------------------ home

    fn render_home(&self) -> El {
        let shortcut_row = |keys: String, what: &str| row().w_full().justify_between().gap(28.0).child(label(what, 12.0, theme::TEXT_DIM)).child(label(keys, 12.0, theme::TEXT_FAINT));
        let mut recent = col().gap(2.0).mt(26.0).w(420.0);
        // Newest first. Only the five shown are copied out of the store.
        let mut newest: Vec<&crate::store::SessionRecord> = self.store.data.sessions.iter().filter(|s| s.kind == SessionKind::Claude && !s.hidden).collect();
        newest.sort_by_key(|s| std::cmp::Reverse(s.last_active));
        let t = now();
        if !newest.is_empty() {
            recent = recent.child(el().mb(4.0).child(section_label("RECENT")));
        }
        for s in newest.into_iter().take(5) {
            let id = s.id.clone();
            recent = recent.child(
                row()
                    .key(format!("recent-{id}"))
                    .w_full()
                    .justify_between()
                    .px(10.0)
                    .py(6.0)
                    .radius(6.0)
                    .cursor(Cursor::Pointer)
                    .role("button")
                    .hover(Color::WHITE.alpha(0.04))
                    .child(label(s.display_title(), 13.0, theme::TEXT).truncate().shrink())
                    .child(label(ago(s.last_active, t), 11.5, theme::TEXT_FAINT))
                    .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, Act::Select(id.clone()), tx)),
            );
        }
        let new = button_leading(&icons::PLUS, "New", true).key("home-claude").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
            let mut items: Vec<MenuItem> = ws.store.data.projects.iter().map(|p| MenuItem { icon: Some(&icons::FOLDER), ..MenuItem::new(p.name.clone(), Act::NewClaude { cwd: p.path.clone() }) }).collect();
            let sep = !items.is_empty();
            items.push(MenuItem { separator_before: sep, ..MenuItem::new("Add project…", Act::ImportProject) });
            ws.open_menu(MenuAnchor::Key("home-claude".into()), 220.0, items);
            ws.after_input(tx);
        });
        let import = button("Add project", false).key("home-import").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| act(ws, Act::ImportProject, tx));
        col()
            .key("home")
            .w_full()
            .h_full()
            .center()
            .transition(Transition::opacity())
            .child(clawd(72.0))
            .child(text("Claudiu").font(font(22.0, 600.0)).color(rgb(theme::TEXT)).mt(20.0))
            .child(label("A workspace for your Claude Code sessions", 13.0, theme::TEXT_DIM).mt(4.0))
            .child(row().mt(22.0).gap(8.0).child(new).child(import))
            .child(recent)
            .child(
                col()
                    .mt(30.0)
                    .w(260.0)
                    .gap(4.0)
                    .child(shortcut_row(shortcut("Shift+N"), "New code session"))
                    .child(shortcut_row(shortcut("J"), "Show or hide shell"))
                    .child(shortcut_row("Ctrl+Tab".into(), "Next session"))
                    .child(shortcut_row(shortcut("Shift+W"), "End session")),
            )
    }
}

/// A small rounded button in the top bar: an icon, a name, and what follows (a chevron or a spinner).
fn pill(key: &str, glyph: &'static rune::Icon, name: String, tail: El) -> El {
    row()
        .key(key.to_string())
        .gap(4.0)
        .px(7.0)
        .py(1.0)
        .center_y()
        .radius(10.0)
        .fill(rgb(0x26262a))
        .hover(Color::WHITE.alpha(0.05))
        .cursor(Cursor::Pointer)
        .role("button")
        .press_scale(0.97)
        .child(icon(glyph, 12.0, theme::TEXT_DIM))
        .child(label(name, 11.0, theme::TEXT_DIM))
        .child(tail)
}
