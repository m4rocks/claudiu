//! Git from the workspace: git actions off the UI thread, the background fetch and watch, and the branch menu.

use super::*;

impl Workspace {
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
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.git_busy = None;
                // The branch shown in the top bar follows a pull, switch or new branch.
                ws.store.refresh_git();
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
                                Some(("Ask agent".into(), Act::FixGitWithClaude(cwd))),
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
                let Ok(paths) = this.update(cx, |ws, _| ws.store.data.projects.iter().map(|p| p.path.clone()).collect::<Vec<_>>())
                else {
                    break;
                };
                let infos = cx
                    .background_executor()
                    .spawn(async move { paths.into_iter().map(|p| { let g = crate::git::detect(&p); (p, g) }).collect::<Vec<_>>() })
                    .await;
                let _ = this.update(cx, |ws, cx| {
                    let mut changed = false;
                    for (path, git) in infos {
                        if let Some(p) = ws.store.data.projects.iter_mut().find(|p| p.path == path && p.git != git) {
                            p.git = git;
                            changed = true;
                        }
                    }
                    if changed {
                        ws.store.mark_dirty();
                        cx.notify();
                    }
                });
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
