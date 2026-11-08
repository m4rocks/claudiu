//! Panels: the shell under the shown session, the left and right sidebars, their sizes, and remembering them.

use super::*;

impl Workspace {
    /// Show or hide the shell under a Claude session. It starts on first use, in the session's folder, and lives and
    /// dies with the session.
    pub fn toggle_shell(&mut self, id: &str) {
        let rec = self.store.session(id).filter(|r| r.kind == SessionKind::Claude).cloned();
        let Some(rec) = rec.filter(|_| self.is_running(id)) else {
            self.toast("Open a Claude session to use its shell.", false);
            return;
        };
        // From another session the shell is brought up with its session; only on the shown one does it toggle.
        let shown = self.active.as_deref() == Some(id) && matches!(self.main, Main::Terminals);
        if !shown {
            self.show_session(id);
        }
        if let Some(p) = self.shells.get_mut(id) {
            p.open = !shown || !p.open;
            if !p.open {
                self.focus_active();
                return;
            }
        } else {
            // A session's folder may be gone (temp dirs, deleted worktrees); CreateProcess then fails with "filename syntax is incorrect".
            let cwd = if rec.cwd.is_dir() { rec.cwd.clone() } else { platform::home_dir().unwrap_or(rec.cwd.clone()) };
            let panel = self.new_shell_panel(id, cwd);
            self.shells.insert(id.to_string(), panel);
        }
        self.focus_shell();
    }

    /// A shell for the session, started in `cwd`. When the shell ends, its panel goes away (`on_terminal_event`), with a
    /// toast if it never started.
    pub(super) fn new_shell_panel(&mut self, id: &str, cwd: PathBuf) -> ShellPanel {
        let (program, args) = self.shell.clone();
        let spec = SpawnSpec { program, args, cwd: Some(cwd), ..Default::default() };
        let (terminal, rx) = TerminalView::spawn(&spec);
        ShellPanel { view: TerminalView::new(terminal, rx, shell_key(id)), open: true, height: SHELL_HEIGHT }
    }

    fn focus_shell(&mut self) {
        if let Some(id) = self.active_id().filter(|id| self.shells.get(*id).is_some_and(|p| p.open)).cloned() {
            self.focus_terminal(&shell_key(&id));
        }
    }

    /// Move the keyboard between the Claude terminal and its open shell.
    pub fn swap_focus(&mut self) {
        let Some(id) = self.active_id().cloned() else { return };
        if !self.shells.get(&id).is_some_and(|p| p.open) {
            return;
        }
        if self.terminal_focused(&shell_key(&id)) {
            self.focus_active();
        } else {
            self.focus_shell();
        }
    }

    /// The divider above the shell panel moved `dy` points since the drag began: follow it, keeping room for the terminal.
    pub fn drag_shell(&mut self, dy: f32) {
        let Some(d) = self.shell_drag else { return };
        let window_h = self.window_size.1;
        let height = (d.size - dy).clamp(SHELL_MIN, (window_h - 200.0).max(SHELL_MIN));
        if let Some(p) = self.active.clone().and_then(|id| self.shells.get_mut(&id)) {
            p.height = height;
        }
    }

    pub(super) fn persist_layout(&mut self) {
        self.store.data.layout.selected_session = self.selected.clone();
        self.store.mark_dirty();
    }

    /// In editor mode the sidebar is hidden whatever the remembered setting, so asking for it leaves editor mode and shows it.
    pub fn toggle_left(&mut self) {
        let editing = self.editing();
        let layout = &mut self.store.data.layout;
        layout.sidebar_visible = editing || !layout.sidebar_visible;
        self.editor_mode = false;
        self.store.mark_dirty();
    }

    pub fn toggle_right(&mut self) {
        let editing = self.editing();
        let layout = &mut self.store.data.layout;
        layout.right_hidden = !editing && !layout.right_hidden;
        self.editor_mode = false;
        self.store.mark_dirty();
        self.focus_active();
    }

    /// GitHub is read and written in, so it gets a wider, resizable width.
    fn right_wide(&self) -> bool {
        self.right_gh.is_some()
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

    /// The right sidebar's left edge moved `dx` points since the drag began: follow it, wider to the left.
    pub fn drag_right(&mut self, dx: f32) {
        let Some(d) = self.right_drag else { return };
        let window_w = self.window_size.0;
        let wanted = d.size - dx;
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

    /// The left sidebar's right edge moved `dx` points since the drag began: follow it, wider to the right.
    pub fn drag_left(&mut self, dx: f32) {
        let Some(d) = self.left_drag else { return };
        let window_w = self.window_size.0;
        let wanted = d.size + dx;
        self.store.data.layout.left_w = Some(wanted.clamp(SIDEBAR_MIN, SIDEBAR_MAX.min((window_w - 560.0).max(SIDEBAR_MIN))));
        self.store.mark_dirty();
    }
}
