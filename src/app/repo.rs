//! Git from the workspace: git actions off the UI thread, the background fetch and watch, and the branch menu.

use super::*;

impl Workspace {
    pub(super) fn refresh_git_async(&mut self, cx: &mut Context<Self>) {
        self.refresh_git_at(None, cx);
    }

    /// Polls skip repositories already being read. An explicit action supersedes an older read of its checkout.
    pub(super) fn refresh_git_at(&mut self, affected: Option<&std::path::Path>, cx: &mut Context<Self>) {
        let paths: Vec<_> = self.store.data.projects.iter()
            .filter(|p| affected.is_none_or(|cwd| crate::platform::is_same_or_under(cwd, &p.path) || p.git.as_ref().is_some_and(|git| git.folder_of(cwd).is_some())))
            .filter(|p| affected.is_some() || !self.git_refreshes.contains_key(&p.path))
            .map(|p| p.path.clone()).collect();
        if paths.is_empty() { return; }
        self.git_refresh_seq += 1;
        let seq = self.git_refresh_seq;
        for path in &paths { self.git_refreshes.insert(path.clone(), seq); }
        cx.spawn(async move |this, cx| {
            let infos = cx.background_executor().spawn(async move {
                paths.into_iter().map(|path| { let info = crate::git::detect(&path); (path, info) }).collect::<Vec<_>>()
            }).await;
            let _ = this.update(cx, |ws, cx| {
                let mut changed = false;
                for (path, info) in infos {
                    if ws.git_refreshes.get(&path) != Some(&seq) { continue; }
                    ws.git_refreshes.remove(&path);
                    if let Some(project) = ws.store.data.projects.iter_mut().find(|p| p.path == path) && project.git != info {
                        project.git = info;
                        changed = true;
                    }
                }
                if changed { ws.store.mark_dirty(); ws.store.save_if_dirty(); cx.notify(); }
                ws.recover_deleted_worktrees(cx);
            });
        }).detach();
    }

    /// If a live session's linked worktree was deleted externally (by a model or terminal), queue a real switch back
    /// to the main checkout so the agent process isn't left running in a missing folder.
    fn recover_deleted_worktrees(&mut self, cx: &mut Context<Self>) {
        let mut to_recover: Vec<(Id, PathBuf, String)> = vec![];
        for live in &self.live {
            if self.switching_worktree.contains_key(&live.id)
                || self.pending_handoffs.contains_key(&live.id)
                || self.pending_switches.iter().any(|p| p.id == live.id)
                || !live.view.is_running(cx)
            {
                continue;
            }
            let Some(rec) = self.store.session(&live.id).filter(|r| r.kind == SessionKind::Agent) else { continue };
            let Some(git) = rec.project_id.as_ref().and_then(|p| self.store.project(p)).and_then(|p| p.git.as_ref()) else { continue };
            let main = git.main_checkout();
            if crate::platform::path_key(&rec.cwd) == crate::platform::path_key(main) || !main.is_dir() {
                continue;
            }
            let missing_dir = !rec.cwd.is_dir();
            let unregistered_wt = !crate::platform::is_same_or_under(&rec.cwd, main) && git.folder_of(&rec.cwd).is_none();
            if missing_dir || unregistered_wt {
                let name = rec.cwd.file_name().map_or_else(|| "worktree".to_string(), |n| n.to_string_lossy().into_owned());
                to_recover.push((live.id.clone(), main.to_path_buf(), name));
            }
        }
        for (id, main, name) in to_recover {
            self.pending_switches.push(super::cross::PendingSwitch {
                id,
                folder: main,
                base: crate::git::worktree::Base::Main,
                asked: crate::store::now(),
            });
            self.toast(format!("Worktree {name} was removed — returning to main checkout."), cx);
        }
    }

