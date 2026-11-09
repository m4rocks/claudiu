//! Switching a running session into another working tree of its repository, the way a session switches branches.
//! The process ends and `claude --resume` continues the same conversation in the target folder; the Claudiu session
//! (its title, cards, place in the sidebar) is the same record, whose folder is now the target's. A new worktree is
//! made with git first (`worktree::add`).
//!
//! A session that Claude Code itself put in a worktree (`claude --worktree`) has that worktree recorded in its
//! transcript, and Claude Code restores it on every resume, which wins over the folder `claude` is started in. So
//! before such a session moves, Claudiu appends the line that Claude Code's own ExitWorktree writes (see
//! `claude::clear_recorded_worktree`).

use std::path::Path;
use std::time::Instant;

use super::*;
use crate::git::GitInfo;
use crate::git::worktree::{self, Base};
use crate::terminal::view::Handover;

const BUSY: &str = "Wait until Claude finishes, then switch.";

/// What the skeleton says while the session moves to `folder`.
fn switching_message(git: &GitInfo, folder: &Path) -> String {
    match folder.file_name() {
        Some(name) if !git.is_main(folder) => format!("Switching to {}…", name.to_string_lossy()),
        _ => "Switching to the main checkout…".into(),
    }
}

/// What the background look at a session found before it may be switched.
struct Checked {
    idle: bool,
    uncommitted: usize,
    transcript: Option<PathBuf>,
}

/// Claude Code's registry says whether the session is busy, idle or waiting; without an entry (older versions) the
/// terminal's own activity decides.
fn inspect(claude_id: &str, folder: &Path, quiet: bool) -> Checked {
    let home = claude::claude_home();
    let statuses = home.as_deref().map(|h| claude::session_statuses(h, &mut Default::default())).unwrap_or_default();
    Checked {
        idle: statuses.get(claude_id).map_or(quiet, |s| s == "idle"),
        uncommitted: worktree::uncommitted(folder).unwrap_or(0),
        transcript: home.and_then(|h| claude::find_transcript(&h, claude_id)),
    }
}

/// Ready the target before the session moves: make the worktree when it is new, and clear a worktree Claude Code
/// recorded, so the resume starts in the folder `claude` is launched in. Blocking.
fn prepare(main_checkout: &Path, folder: &Path, base: Base, transcript: Option<&Path>, claude_id: &str) -> Result<(), String> {
    if !folder.exists() {
        worktree::add(main_checkout, folder, base).map_err(|e| format!("could not make the worktree: {}", e.trim()))?;
    }
    // Without a conversation there is nothing recorded to clear.
    transcript.map_or(Ok(()), |t| clear_worktree_record(t, claude_id))
}

fn clear_worktree_record(transcript: &Path, claude_id: &str) -> Result<(), String> {
    if claude::worktree_recorded(transcript).map_err(|e| format!("could not read the conversation: {e}"))? {
        claude::clear_recorded_worktree(transcript, claude_id).map_err(|e| format!("could not update the conversation: {e}"))?;
    }
    Ok(())
}

impl Workspace {
    /// Switch the shown session into `folder`, a working tree of its project. `asked` is set once the user has
    /// confirmed leaving uncommitted files behind.
    pub(super) fn switch_shown(&mut self, folder: PathBuf, base: Base, asked: bool) {
        let Some(id) = self.active_id().cloned() else { return };
        self.switch_session(id, folder, base, asked);
    }

