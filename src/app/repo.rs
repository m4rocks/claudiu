//! Git from the workspace: git actions off the UI thread, the background fetch and watch, and the branch menu.

use super::*;

impl Workspace {
    /// Git actions (see `git/commit.rs`): one at a time, off the UI thread, reported through a toast.
    pub(super) fn run_git(
        &mut self,
        op: GitOp,
        job: impl FnOnce() -> Result<String, crate::git::commit::GitFail> + Send + 'static,
    ) {
        if self.git_busy.is_some() {
            return;
        }
        self.git_busy = Some(op);
        self.rt.spawn(
            move || { job() },
            move |ws: &mut Workspace, result| {
                ws.git_busy = None;
                // The branch shown in the top bar follows a pull, switch or new branch.
                ws.store.refresh_git();
                ws.store.save_if_dirty();
                ws.refresh_pending();
                // The Worktrees page of Settings lists what a removal just deleted.
                if op == GitOp::Worktree && ws.settings_page == SettingsPage::Worktrees {
                    ws.load_stored_worktrees();
                }
                match result {
                    Ok(summary) => ws.toast(summary, false),
                    Err(fail) => match fail.conflict_in {
                        Some(cwd) => {
                            let what = if op == GitOp::Pull { "Pull" } else { "Commit & Push" };
                            ws.show_toast(
                                format!("{what} hit a conflict. Ask Claude to fix it?"),
                                true,
                                Some(("Ask Claude".into(), Act::FixGitWithClaude(cwd))),
                            );
                        }
                        None => ws.toast(fail.msg, true),
                    },
                }
            },
        );
    }

    /// Re-read, off the UI thread, whether Commit & Push has anything to do in the shown session's folder.
    pub(super) fn refresh_pending(&mut self) {
        let cwd = self.active_cwd();
        let dir = cwd.clone();
        self.rt.spawn(
            move || crate::git::commit::pending(&dir),
            move |ws: &mut Workspace, pending| {
                // The shown session may have changed meanwhile; its own read replaces this one.
                let now = pending.ok().map(|p| (cwd, p));
                if ws.pending != now {
                    ws.pending = now;
                }
            },
        );
    }

    /// Fetch origin for the folder in view once a minute so the branch menu opens instantly and the
    /// remote branches it lists are current. Skipped while another git operation runs.
    pub(super) fn start_fetch_loop(&mut self) {
        if let Some(cwd) = self.git_busy.is_none().then(|| self.active_cwd()) {
            self.rt.background(move || crate::git::commit::background_fetch(&cwd));
        }
        self.rt.after(Duration::from_secs(60), |ws: &mut Workspace| ws.start_fetch_loop());
    }

    /// Re-read each project's git info every few seconds, off the UI thread, so the branch badge follows a
    /// checkout made in a terminal or by Claude, not only Claudiu's own branch actions.
    pub(super) fn start_git_watch(&mut self) {
        self.rt.after(Duration::from_secs(3), |ws: &mut Workspace| {
            let paths: Vec<PathBuf> = ws.store.data.projects.iter().map(|p| p.path.clone()).collect();
            ws.rt.spawn(
                move || paths.into_iter().map(|p| {
                    let g = crate::git::detect(&p);
                    (p, g)
                }).collect::<Vec<_>>(),
                |ws: &mut Workspace, infos| {
                    let mut changed = false;
                    for (path, git) in infos {
                        if let Some(p) = ws.store.data.projects.iter_mut().find(|p| p.path == path && p.git != git) {
                            p.git = git;
                            changed = true;
                        }
                    }
                    if changed {
                        ws.store.mark_dirty();
                    }
                    ws.start_git_watch();
                },
            );
        });
    }

    /// Branch menu: list branches off the UI thread, then open the menu under the trigger.
    pub fn open_branch_menu(&mut self, cwd: PathBuf, at: MenuAnchor) {
        if self.git_busy.is_some() {
            return;
        }
        self.git_busy = Some(GitOp::Branch);
        let dir = cwd.clone();
        self.rt.spawn(
            move || {
                let list = crate::git::commit::branches(&dir)?;
                let deletable = crate::git::commit::deletable_branches(&dir, list.current.as_deref());
                Ok::<_, String>((list, deletable))
            },
            move |ws: &mut Workspace, result| {
                ws.git_busy = None;
                match result {
                    Ok((list, deletable)) => {
                        let mut items = vec![MenuItem::new("Branches", Act::Dismiss)];
                        items[0].disabled = true;
                        // ponytail: the menu doesn't scroll, so only the most recently used branches are listed.
                        items.extend(list.names.into_iter().take(15).map(|name| {
                            let label = format!("{}{name}", crate::panels::widgets::tick(list.current.as_deref() == Some(name.as_str())));
                            let held = list.elsewhere.iter().find(|h| h.branch == name);
                            let hint = held.map(|h| format!("opens {}", h.tree));
                            // Git won't check a branch out twice, so the row moves the session into the working tree that has it.
                            let act = match held {
                                Some(h) => Act::SwitchWorktree { folder: h.folder.clone(), base: Default::default() },
                                None => Act::SwitchBranch(cwd.clone(), name.clone()),
                            };
                            // Remote-only branches have nothing local to delete.
                            let remove = (held.is_none() && deletable.contains(&name)).then(|| Act::DeleteBranch(cwd.clone(), name.clone()));
                            MenuItem { remove, hint, icon: held.map(|_| &crate::panels::icons::FOLDER_GIT_2), ..MenuItem::new(label, act) }
                        }));
                        items.push(MenuItem { separator_before: true, ..MenuItem::new("New branch…", Act::NewBranch(cwd)) });
                        ws.open_menu(at, 300.0, items);
                    }
                    Err(e) => ws.toast(e, true),
                }
            },
        );
    }

    /// Ask before deleting a local branch, after counting the commits only it has.
    pub(super) fn ask_delete_branch(&mut self, cwd: PathBuf, name: String) {
        let (dir, branch) = (cwd.clone(), name.clone());
        self.rt.spawn(
            move || crate::git::commit::commits_only_on(&dir, &branch),
            move |ws: &mut Workspace, lost| match lost {
                Ok(lost) => {
                    let (title, body, confirm, danger) = delete_branch_question(&name, lost);
                    ws.modal = Some(Modal::Confirm { title, body, confirm, danger, act: Act::ConfirmDeleteBranch(cwd, name) });
                }
                Err(e) => ws.toast(e, true),
            },
        );
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
