//! Sending a message to a running Claude session from elsewhere in the app (GitHub's Ask Claude, Fix with Claude, a
//! card's button). It goes through the session's mod, which sends it once the session is idle, so nothing is typed
//! into the terminal and a half-written prompt there is left alone.

use std::path::Path;

use super::*;

impl Workspace {
    /// The shown session when it is a running Claude session: where a request from anywhere in the app goes, rather than
    /// into a session of its own.
    pub(super) fn running_claude(&self) -> Option<Id> {
        self.active_id().filter(|id| self.is_running(id) && self.store.session(id).is_some_and(|r| r.kind == SessionKind::Claude)).cloned()
    }

    /// `running_claude`, when it works in `folder` (or inside it, or around it). A request about another repository,
    /// or another worktree of this one, must not land in a session that would act on the wrong files.
    pub(super) fn running_claude_in(&self, folder: &Path) -> Option<Id> {
        self.running_claude().filter(|id| self.store.session(id).is_some_and(|r| works_in(&r.cwd, folder)))
    }

    /// Ask the Claude session `id` to take `text` as the user's next message, then move the keyboard to its terminal.
    pub(super) fn send_to_claude(&mut self, id: &str, text: &str) {
        if !self.is_running(id) {
            self.toast("This session isn't running. Resume it, then try again.", false);
            return;
        }
        usage::send_ctl(&usage::ctl_file(id), "prompt", text.into());
        self.focus_active();
        // The request names its lines already.
        self.focus_without_reference();
    }
}

/// Whether a session started in `session` is working on `folder`.
fn works_in(session: &Path, folder: &Path) -> bool {
    crate::platform::is_same_or_under(session, folder) || crate::platform::is_same_or_under(folder, session)
}

#[cfg(test)]
mod tests {
    use super::works_in;
    use std::path::Path;

    #[test]
    fn a_session_works_in_its_folder_and_those_around_and_inside_it_only() {
        assert!(works_in(Path::new("/p/app"), Path::new("/p/app")));
        assert!(works_in(Path::new("/p/app/crates/x"), Path::new("/p/app")), "started in a sub-folder of the repository");
        assert!(works_in(Path::new("/p"), Path::new("/p/app")), "started above the repository");
        assert!(!works_in(Path::new("/p/other"), Path::new("/p/app")));
        assert!(!works_in(Path::new("/p/app-wt"), Path::new("/p/app")), "another worktree is not this one");
    }
}
