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
                match result {
                    Ok(summary) => ws.toast(summary, false, cx),
                    Err(fail) => match fail.conflict_in {
                        Some(cwd) => {
                            let what = if op == GitOp::Pull { "Pull" } else { "Commit & Push" };
                            ws.show_toast(
                                format!("{what} hit a conflict. Ask Claude to fix it?"),
                                true,
                                Some(("Ask Claude".into(), Act::FixGitWithClaude(cwd))),
                                cx,
                            );
                        }
                        None => ws.toast(fail.msg, true, cx),
                    },
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
    /// checkout made in a terminal or by Claude, not only Claudiu's own branch actions.
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

    /// Branch menu: list branches off the UI thread, then open the menu under the trigger.
    pub fn open_branch_menu(&mut self, cwd: PathBuf, at: crate::panels::widgets::Dropdown, cx: &mut Context<Self>) {
        if self.git_busy.is_some() {
            return;
        }
        self.git_busy = Some(GitOp::Branch);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let dir = cwd.clone();
            let result = cx.background_executor().spawn(async move { crate::git::commit::branches(&dir) }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.git_busy = None;
                match result {
                    Ok((names, current)) => {
                        // ponytail: the menu doesn't scroll, so only the most recently used branches are listed.
                        let mut items: Vec<MenuItem> = names
                            .into_iter()
                            .take(15)
                            .map(|name| MenuItem::new(format!("{}{name}", crate::panels::widgets::tick(current.as_deref() == Some(name.as_str()))), Act::SwitchBranch(cwd.clone(), name)))
                            .collect();
                        let first = items.is_empty();
                        items.push(MenuItem { label: "New branch…".into(), hint: None, act: Act::NewBranch(cwd), danger: false, disabled: false, separator_before: !first });
                        ws.open_dropdown(at, items, cx);
                    }
                    Err(e) => ws.toast(e, true, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
