//! Switching a running session into another working tree of its repository, the way a session switches branches.
//! The process ends and resumes the same conversation in the target folder; the m4code session (its title, cards,
//! place in the sidebar, and visible conversation) stays intact, with its folder updated to the target's. A new
//! worktree is made with git first (`worktree::add`).
//!
//! For Claude Code, a session that Claude Code itself put in a worktree (`claude --worktree`) has that worktree
//! recorded in its transcript, and Claude Code restores it on every resume, which wins over the folder `claude` is
//! started in. So before such a session moves, m4code appends the line that Claude Code's own ExitWorktree writes
//! (see `claude::clear_recorded_worktree`).

use std::path::Path;

use super::*;
use crate::git::worktree::{self, Base};

const BUSY: &str = "Wait until the session finishes, then switch.";

/// What the background look at a session found before it may be switched.
struct Checked {
    idle: bool,
    uncommitted: usize,
    transcript: Option<PathBuf>,
}

/// Check uncommitted files and, for Claude Code sessions, Claude's registry status and transcript path.
fn inspect(harness: Harness, session_id: Option<&str>, folder: &Path, quiet: bool) -> Checked {
    let uncommitted = worktree::uncommitted(folder).unwrap_or(0);
    if harness == Harness::Claude
        && let Some(claude_id) = session_id
    {
        let home = claude::claude_home();
        let statuses = home.as_deref().map(|h| claude::session_statuses(h, &mut Default::default())).unwrap_or_default();
        return Checked {
            idle: statuses.get(claude_id).map_or(quiet, |s| s == "idle"),
            uncommitted,
            transcript: home.and_then(|h| claude::find_transcript(&h, claude_id)),
        };
    }
    Checked { idle: quiet, uncommitted, transcript: None }
}

