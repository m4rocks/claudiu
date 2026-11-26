//! Switching a running session into another working tree of its repository, the way a session switches branches.
//! The process ends and resumes the same conversation in the target folder; the m4code session (its title, cards,
//! place in the sidebar, and visible conversation) stays intact, with its folder updated to the target's. A new
//! worktree is made with git first (`worktree::add`).
//!
//! Providers that bind their session to a checkout receive a new native session seeded from the saved conversation.

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
fn inspect(harness: Harness, session_id: Option<&str>, folder: &Path) -> Checked {
    let uncommitted = worktree::uncommitted(folder).unwrap_or(0);
    let (idle, transcript) = session_id.map(|id| (harness.adapter().worktree.inspect)(id)).unwrap_or_default();
    Checked { idle: idle.unwrap_or(true), uncommitted, transcript }
}

fn prepare(harness: Harness, main_checkout: &Path, folder: &Path, base: Base, transcript: Option<&Path>) -> Result<bool, String> {
    if !folder.exists() {
        worktree::add(main_checkout, folder, base).map_err(|e| format!("could not make the worktree: {}", e.trim()))?;
    }
    transcript.map_or(Ok(true), harness.adapter().worktree.can_resume)
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
        let Some(meta) = rec.agent.as_ref() else { return };
        let harness = meta.harness;
        let session_id = rec.provider_session_id().map(str::to_string);
        let Some(git) = rec.project_id.as_ref().and_then(|p| self.store.project(p)).and_then(|p| p.git.clone()) else { return };
        let Some(view) = self.live_view(&id).filter(|v| v.is_running(cx)).cloned() else {
            self.toast("Resume the session to switch.", cx);
            return;
        };
        let Some(thread) = view.thread().cloned() else { return };
        if self.switching_worktree.contains_key(&id) || self.pending_handoffs.contains_key(&id)
            || !thread.read(cx).can_move_workspace()
        {
            self.toast(BUSY, cx);
            return;
        }
        thread.update(cx, |t, cx| t.hold_workspace_switch(true, cx));
        let from = git.folder_of(&rec.cwd).unwrap_or(&rec.cwd).to_path_buf();
        // Show the spinner in the branch switcher button while checking git and preparing the worktree.
        self.switching_worktree.insert(id.clone(), folder.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let looked = from.clone();
            let checked = cx.background_executor().spawn(async move { inspect(harness, session_id.as_deref(), &looked) }).await;
            let _ = this.update_in(cx, |ws, window, cx| ws.after_inspection(id, folder, base, from, asked, checked, window, cx));
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn after_inspection(&mut self, id: Id, folder: PathBuf, base: Base, from: PathBuf, asked: bool, checked: Checked, window: &mut Window, cx: &mut Context<Self>) {
        if self.switching_worktree.get(&id) != Some(&folder) {
            return;
        }
        if !checked.idle {
            self.release_worktree_switch(&id, cx);
            self.toast(BUSY, cx);
            return;
        }
        if checked.uncommitted > 0 && !asked {
            self.release_worktree_switch(&id, cx);
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
        let Some(rec) = self.store.session(&id) else {
            self.release_worktree_switch(&id, cx);
            return;
        };
        let context = rec.agent.as_ref().zip(rec.project_id.as_ref().and_then(|p| self.store.project(p)).and_then(|p| p.git.as_ref()));
        let Some((meta, git)) = context else {
            self.release_worktree_switch(&id, cx);
            return;
        };
        let harness = meta.harness;
        let main = git.main_checkout().to_path_buf();
        let has_transcript = transcript.is_some();
        cx.spawn_in(window, async move |this, cx| {
            let (target, main_checkout) = (folder.clone(), main.clone());
            let ready = cx
                .background_executor()
                .spawn(async move { prepare(harness, &main_checkout, &target, base, transcript.as_deref()) })
                .await;
            let _ = this.update_in(cx, |ws, window, cx| match ready {
                Ok(can_resume) => ws.move_into(&id, folder, has_transcript, can_resume, window, cx),
                Err(why) => {
                    if ws.switching_worktree.get(&id) != Some(&folder) { return; }
                    ws.release_worktree_switch(&id, cx);
                    ws.toast(format!("Couldn't switch: {why}."), cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn release_worktree_switch(&mut self, id: &str, cx: &mut Context<Self>) {
        self.switching_worktree.remove(id);
        if let Some(thread) = self.live_view(id).and_then(|v| v.thread()) {
            thread.update(cx, |t, cx| t.hold_workspace_switch(false, cx));
        }
        cx.notify();
    }

    fn move_into(&mut self, id: &str, folder: PathBuf, has_transcript: bool, can_resume: bool, _window: &mut Window, cx: &mut Context<Self>) {
        if self.switching_worktree.get(id) != Some(&folder) {
            return;
        }
        let Some(source) = self.live_view(id).and_then(|v| v.thread()).cloned() else {
            self.release_worktree_switch(id, cx);
            return;
        };
        if !source.read(cx).can_move_workspace() {
            self.release_worktree_switch(id, cx);
            self.toast(BUSY, cx);
            return;
        }
        let Some(rec) = self.store.session(id).cloned() else {
            self.release_worktree_switch(id, cx);
            return;
        };
        let Some(mut meta) = rec.agent.clone() else {
            self.release_worktree_switch(id, cx);
            return;
        };
        let (has_entries, permission) = {
            let thread = source.read(cx);
            (!thread.thread.entries.is_empty(), thread.permission_for_relaunch())
        };
        let provider_id = (!meta.session_id.is_empty()).then_some(meta.session_id.as_str());
        let has_history = (meta.harness.adapter().worktree.has_history)(provider_id, has_transcript);
        if !can_resume || (!has_entries && !has_history) {
            meta.session_id.clear();
        }
        let seeded_notice = !can_resume && has_entries;
        let Some(mut start) = self.thread_start(id, &folder, &meta, rec.chosen_title().map(String::from), None, Starter::User, cx) else {
            self.release_worktree_switch(id, cx);
            return;
        };
        start.permission = permission.or(start.permission);
        source.update(cx, |t, cx| t.switch_worktree_session(start, folder, seeded_notice, cx));
    }

    pub(super) fn finish_worktree_switch(&mut self, id: &str, folder: PathBuf, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let shell = self.shells.remove(id).map(|p| (p.open, p.height));
        let (chosen_model, chosen_effort, full_access) = self
            .live_view(id)
            .and_then(SessionView::thread)
            .map(|view| {
                let v = view.read(cx);
                (
                    v.thread.option("model").map(|o| o.current.clone()),
                    v.thread.option("thought_level").map(|o| o.current.clone()),
                    v.antigravity_full_access(),
                )
            })
            .unwrap_or((None, None, false));
        if let Some(rec) = self.store.session_mut(id) {
            rec.cwd = folder.clone();
            if let Some(meta) = rec.agent.as_mut() {
                meta.session_id = session_id;
                if let Some(m) = chosen_model { meta.chosen_model = Some(m); }
                if let Some(e) = chosen_effort { meta.chosen_effort = Some(e); }
                meta.read_only_pending = false;
                meta.antigravity_full_access = full_access;
            }
        }
        self.store.mark_dirty();
        self.repoint_tabs(id, &folder, window, cx);
        self.restore_shell(id, shell, window, cx);
        self.release_worktree_switch(id, cx);
        self.refresh_git_at(Some(&folder), cx);
        if self.active_id().is_some_and(|a| a == id) {
            self.sync_github(window, cx);
        }
        cx.notify();
    }

    pub(super) fn fail_worktree_switch(&mut self, id: &str, folder: &Path, error: &str, cx: &mut Context<Self>) {
        if self.switching_worktree.get(id).map(PathBuf::as_path) == Some(folder) {
            self.switching_worktree.remove(id);
            self.toast(format!("Worktree switch failed. The original session is still open. {error}"), cx);
            cx.notify();
        }
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
        let old_root = std::mem::replace(&mut pane.root, folder.clone());
        pane.watcher = None;
        pane.stale = true;
        pane.servers.clear();
        pane.server_gen = pane.server_gen.wrapping_add(1);
        let (mut moved, mut stayed) = (vec![], 0);
        for tab in &mut pane.tabs {
            let there = tab.path.strip_prefix(&old_root).ok().map(|rel| folder.join(rel)).filter(|p| p.is_file());
            match there {
                Some(path) if !tab.dirty && !tab.saving => {
                    Self::tell_server_closed(tab);
                    (tab.path, tab.server, tab.version) = (path.clone(), None, 0);
                    moved.push(path);
                }
                _ => {
                    Self::tell_server_closed(tab);
                    (tab.server, tab.version) = (None, 0);
                    stayed += 1;
                }
            }
        }
        for path in moved {
            self.reload_tab(&id.to_string(), path.clone(), window, cx);
            self.attach_server(&id.to_string(), &path, cx);
        }
        self.watch_folder(&id.to_string(), cx);
        if stayed > 0 {
            let files = if stayed == 1 { "1 open file stays".to_string() } else { format!("{stayed} open files stay") };
            self.toast(format!("{files} on its old path: it has unsaved edits, or the new folder lacks it."), cx);
        }
    }

    /// Follow a native cwd switch (`EnterWorktree` / `ExitWorktree`) at turn end without restarting the agent process.
    fn follow_native_worktree(&mut self, id: &str, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if !folder.is_dir() {
            return;
        }
        let Some(rec) = self.store.session(id) else { return };
        if crate::platform::path_key(&rec.cwd) == crate::platform::path_key(&folder) {
            return;
        }
        let is_main = rec
            .project_id
            .as_ref()
            .and_then(|p| self.store.project(p))
            .and_then(|p| p.git.as_ref())
            .is_some_and(|git| git.is_main(&folder));
        let shell = self.shells.remove(id).map(|p| (p.open, p.height));
        if let Some(rec) = self.store.session_mut(id) {
            rec.cwd = folder.clone();
        }
        self.store.mark_dirty();
        self.store.save_if_dirty();
        if let Some(thread) = self.live_view(id).and_then(SessionView::thread).cloned() {
            thread.update(cx, |t, cx| t.follow_workspace(folder.clone(), cx));
        }
        self.repoint_tabs(id, &folder, window, cx);
        self.restore_shell(id, shell, window, cx);
        self.refresh_git_at(Some(&folder), cx);
        if self.active_id().is_some_and(|a| a == id) {
            self.show_session(id, window, cx);
        }
        let notice = if is_main {
            "Returned to main checkout.".to_string()
        } else {
            let name = folder.file_name().map_or_else(|| "worktree".to_string(), |n| n.to_string_lossy().into_owned());
            format!("Followed agent into worktree {name}.")
        };
        self.toast(notice, cx);
    }

    /// At turn end, follow Claude Code's native `EnterWorktree`/`ExitWorktree` or surface an actionable toast when
    /// the agent edited files in another worktree of the same repository (without mutating `rec.cwd`).
    pub(super) fn check_turn_worktree_activity(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.live_view(id).and_then(SessionView::thread).cloned() else { return };
        let (native_changed, edited_paths) = thread.update(cx, |t, _| {
            (t.take_native_worktree_changed(), t.take_turn_edited_paths())
        });
        if self.switching_worktree.contains_key(id)
            || self.pending_handoffs.contains_key(id)
            || self.pending_switches.iter().any(|p| p.id == id)
        {
            return;
        }
        let Some(rec) = self.store.session(id).cloned() else { return };
        let Some(git) = rec.project_id.as_ref().and_then(|p| self.store.project(p)).and_then(|p| p.git.clone()) else { return };

        if native_changed && rec.agent.as_ref().is_some_and(|m| m.harness == Harness::Claude) {
            let Some(session_id) = rec.provider_session_id().map(str::to_string) else { return };
            let main = git.main_checkout().to_path_buf();
            let id = id.to_string();
            cx.spawn_in(window, async move |this, cx| {
                let target = cx
                    .background_executor()
                    .spawn(async move {
                        let home = crate::harness::claude::claude_home()?;
                        let transcript = crate::harness::claude::find_transcript(&home, &session_id)?;
                        match crate::harness::claude::worktree_session_state(&transcript).ok()?? {
                            crate::harness::claude::WorktreeState::Entered(Some(path)) if path.is_dir() => Some(path),
                            crate::harness::claude::WorktreeState::Exited if main.is_dir() => Some(main),
                            _ => None,
                        }
                    })
                    .await;
                if let Some(folder) = target {
                    let _ = this.update_in(cx, |ws, window, cx| ws.follow_native_worktree(&id, folder, window, cx));
                }
            })
            .detach();
            return;
        }

        if let Some((other, count)) = other_worktree_edit_target(&git, &rec.cwd, &edited_paths) {
            let label = worktree::label(&git, &other).unwrap_or_else(|| {
                other.file_name().map_or_else(|| "worktree".to_string(), |n| n.to_string_lossy().into_owned())
            });
            let files = if count == 1 { "1 file".to_string() } else { format!("{count} files") };
            self.show_toast(
                format!("Agent edited {files} in {label}."),
                Some(("Switch".into(), Act::SwitchSessionWorktree { id: id.to_string(), folder: other, base: Base::Main })),
                cx,
            );
        }
    }
}

/// If the turn's edited files landed in another linked worktree of `git` than `cwd`, return the worktree with the most
/// edited files and its count. Never matches the current worktree or files outside the repository.
fn other_worktree_edit_target(git: &crate::git::GitInfo, cwd: &Path, edited_paths: &[PathBuf]) -> Option<(PathBuf, usize)> {
    let here = git.folder_of(cwd).map(crate::platform::path_key);
    let mut counts: HashMap<String, (PathBuf, usize)> = HashMap::new();
    for path in edited_paths {
        if !path.is_absolute() {
            continue;
        }
        let Some(folder) = git.folder_of(path) else { continue };
        let key = crate::platform::path_key(folder);
        if here.as_deref() == Some(key.as_str()) {
            continue;
        }
        counts.entry(key).and_modify(|(_, n)| *n += 1).or_insert_with(|| (folder.to_path_buf(), 1));
    }
    counts.into_values().max_by_key(|(_, n)| *n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_bound_transcripts_are_left_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let transcript = r#"{"type":"worktree-state","worktreeSession":{"worktreeName":"before"},"sessionId":"s"}"#;
        std::fs::write(&path, transcript).unwrap();
        let can_resume = Harness::Claude.adapter().worktree.can_resume;
        assert!(!can_resume(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), transcript);
        assert!(can_resume(&dir.path().join("absent.jsonl")).is_err());
    }

    #[test]
    fn other_worktree_edit_target_picks_another_worktree_with_most_edits() {
        let git = crate::git::GitInfo {
            root: "/repo".into(),
            main: "/repo".into(),
            worktrees: vec!["/wt/alpha".into(), "/wt/beta".into()],
            ..Default::default()
        };
        let cwd = Path::new("/repo");
        // Edits in current worktree or outside repo produce nothing.
        assert_eq!(other_worktree_edit_target(&git, cwd, &["/repo/a.rs".into(), "/other/b.rs".into()]), None);
        // Edits in another worktree pick the one with the most files.
        let edited = vec![
            "/repo/a.rs".into(),
            "/wt/alpha/one.rs".into(),
            "/wt/beta/one.rs".into(),
            "/wt/beta/two.rs".into(),
        ];
        assert_eq!(other_worktree_edit_target(&git, cwd, &edited), Some((PathBuf::from("/wt/beta"), 2)));
    }
}