    /// Switch session `id` (shown or not) into `folder`.
    pub(super) fn switch_session(&mut self, id: Id, folder: PathBuf, base: Base, asked: bool) {
        let Some(rec) = self.store.session(&id).cloned() else { return };
        let Some(claude_id) = rec.claude_id().map(str::to_string) else { return };
        let Some(git) = rec.project_id.as_ref().and_then(|p| self.store.project(p)).and_then(|p| p.git.clone()) else { return };
        let Some((loading, working)) = self.live_view(&id).filter(|v| v.is_running()).map(|v| (v.is_loading(), v.is_working())) else {
            self.toast("Resume the session to switch.", false);
            return;
        };
        // The skeleton is up while a switch readies its target, so this also refuses a second switch.
        if loading {
            self.toast(BUSY, false);
            return;
        }
        let quiet = !working;
        let from = git.folder_of(&rec.cwd).unwrap_or(&rec.cwd).to_path_buf();
        // Looking at the session takes a moment (git, the registry, the transcript): the skeleton answers the click at once.
        self.set_handover(&id, Some(Handover { message: switching_message(&git, &folder), since: Instant::now() }));
        let looked = from.clone();
        self.rt.spawn(
            move || inspect(&claude_id, &looked, quiet),
            move |ws: &mut Workspace, checked| ws.after_inspection(id, folder, base, from, asked, checked),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn after_inspection(&mut self, id: Id, folder: PathBuf, base: Base, from: PathBuf, asked: bool, checked: Checked) {
        if !checked.idle {
            self.set_handover(&id, None);
            self.toast(BUSY, false);
            return;
        }
        if checked.uncommitted > 0 && !asked {
            self.set_handover(&id, None);
            let files = if checked.uncommitted == 1 { "1 uncommitted file stays".to_string() } else { format!("{} uncommitted files stay", checked.uncommitted) };
            let name = from.file_name().map_or_else(|| from.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
            self.modal = Some(Modal::Confirm {
                title: "Switch worktree?".into(),
                body: format!("{files} in {name}. Your shell restarts."),
                confirm: "Switch".into(),
                danger: false,
                act: Act::ConfirmSwitch { id, folder, base },
            });
            return;
        }
        self.perform_switch(id, folder, base, checked.transcript);
    }

    /// `transcript` is None for a session that has no conversation yet: it starts over in the folder, as a new session.
    fn perform_switch(&mut self, id: Id, folder: PathBuf, base: Base, transcript: Option<PathBuf>) {
        let Some(rec) = self.store.session(&id) else { return };
        let Some(claude_id) = rec.claude_id().map(str::to_string) else { return };
        let Some(git) = rec.project_id.as_ref().and_then(|p| self.store.project(p)).and_then(|p| p.git.clone()) else { return };
        let main = git.main_checkout().to_path_buf();
        let message = switching_message(&git, &folder);
        let fresh = transcript.is_none();
        let (target, main_checkout) = (folder.clone(), main.clone());
        self.rt.spawn(
            move || prepare(&main_checkout, &target, base, transcript.as_deref(), &claude_id),
            move |ws: &mut Workspace, ready| match ready {
                Ok(()) => ws.move_into(&id, folder, message, fresh),
                Err(why) => {
                    ws.set_handover(&id, None);
                    ws.toast(format!("Couldn't switch: {why}."), true);
                }
            },
        );
    }

    /// The old process ends and a new one resumes the conversation in `folder`, as a restart does; `fresh` when there is
    /// no conversation to resume.
    fn move_into(&mut self, id: &str, folder: PathBuf, message: String, fresh: bool) {
        // Closed while the target was readied.
        if !self.is_running(id) {
            return;
        }
        let was_shown = self.active_id().is_some_and(|a| a == id);
        let shell = self.shells.remove(id).map(|p| (p.open, p.height));
        if let Some(r) = self.store.session_mut(id) {
            r.cwd = folder;
        }
        // Replacing the view ends the old process.
        if fresh {
            self.relaunch_empty(id);
        } else {
            self.relaunch(id);
        }
        self.set_handover(id, Some(Handover { message, since: Instant::now() }));
        self.restore_shell(id, shell);
        if was_shown {
            self.show_session(id);
        }
        self.store.refresh_git();
    }

    /// A shell in the session's folder where there was one, open and as tall as before.
    fn restore_shell(&mut self, id: &str, shell: Option<(bool, f32)>) {
        let Some((open, height)) = shell else { return };
        let Some(rec) = self.store.session(id) else { return };
        let cwd = if rec.cwd.is_dir() { rec.cwd.clone() } else { platform::home_dir().unwrap_or(rec.cwd.clone()) };
        let mut panel = self.new_shell_panel(id, cwd);
        (panel.open, panel.height) = (open, height);
        self.shells.insert(id.to_string(), panel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_appends_one_line_only_while_a_worktree_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, r#"{"type":"worktree-state","worktreeSession":{"worktreeName":"t"},"sessionId":"s"}"#.to_string() + "\n").unwrap();
        // A worktree Claude Code recorded: the line is appended.
        clear_worktree_record(&path, "s").unwrap();
        assert!(!claude::worktree_recorded(&path).unwrap());
        // Nothing recorded, nothing appended.
        let before = std::fs::read_to_string(&path).unwrap();
        clear_worktree_record(&path, "s").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // A missing transcript is an error the user is told about.
        assert!(clear_worktree_record(&dir.path().join("gone.jsonl"), "s").is_err());
    }
}