/// Ready the target before the session moves: make the worktree when it is new, and for Claude Code clear any
/// worktree recorded in its transcript so resume starts in the target folder. Blocking.
fn prepare(main_checkout: &Path, folder: &Path, base: Base, transcript: Option<&Path>, session_id: Option<&str>) -> Result<(), String> {
    if !folder.exists() {
        worktree::add(main_checkout, folder, base).map_err(|e| format!("could not make the worktree: {}", e.trim()))?;
    }
    if let (Some(t), Some(claude_id)) = (transcript, session_id) {
        clear_worktree_record(t, claude_id)?;
    }
    Ok(())
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
    pub(super) fn switch_shown(&mut self, folder: PathBuf, base: Base, asked: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().cloned() else { return };
        self.switch_session(id, folder, base, asked, window, cx);
    }

    /// Switch session `id` (shown or not) into `folder`.
    pub(super) fn switch_session(&mut self, id: Id, folder: PathBuf, base: Base, asked: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rec) = self.store.session(&id).cloned() else { return };
        let Some(meta) = rec.claude.as_ref() else { return };
        let harness = meta.harness;
        let session_id = rec.claude_id().map(str::to_string);
        let Some(git) = rec.project_id.as_ref().and_then(|p| self.store.project(p)).and_then(|p| p.git.clone()) else { return };
        let Some(view) = self.live_view(&id).filter(|v| v.is_running(cx)).cloned() else {
            self.toast("Resume the session to switch.", false, cx);
            return;
        };
        if self.switching_worktree.contains_key(&id) || view.is_loading(cx) {
            self.toast(BUSY, false, cx);
            return;
        }
        let quiet = !view.is_working(cx);
        let from = git.folder_of(&rec.cwd).unwrap_or(&rec.cwd).to_path_buf();
        // Show the spinner in the branch switcher button while checking git and preparing the worktree.
        self.switching_worktree.insert(id.clone(), folder.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let looked = from.clone();
            let checked = cx.background_executor().spawn(async move { inspect(harness, session_id.as_deref(), &looked, quiet) }).await;
            let _ = this.update_in(cx, |ws, window, cx| ws.after_inspection(id, folder, base, from, asked, checked, window, cx));
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn after_inspection(&mut self, id: Id, folder: PathBuf, base: Base, from: PathBuf, asked: bool, checked: Checked, window: &mut Window, cx: &mut Context<Self>) {
        if !checked.idle {
            self.switching_worktree.remove(&id);
            self.toast(BUSY, false, cx);
            return;
        }
        if checked.uncommitted > 0 && !asked {
            self.switching_worktree.remove(&id);
            let files = if checked.uncommitted == 1 { "1 uncommitted file stays".to_string() } else { format!("{} uncommitted files stay", checked.uncommitted) };
            let name = from.file_name().map_or_else(|| from.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
            self.modal = Some(Modal::Confirm {
                title: "Switch worktree?".into(),
                body: format!("{files} in {name}. Your shell restarts."),
                confirm: "Switch".into(),
                danger: false,
                act: Act::ConfirmSwitch { id, folder, base },
            });
            cx.notify();
            return;
        }
        self.perform_switch(id, folder, base, checked.transcript, window, cx);
    }

    /// Prepare the target folder and move the session into it, preserving its conversation.
    fn perform_switch(&mut self, id: Id, folder: PathBuf, base: Base, transcript: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rec) = self.store.session(&id) else { return };
        let Some(meta) = rec.claude.as_ref() else { return };
        let harness = meta.harness;
        let session_id = rec.claude_id().map(str::to_string);
        let Some(git) = rec.project_id.as_ref().and_then(|p| self.store.project(p)).and_then(|p| p.git.clone()) else { return };
        let main = git.main_checkout().to_path_buf();
        let has_transcript = transcript.is_some();
        cx.spawn_in(window, async move |this, cx| {
            let (target, main_checkout) = (folder.clone(), main.clone());
            let ready = cx
                .background_executor()
                .spawn(async move { prepare(&main_checkout, &target, base, transcript.as_deref(), session_id.as_deref()) })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| match ready {
                Ok(()) => ws.move_into(&id, harness, folder, has_transcript, window, cx),
                Err(why) => {
                    ws.switching_worktree.remove(&id);
                    ws.toast(format!("Couldn't switch: {why}."), true, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The old process ends and a new one resumes the conversation in `folder`, keeping the visible thread entries so
    /// the conversation never disappears mid-switch.
    fn move_into(&mut self, id: &str, harness: Harness, folder: PathBuf, has_transcript: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Closed while the target was readied.
        if !self.is_running(id, cx) {
            self.switching_worktree.remove(id);
            return;
        }
        let prior_entries = self
            .live_view(id)
            .and_then(|v| v.thread())
            .map(|t| t.read(cx).thread.entries.clone())
            .unwrap_or_default();
        if !prior_entries.is_empty() {
            crate::harness::thread::save_entries(id, &prior_entries);
        }
        let has_session_id = self.store.session(id).and_then(|r| r.claude_id()).is_some();
        let fresh = prior_entries.is_empty() && (if harness == Harness::Claude { !has_transcript } else { !has_session_id });

        let was_shown = self.active_id().is_some_and(|a| a == id);
        let shell = self.shells.remove(id).map(|p| (p.open, p.height));
        self.repoint_tabs(id, &folder, window, cx);
        if let Some(r) = self.store.session_mut(id) {
            r.cwd = folder;
        }
        // Replacing the view ends the old process while carrying over the live conversation entries.
        if fresh {
            self.relaunch_empty(id, window, cx);
        } else {
            self.relaunch_with_entries(id, prior_entries, window, cx);
        }
        self.restore_shell(id, shell, window, cx);
        if was_shown {
            self.show_session(id, window, cx);
        }
        self.store.refresh_git();
    }

    /// A shell in the session's folder where there was one, open and as tall as before.
    fn restore_shell(&mut self, id: &str, shell: Option<(bool, f32)>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((open, height)) = shell else { return };
        let Some(rec) = self.store.session(id) else { return };
        let cwd = if rec.cwd.is_dir() { rec.cwd.clone() } else { platform::home_dir().unwrap_or(rec.cwd.clone()) };
        let mut panel = self.new_shell_panel(id, cwd, window, cx);
        (panel.open, panel.height) = (open, height);
        self.shells.insert(id.to_string(), panel);
    }

    /// Open files move to the same path in `folder` when it has the file; a file with unsaved edits, or one the folder
    /// lacks, stays on its old path and a toast says how many.
    fn repoint_tabs(&mut self, id: &str, folder: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get_mut(id).filter(|p| folder.is_dir() && !p.root.as_os_str().is_empty()) else { return };
        let folder = dunce::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
        let (mut moved, mut stayed) = (vec![], 0);
        for tab in &mut pane.tabs {
            let there = tab.path.strip_prefix(&pane.root).ok().map(|rel| folder.join(rel)).filter(|p| p.is_file());
            match there {
                Some(path) if !tab.dirty => {
                    let old = std::mem::replace(&mut tab.path, path.clone());
                    moved.push((old, path));
                }
                _ => stayed += 1,
            }
        }
        for (_, path) in moved {
            self.reload_tab(&id.to_string(), path, window, cx);
        }
        if stayed > 0 {
            let files = if stayed == 1 { "1 open file stays".to_string() } else { format!("{stayed} open files stay") };
            self.toast(format!("{files} on its old path: it has unsaved edits, or the new folder lacks it."), false, cx);
        }
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