    /// Git actions (see `git/commit.rs`): one at a time, off the UI thread, reported through a toast.
    pub(super) fn run_git(
        &mut self,
        op: GitOp,
        job: impl FnOnce() -> Result<String, crate::git::commit::GitFail> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.git_busy.is_some() {
            return;
        }
        self.git_busy = Some(op);
        let affected = self.active_cwd();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.git_busy = None;
                // The branch shown in the top bar follows a pull, switch or new branch.
                ws.refresh_git_at(Some(&affected), cx);
                ws.store.save_if_dirty();
                ws.refresh_pending(cx);
                // The Worktrees page of Settings lists what a removal just deleted.
                if op == GitOp::Worktree && ws.settings_page == SettingsPage::Projects {
                    ws.load_stored_worktrees(cx);
                }
                match result {
                    Ok(summary) => ws.toast(summary, cx),
                    Err(fail) => match fail.conflict_in {
                        Some(cwd) => {
                            let what = if op == GitOp::Pull { "Pull" } else { "Commit & Push" };
                            ws.show_toast(
                                format!("{what} hit a conflict. Ask an agent to fix it?"),
                                Some(("Ask agent".into(), Act::FixGitWithAgent(cwd))),
                                cx,
                            );
                        }
                        None => ws.toast(fail.msg, cx),
                    },
                }
            });
        })
        .detach();
    }

    /// Re-read, off the UI thread, whether Commit & Push has anything to do in the shown session's folder.
    pub(super) fn refresh_pending(&mut self, cx: &mut Context<Self>) {
        let cwd = self.active_cwd();
        cx.spawn(async move |this, cx| {
            let dir = cwd.clone();
            let pending = cx.background_executor().spawn(async move { crate::git::commit::pending(&dir) }).await;
            let _ = this.update(cx, |ws, cx| {
                // The shown session may have changed meanwhile; its own read replaces this one.
                if ws.active_cwd() != cwd { return; }
                let now = pending.ok().map(|p| (cwd, p));
                if ws.pending != now {
                    ws.pending = now;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Fetch origin for the folder in view once a minute so the branch menu opens instantly and the
    /// remote branches it lists are current. Skipped while another git operation runs.
    pub(super) fn start_fetch_loop(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(cwd) = this.update(cx, |ws, _| (ws.git_busy.is_none()).then(|| ws.active_cwd())) else { break };
                if let Some(cwd) = cwd {
                    cx.background_executor().spawn(async move { crate::git::commit::background_fetch(&cwd) }).await;
                }
                cx.background_executor().timer(Duration::from_secs(60)).await;
            }
        })
        .detach();
    }

    /// Re-read each project's git info every few seconds, off the UI thread, so the branch badge follows a
    /// checkout made in a terminal or by Claude, not only m4code's own branch actions.
    pub(super) fn start_git_watch(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                if this.update(cx, |ws, cx| ws.refresh_git_async(cx)).is_err() { break; }
            }
        })
        .detach();
    }

    /// Ask before deleting a local branch, after counting the commits only it has.
    pub(super) fn ask_delete_branch(&mut self, cwd: PathBuf, name: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (dir, branch) = (cwd.clone(), name.clone());
            let lost = cx.background_executor().spawn(async move { crate::git::commit::commits_only_on(&dir, &branch) }).await;
            let _ = this.update(cx, |ws, cx| match lost {
                Ok(lost) => {
                    let (title, body, confirm, danger) = delete_branch_question(&name, lost);
                    ws.modal = Some(Modal::Confirm { title, body, confirm, danger, act: Act::ConfirmDeleteBranch(cwd, name) });
                    cx.notify();
                }
                Err(e) => ws.toast(e, cx),
            });
        })
        .detach();
    }
}

/// The dialog's title, text, button and whether the button is red. Only the local branch goes; origin's copy stays.
fn delete_branch_question(name: &str, lost: usize) -> (String, String, String, bool) {
    let title = format!("Delete branch {name}?");
    if lost == 0 {
        return (title, "Every commit on it is also on another branch, so nothing is lost. A copy on origin is not deleted.".into(), "Delete".into(), false);
    }
    let commits = if lost == 1 { "1 commit is".to_string() } else { format!("{lost} commits are") };
    (title, format!("{commits} on no other branch and will be lost."), "Delete anyway".into(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleting_a_branch_is_plain_until_commits_would_be_lost() {
        let (title, body, button, red) = delete_branch_question("fix", 0);
        assert_eq!((title.as_str(), button.as_str(), red), ("Delete branch fix?", "Delete", false));
        assert!(body.contains("nothing is lost"), "{body}");

        let (_, body, button, red) = delete_branch_question("fix", 1);
        assert_eq!((body.as_str(), button.as_str(), red), ("1 commit is on no other branch and will be lost.", "Delete anyway", true));
        assert_eq!(delete_branch_question("fix", 3).1, "3 commits are on no other branch and will be lost.");
    }
}
