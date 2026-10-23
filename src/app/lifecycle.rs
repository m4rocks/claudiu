//! Ending and starting Claudiu: the quit question, remembering what was open, and bringing it back.

use super::*;

/// The ids of the sessions that were open, in order.
fn running_file() -> PathBuf {
    datadir::data_dir().join("running.json")
}

impl Workspace {
    /// Called by the window's close handler. Returns true when it is fine to close right now: asks first only
    /// while a Claude session is mid-turn; idle sessions just end with the app.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.quitting {
            return true;
        }
        let working = self.live.iter().filter(|l| {
            let v = l.view.read(cx);
            v.is_running() && v.is_working() && self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude)
        });
        let working = working.count();
        if working == 0 {
            self.finalize(cx);
            return true;
        }
        self.modal = Some(Modal::Confirm {
            title: "Quit Claudiu?".into(),
            body: format!(
                "{working} Claude session{} still working. Quitting interrupts {} — sessions stay in your history and can be resumed.",
                if working == 1 { " is" } else { "s are" },
                if working == 1 { "it" } else { "them" }
            ),
            confirm: "Quit anyway".into(),
            danger: true,
            act: Act::ConfirmQuit,
        });
        window.focus(&self.modal_focus, cx);
        cx.notify();
        false
    }

    /// The sessions that are open now, in order.
    fn open_ids(&self, cx: &App) -> Vec<Id> {
        self.live.iter().filter(|l| self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude) && self.is_running(&l.id, cx)).map(|l| l.id.clone()).collect()
    }

    /// Remember what is open, so the next start brings it back (also while running, in case Claudiu does not get to
    /// quit). It has its own file: another Claudiu sharing the data folder rewrites `state.json` without knowing this
    /// list.
    pub(super) fn remember_open(&mut self, cx: &App) {
        if !self.restored || self.quitting {
            return;
        }
        let ids = self.open_ids(cx);
        if self.remembered != ids {
            let _ = datadir::write_atomic(&running_file(), &serde_json::json!(ids).to_string());
            self.remembered = ids;
        }
    }

    /// Bring back what was open when Claudiu last ended, then show what was on screen. (Older versions also listed
    /// chats here; those ids match no session and are skipped.)
    pub(super) fn restore_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved: Vec<Id> = std::fs::read_to_string(running_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let shown = self.selected.clone();
        let mut back = vec![];
        for id in saved {
            if self.store.session(&id).is_some_and(|s| s.kind == SessionKind::Claude && !s.hidden) {
                self.resume(&id, window, cx);
                back.push(id);
            }
        }
        if let Some(id) = shown.filter(|s| back.contains(s)) {
            self.show_session(&id, window, cx);
        }
        self.restored = true;
        self.remember_open(cx);
    }

    fn screen_file(id: &str) -> PathBuf {
        crate::datadir::data_dir().join("screens").join(format!("{}.txt", crate::datadir::safe_file_stem(id)))
    }

    /// Keep the end of a session's screen, to show while its resume starts.
    pub(super) fn save_screen(&self, id: &str, cx: &App) {
        let Some(view) = self.live_view(id) else { return };
        let text = view.read(cx).tail_text(40);
        if !text.trim().is_empty() {
            let _ = crate::datadir::write_atomic(&Self::screen_file(id), &text);
        }
    }

    pub(super) fn load_screen(id: &str) -> Option<String> {
        std::fs::read_to_string(Self::screen_file(id)).ok().filter(|t| !t.trim().is_empty())
    }

    pub fn finalize(&mut self, cx: &App) {
        self.remember_open(cx);
        for l in &self.live {
            self.save_screen(&l.id, cx);
        }
        self.quitting = true;
        let t = now();
        let running: Vec<Id> = self.live.iter().map(|l| l.id.clone()).collect();
        for id in running {
            if let Some(rec) = self.store.session_mut(&id)
                && rec.exit_code.is_none() {
                    rec.exit_code = Some(-1);
                    rec.last_active = t;
                }
        }
        // Terminal sessions are never persisted, nor are ephemeral ones.
        let ephemeral = &self.ephemeral;
        self.store
            .data
            .sessions
            .retain(|s| s.kind != SessionKind::Shell && !ephemeral.contains(&s.id));
        self.store.save();
    }
}
