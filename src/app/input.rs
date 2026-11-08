//! Where input and frames go. Rune hands every handler the workspace and a `Tx`; a terminal is found by its key (the
//! session id, or `shell_key` for the shell under it). Terminals are not Rune text fields: the window's `on_key` sees
//! every key no text field takes and gives it to the terminal in `keys_to`. Each frame `poll` runs background jobs and
//! timers, reads what the terminals' processes printed, and asks for the next frame when something is due.

use std::time::Instant;

use super::*;
use crate::terminal::keys::{Keystroke, Modifiers};

/// How long Ctrl (Cmd) is held alone before the sidebar shows the session shortcuts.
const SHORTCUT_HOLD: Duration = Duration::from_millis(400);

impl Workspace {
    /// The terminal with this key: a session's (its id) or the shell under one (`shell_key`).
    pub fn terminal_mut(&mut self, key: &str) -> Option<&mut TerminalView> {
        if let Some(owner) = key.strip_prefix("shell:") {
            return self.shells.get_mut(owner).map(|p| &mut p.view);
        }
        self.live.iter_mut().find(|l| l.id == key).map(|l| &mut l.view)
    }

    /// Give the keyboard to this terminal: text fields let go of it.
    pub fn focus_terminal(&mut self, key: &str) {
        self.keys_to = Some(key.to_string());
        self.blur = true;
        self.search.focused = false;
        self.menu = None;
    }

    /// Whether this terminal has the keyboard.
    pub fn terminal_focused(&self, key: &str) -> bool {
        self.keys_to.as_deref() == Some(key) && !self.search.focused && self.modal.is_none()
    }

    /// The end of every handler: run what the terminals asked for, let a text field go when a terminal took the
    /// keyboard, and hand the queued `Tx` work (clipboard, dialogs, timers) to the frame.
    pub fn after_input(&mut self, tx: &mut Tx) {
        self.drain_terminals();
        self.drain_editors();
        self.note_edits();
        self.drain_github();
        if std::mem::take(&mut self.blur) {
            tx.blur();
        }
        self.sync_window_title(tx);
        self.flush(tx);
    }

    /// The window's title follows the active session.
    fn sync_window_title(&mut self, tx: &mut Tx) {
        let title = match self.active_id().and_then(|id| self.store.session(id)) {
            Some(s) => format!("{} — Claudiu", s.display_title()),
            None => "Claudiu".to_string(),
        };
        if title != self.window_title {
            tx.set_title(title.clone());
            self.window_title = title;
        }
    }

    /// Run the events each terminal queued, and move the `Tx` work they queued to the workspace's.
    fn drain_terminals(&mut self) {
        let mut events: Vec<(String, TerminalEvent)> = vec![];
        for l in &mut self.live {
            events.extend(l.view.events.drain(..).map(|e| (l.id.clone(), e)));
            for f in l.view.fx.drain(..) {
                self.rt.with_tx(f);
            }
        }
        for (owner, p) in &mut self.shells {
            events.extend(p.view.events.drain(..).map(|e| (shell_key(owner), e)));
            for f in p.view.fx.drain(..) {
                self.rt.with_tx(f);
            }
        }
        for (key, ev) in events {
            self.on_terminal_event(&key, ev);
        }
    }

    /// Move the `Tx` work the editors queued (the clipboard) to the workspace's.
    fn drain_editors(&mut self) {
        let mut requests = vec![];
        for tab in self.panes.values_mut().flat_map(|p| p.tabs.iter_mut()) {
            for f in tab.editor.fx.drain(..) {
                self.rt.with_tx(f);
            }
            requests.extend(tab.editor.requests.drain(..).map(|r| (tab.editor.key.clone(), r)));
        }
        for (key, request) in requests {
            self.serve_editor(&key, request);
        }
    }

    fn on_terminal_event(&mut self, key: &str, ev: TerminalEvent) {
        if let Some(owner) = key.strip_prefix("shell:") {
            if let TerminalEvent::Exited(_) = ev {
                if let Some(err) = self.shells.get(owner).and_then(|p| p.view.start_error()) {
                    self.toast(format!("Could not start the shell: {err}"), true);
                }
                self.shells.remove(owner);
                self.focus_active();
            }
            return;
        }
        match ev {
            TerminalEvent::Exited(code) => self.on_exit(key, code),
            TerminalEvent::Bell => {}
            TerminalEvent::OpenPath(path, line) => self.open_from_terminal(key, &path, line),
            TerminalEvent::SendText(text) => self.send_to_claude(key, &text),
        }
    }

    /// Ctrl+click on a path in a terminal: open it in editor mode beside the session, or for a folder or a file the
    /// editor can't take, in the "Open in" editor or the file manager.
    fn open_from_terminal(&mut self, id: &str, path: &str, line: Option<usize>) {
        if self.open_in_editor_mode(&id.to_string(), path, line) {
            return;
        }
        let base = self.store.session(id).map(|s| s.cwd.clone()).unwrap_or_default();
        let full = base.join(path);
        if !full.exists() {
            self.toast(format!("{path} was not found"), true);
            return;
        }
        match self.preferred_editor() {
            Some(editor) => self.act(Act::OpenEditor(editor, full)),
            None => platform::reveal(&full),
        }
    }

