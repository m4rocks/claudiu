//! Panels: the shell under the shown session, the left and right sidebars, their sizes, and remembering them.

use super::*;

impl Workspace {
    /// Show or hide the shell under a Claude session. It starts on first use, in the session's folder, and lives and
    /// dies with the session.
    pub fn toggle_shell(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let rec = self.store.session(id).filter(|r| r.kind == SessionKind::Claude).cloned();
        let Some(rec) = rec.filter(|_| self.is_running(id, cx)) else {
            self.toast("Open a Claude session to use its shell.", false, cx);
            return;
        };
        // From another session the shell is brought up with its session; only on the shown one does it toggle.
        let shown = self.active.as_deref() == Some(id) && matches!(self.main, Main::Terminals);
        if !shown {
            self.show_session(id, window, cx);
        }
        if let Some(p) = self.shells.get_mut(id) {
            p.open = !shown || !p.open;
            if !p.open {
                self.focus_active(window, cx);
                cx.notify();
                return;
            }
        } else {
            let (program, args) = self.shell.clone();
            // A session's folder may be gone (temp dirs, deleted worktrees); CreateProcess then fails with "filename syntax is incorrect".
            let cwd = if rec.cwd.is_dir() { rec.cwd.clone() } else { platform::home_dir().unwrap_or(rec.cwd.clone()) };
            let spec = SpawnSpec { program, args, cwd: Some(cwd), ..Default::default() };
            let (terminal, rx) = TerminalView::spawn(&spec);
            let view = cx.new(|cx| TerminalView::new(terminal, rx, cx));
            let owner = id.to_string();
            let sub = cx.subscribe_in(&view, window, move |ws: &mut Workspace, view, ev: &TerminalEvent, window, cx| {
                if matches!(ev, TerminalEvent::Exited(_)) {
                    if let Some(err) = view.read(cx).start_error() {
                        ws.toast(format!("Could not start the shell: {err}"), true, cx);
                    }
                    ws.shells.remove(&owner);
                    ws.focus_active(window, cx);
                    cx.notify();
                }
            });
            self.shells.insert(id.to_string(), ShellPanel { view, _sub: sub, open: true, height: SHELL_HEIGHT });
        }
        self.focus_shell(window, cx);
        cx.notify();
    }

    fn focus_shell(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.active_id().and_then(|id| self.shells.get(id)).filter(|p| p.open) {
            window.focus(&p.view.read(cx).focus_handle(cx), cx);
        }
    }

    /// Move the keyboard between the Claude terminal and its open shell.
    pub fn swap_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shell) = self.active_id().and_then(|id| self.shells.get(id)).filter(|p| p.open) else { return };
        if shell.view.read(cx).focus_handle(cx).is_focused(window) {
            self.focus_active(window, cx);
        } else {
            self.focus_shell(window, cx);
        }
    }

    /// The divider above the shell panel is being dragged: follow the pointer, keeping room for the terminal.
    pub fn drag_shell(&mut self, y: f32, window_h: f32) {
        let Some(d) = self.shell_drag.as_ref() else { return };
        let height = (d.height + d.y - y).clamp(SHELL_MIN, (window_h - 200.0).max(SHELL_MIN));
        if let Some(p) = self.shells.get_mut(&d.id) {
            p.height = height;
        }
    }

    pub(super) fn persist_layout(&mut self) {
        self.store.data.layout.selected_session = self.selected.clone();
        self.store.mark_dirty();
    }

    /// In editor mode the sidebar is hidden whatever the remembered setting, so asking for it leaves editor mode and shows it.
    pub fn toggle_left(&mut self, cx: &mut Context<Self>) {
        let editing = self.editing();
        let layout = &mut self.store.data.layout;
        layout.sidebar_visible = editing || !layout.sidebar_visible;
        self.editor_mode = false;
        self.store.mark_dirty();
        cx.notify();
    }

    pub fn toggle_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editing = self.editing();
        let layout = &mut self.store.data.layout;
        layout.right_hidden = !editing && !layout.right_hidden;
        self.editor_mode = false;
        self.store.mark_dirty();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// GitHub and the Visual panel are read and written in, so they share one wider, resizable width.
    fn right_wide(&self) -> bool {
        self.right_gh.is_some() || self.visual_shown()
    }

    /// The right sidebar's current width: the remembered one for the menu or for GitHub, kept within bounds.
    #[cfg(test)]
    pub fn right_width(&self, window_w: f32) -> f32 {
        self.right_width_of(self.right_wide(), window_w)
    }

    pub fn right_width_of(&self, wide: bool, window_w: f32) -> f32 {
        let layout = &self.store.data.layout;
        let room = (window_w - 560.0).max(RIGHT_GH_MIN);
        if !wide {
            return layout.right_menu_w.unwrap_or(RIGHT_MENU_W).clamp(RIGHT_MENU_MIN, RIGHT_MENU_MAX.min(room));
        }
        layout.right_gh_w.unwrap_or(RIGHT_GH_W).clamp(RIGHT_GH_MIN, room)
    }

    /// The right sidebar's left edge is being dragged: follow the pointer, wider to the left.
    pub fn drag_right(&mut self, x: f32, window_w: f32) {
        let Some(d) = self.right_drag.as_ref() else { return };
        let wanted = d.width + d.x - x;
        let room = (window_w - 560.0).max(RIGHT_GH_MIN);
        let wide = self.right_wide();
        let layout = &mut self.store.data.layout;
        if !wide {
            layout.right_menu_w = Some(wanted.clamp(RIGHT_MENU_MIN, RIGHT_MENU_MAX.min(room)));
        } else {
            layout.right_gh_w = Some(wanted.clamp(RIGHT_GH_MIN, room));
        }
        self.store.mark_dirty();
    }

    /// The left sidebar's current width, kept within bounds and leaving room for the session.
    pub fn left_width(&self, window_w: f32) -> f32 {
        let wanted = self.store.data.layout.left_w.unwrap_or(SIDEBAR_W);
        wanted.clamp(SIDEBAR_MIN, SIDEBAR_MAX.min((window_w - 560.0).max(SIDEBAR_MIN)))
    }

    /// The left sidebar's right edge is being dragged: follow the pointer, wider to the right.
    pub fn drag_left(&mut self, x: f32, window_w: f32) {
        let Some(d) = self.left_drag.as_ref() else { return };
        let wanted = d.width + x - d.x;
        self.store.data.layout.left_w = Some(wanted.clamp(SIDEBAR_MIN, SIDEBAR_MAX.min((window_w - 560.0).max(SIDEBAR_MIN))));
        self.store.mark_dirty();
    }
}