    /// Each frame: run arrived jobs and due timers, read the terminals, and say whether the view must run again.
    pub fn poll(&mut self, tx: &mut Tx) -> bool {
        let mut changed = self.run_jobs();
        changed |= std::mem::take(&mut self.rt.dirty);
        let showing = if self.main == Main::Terminals { self.active.clone() } else { None };
        let foreground = self.window_active;
        let mut next: Option<Instant> = None;
        let mut pump = |view: &mut TerminalView, visible: bool| -> bool {
            view.visible = visible;
            view.foreground = foreground;
            if let Some(at) = view.pump() {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
            std::mem::take(&mut view.dirty) && visible
        };
        for l in &mut self.live {
            changed |= pump(&mut l.view, showing.as_ref() == Some(&l.id));
        }
        for (owner, p) in &mut self.shells {
            changed |= pump(&mut p.view, p.open && showing.as_ref() == Some(owner));
        }
        self.update_picker();
        let (edited, animating) = self.poll_editors();
        changed |= edited;
        if animating {
            let frame = Instant::now() + Duration::from_millis(16);
            next = Some(next.map_or(frame, |n| n.min(frame)));
        }
        if let Some(at) = next {
            tx.wake_after(at.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));
        }
        self.after_input(tx);
        changed
    }

    /// A key no text field took. App shortcuts first, then the modal or menu that is open, then the terminal.
    pub fn on_key(&mut self, ev: &rune::KeyEvent, tx: &mut Tx) -> bool {
        let Some(ks) = Keystroke::from_rune(ev, tx.key_base()) else { return false };
        let used = self.key(&ks, tx);
        self.after_input(tx);
        used
    }

    fn key(&mut self, ks: &Keystroke, tx: &mut Tx) -> bool {
        if let Some(c) = Command::for_key(ks) {
            self.command(c);
            return true;
        }
        let plain = ks.modifiers == Modifiers::default();
        if self.menu.is_some() {
            if plain && ks.key == "escape" {
                self.menu = None;
            }
            return true;
        }
        if self.modal.is_some() {
            match ks.key.as_str() {
                "escape" if plain => self.act(Act::Dismiss),
                "enter" if plain && tx.focused().is_none() => self.submit_modal(),
                _ if matches!(self.modal, Some(Modal::Settings { .. })) => return self.settings_key(ks),
                _ => return false,
            }
            return true;
        }
        if self.search.focused {
            return false;
        }
        if self.keys_in_pane()
            && let Some(c) = commands::PaneCommand::for_key(ks)
        {
            self.pane_command(c);
            return true;
        }
        let Some(key) = self.keys_to.clone() else { return false };
        if let Some(e) = self.editor_mut(&key) {
            return e.on_key(ks, tx);
        }
        match self.terminal_mut(&key) {
            Some(t) => t.on_key(ks, tx),
            None => false,
        }
    }

    /// A paste that reached no text field: the terminal with the keyboard takes it.
    pub fn on_paste(&mut self, content: &rune::ClipboardContent, tx: &mut Tx) {
        if self.search.focused || self.modal.is_some() {
            return;
        }
        let key = self.keys_to.clone().unwrap_or_default();
        if let Some(e) = self.editor_mut(&key) {
            e.paste_text(content.text());
        } else if let Some(t) = self.terminal_mut(&key) {
            t.paste_content(content);
        }
        self.after_input(tx);
    }

    /// The input method's composition and commits go to the terminal with the keyboard.
    pub fn on_ime(&mut self, ime: &rune::Ime, tx: &mut Tx) -> bool {
        if self.search.focused || self.modal.is_some() {
            return false;
        }
        let Some(key) = self.keys_to.clone() else { return false };
        let used = match self.editor_mut(&key) {
            Some(e) => e.ime(ime),
            None => self.terminal_mut(&key).is_some_and(|t| t.ime(ime)),
        };
        self.after_input(tx);
        used
    }

    /// Ctrl (Cmd) alone, held for a moment, shows the sessions' numbers; anything else hides them. Quick chords such as
    /// Ctrl+C never show them, since the key is let go before the wait ends.
    pub fn on_modifiers(&mut self, mods: rune::Mods, tx: &mut Tx) {
        self.shortcuts_wait += 1;
        self.shortcuts_shown = false;
        let secondary = if cfg!(target_os = "macos") { mods.logo } else { mods.ctrl };
        let alone = secondary && !mods.shift && !mods.alt && !(mods.ctrl && mods.logo);
        if alone {
            let wait = self.shortcuts_wait;
            self.rt.after(SHORTCUT_HOLD, move |ws: &mut Workspace| {
                if ws.shortcuts_wait == wait {
                    ws.shortcuts_shown = true;
                }
            });
        }
        self.after_input(tx);
    }

    /// The window came to the front or went behind another.
    pub fn on_window_focus(&mut self, active: bool, tx: &mut Tx) {
        self.window_active = active;
        if !active {
            self.shortcuts_wait += 1;
            self.shortcuts_shown = false;
        } else if let Some(id) = self.right_gh.clone().filter(|_| !self.store.data.layout.right_hidden) {
            // Returning to the window refreshes the GitHub pane on screen.
            if let Some(p) = self.gh.iter_mut().find(|p| p.id == id) {
                p.view.refresh_on_return();
            }
        }
        self.after_input(tx);
    }

    /// Files dropped on the window: onto the shown terminal as paths (Claude Code attaches images), elsewhere folders
    /// become projects.
    pub fn on_drop(&mut self, paths: &[PathBuf], tx: &mut Tx) {
        if let (Main::Terminals, Some(key)) = (self.main, self.keys_to.clone().or(self.active.clone()))
            && let Some(t) = self.terminal_mut(&key)
        {
            t.paste_paths(paths);
        } else {
            for p in paths.iter().filter(|p| p.is_dir()) {
                self.act(Act::AddProject(p.clone()));
            }
        }
        self.after_input(tx);
    }
}
