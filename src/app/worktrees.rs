//! Git worktrees in the app: making one from the Branch & Worktree dialog, switching the shown session between them
//! (see `switch.rs`), and removing them. m4code makes the worktrees with git (see `git/worktree.rs`).

use std::path::Path;

use super::*;
use crate::git::GitInfo;
use crate::git::worktree;
use crate::platform::path_key;

/// What the Worktrees page of Settings shows.
#[derive(Default)]
pub struct WorktreesPage {
    /// The worktree folders in m4code's data folder; none until the first look at the disk ends.
    pub stored: Option<Vec<worktree::Stored>>,
    /// The removal the page is asking the user to confirm.
    pub asking: Option<Asked>,
}

/// A removal question shown inside a row of the Worktrees page, so the dialog's unsaved fields stay where they are.
pub struct Asked {
    pub folder: PathBuf,
    pub body: String,
    pub confirm: String,
    pub danger: bool,
    pub act: Act,
}

/// Whether the unified Branch & Worktree dialog is acting on the shown session or the Welcome screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitPickerTarget {
    Session,
    Home,
}

/// One worktree row inside the unified Branch & Worktree dialog.
pub struct GitPickerWorktree {
    pub label: String,
    pub folder: PathBuf,
    pub branch: Option<String>,
    pub current: bool,
    pub running: bool,
    pub remove: Option<Act>,
    pub missing: bool,
}

/// One branch row inside the unified Branch & Worktree dialog.
pub struct GitPickerBranch {
    pub name: String,
    pub current: bool,
    pub held_tree: Option<(String, PathBuf)>,
    pub held_missing: bool,
    pub remove: Option<Act>,
    pub delete_reason: Option<String>,
}

/// Unified Branch & Worktree dialog state, shared by the Welcome screen and the window's top bar.
pub struct GitPicker {
    pub cwd: PathBuf,
    pub main_checkout: PathBuf,
    pub target: GitPickerTarget,
    pub search: Entity<InputState>,
    pub can_switch_worktree: bool,
    pub new_worktree_reason: Option<String>,
    pub worktrees: Vec<GitPickerWorktree>,
    /// `None` while branches are loading off the UI thread.
    pub branches: Option<Result<Vec<GitPickerBranch>, String>>,
}

/// What making a worktree called `typed` does (a random name when it is blank): on the Welcome screen it is made and
/// chosen for the next session; for a running session, the session moves into it. Err says why the name can't be used.
pub(super) fn new_worktree_act(main_checkout: &Path, typed: &str, base: worktree::Base, for_home: bool) -> Result<Act, String> {
    let typed = typed.trim();
    let name = if typed.is_empty() {
        worktree::generate_name(uuid::Uuid::new_v4().as_u128(), worktree::name_taken(main_checkout))
    } else {
        worktree::check_name(typed).and_then(|()| worktree::check_unused(main_checkout, typed))?;
        typed.to_string()
    };
    let folder = worktree::folder(main_checkout, &name);
    Ok(if for_home { Act::CreateHomeWorktree { main_checkout: main_checkout.to_path_buf(), folder, base } } else { Act::SwitchWorktree { folder, base } })
}

impl Workspace {
    /// Show a page of the Settings dialog. The Worktrees page looks at the disk every time it opens.
    pub(crate) fn set_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.settings_page = page;
        self.worktrees_page.asking = None;
        if page == SettingsPage::Projects {
            self.load_stored_worktrees(cx);
        }
        if page == SettingsPage::Skills {
            self.load_skills_catalog(cx);
            self.load_repo_skills(cx);
        }
        self.load_models(cx);
        cx.notify();
    }

    pub(super) fn load_stored_worktrees(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { worktree::stored() }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.worktrees_page.stored = Some(found);
                cx.notify();
            });
        })
        .detach();
    }

    /// Create a new worktree from the Welcome screen and select it for the upcoming session. The chip shows it at once,
    /// while git works; a session started meanwhile waits for it.
    pub(super) fn create_home_worktree(&mut self, main_checkout: PathBuf, folder: PathBuf, base: Base, window: &mut Window, cx: &mut Context<Self>) {
        if self.git_busy.is_some() {
            return;
        }
        self.git_busy = Some(GitOp::Worktree);
        self.home.creating = Some(folder.clone());
        self.home.launch_when_created = false;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let dir = folder.clone();
            let result = cx.background_executor().spawn(async move { worktree::add(&main_checkout, &dir, base) }).await;
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.git_busy = None;
                ws.home.creating = None;
                let launch = std::mem::take(&mut ws.home.launch_when_created);
                ws.store.refresh_git();
                ws.store.save_if_dirty();
                match result {
                    Ok(()) => {
                        ws.home.worktree = Some(folder);
                        if launch {
                            ws.act(Act::HomeLaunch, window, cx);
                        }
                    }
                    // What was typed stays in the message box, so nothing is lost when git refuses.
                    Err(why) => ws.toast(format!("Couldn't create worktree: {}", why.trim()), true, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the unified Branch & Worktree dropdown for `project` at `cwd`, anchored to `at`. Worktrees populate
    /// immediately; branches load off the UI thread and stream in when ready.
    pub fn open_git_picker(&mut self, project: Id, cwd: PathBuf, target: GitPickerTarget, at: crate::panels::widgets::Dropdown, window: &mut Window, cx: &mut Context<Self>) {
        let Some(git) = self.store.project(&project).and_then(|p| p.git.clone()) else { return };
        self.menu = None;
        let here = git.folder_of(&cwd).map(path_key);
        let can_switch_worktree = match target {
            GitPickerTarget::Home => true,
            GitPickerTarget::Session => self.active_id().is_some_and(|id| self.is_running(id, cx)),
        };
        let worktrees = worktree::folders(&git)
            .into_iter()
            .map(|folder| {
                let label = worktree::label(&git, folder).unwrap_or_default();
                let running = self.live.iter().any(|l| self.store.session(&l.id).is_some_and(|s| same_folder(&git, &s.cwd, folder)));
                let current = here.as_deref() == Some(path_key(folder).as_str());
                let remove = worktree::removable(git.main_checkout(), folder).then(|| Act::RemoveWorktree {
                    main_checkout: git.main_checkout().to_path_buf(),
                    folder: folder.to_path_buf(),
                });
                GitPickerWorktree {
                    label,
                    folder: folder.to_path_buf(),
                    branch: git.branch_in(folder).map(str::to_string),
                    current,
                    running,
                    remove,
                    missing: false,
                }
            })
            .collect();
        let new_worktree_reason = worktree::source(Some(&git)).err().map(str::to_string);
        let search = self.picker_search("Filter branches & worktrees…", window, cx);
        let main_checkout = git.main_checkout().to_path_buf();
        self.picker = Some(AnchoredPicker {
            at,
            kind: WorkspacePicker::Git(GitPicker {
                cwd: cwd.clone(),
                main_checkout,
                target,
                search,
                can_switch_worktree,
                new_worktree_reason,
                worktrees,
                branches: None,
            }),
        });
        cx.notify();

        let dir = cwd.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let list = crate::git::commit::branches(&dir)?;
                    let items = list
                        .names
                        .iter()
                        .cloned()
                        .map(|name| {
                            let current = list.current.as_deref() == Some(name.as_str());
                            let held = list.elsewhere.iter().find(|h| h.branch == name);
                            let held_tree = held.map(|h| (h.tree.clone(), h.folder.clone()));
                            let held_missing = held.is_some_and(|h| !h.folder.is_dir());
                            let delete_reason = list.deletion_reason(&name).map(|reason| if held_missing { format!("{reason}; folder is missing") } else { reason });
                            let remove = delete_reason.is_none().then(|| Act::DeleteBranch(dir.clone(), name.clone()));
                            GitPickerBranch { name, current, held_tree, held_missing, remove, delete_reason }
                        })
                        .collect::<Vec<_>>();
                    let folders = list.worktree_paths.into_iter().map(|path| {
                        let missing = !path.is_dir();
                        (path, missing)
                    }).collect::<Vec<_>>();
                    Ok::<_, String>((items, folders, crate::git::detect(&dir)))
                })
                .await;
            let _ = this.update(cx, |ws, cx| {
                let running: Vec<_> = ws.live.iter().filter_map(|l| ws.store.session(&l.id).map(|s| s.cwd.clone())).collect();
                let result = result.map(|(branches, folders, git)| {
                    if let Some(project) = ws.store.data.projects.iter_mut().find(|p| p.id == project) {
                        project.git = git;
                    }
                    (branches, folders)
                });
                if let Some(AnchoredPicker { kind: WorkspacePicker::Git(picker), .. }) = ws.picker.as_mut()
                    && picker.cwd == cwd
                {
                    picker.branches = Some(result.map(|(branches, folders)| {
                        picker.worktrees.retain(|wt| folders.iter().any(|(folder, _)| path_key(folder) == path_key(&wt.folder)));
                        for (folder, missing) in folders {
                            if let Some(wt) = picker.worktrees.iter_mut().find(|wt| path_key(&wt.folder) == path_key(&folder)) {
                                wt.missing = missing;
                                continue;
                            }
                            let branch = branches.iter().find(|br| br.held_tree.as_ref().is_some_and(|(_, held)| path_key(held) == path_key(&folder))).map(|br| br.name.clone());
                            let label = if path_key(&folder) == path_key(&picker.main_checkout) {
                                "main checkout".to_string()
                            } else {
                                folder.file_name().unwrap_or_default().to_string_lossy().into_owned()
                            };
                            let remove = (!missing && worktree::removable(&picker.main_checkout, &folder)).then(|| Act::RemoveWorktree { main_checkout: picker.main_checkout.clone(), folder: folder.clone() });
                            picker.worktrees.push(GitPickerWorktree {
                                label,
                                current: path_key(&cwd) == path_key(&folder),
                                running: running.iter().any(|cwd| crate::platform::is_same_or_under(cwd, &folder)),
                                folder,
                                branch,
                                remove,
                                missing,
                            });
                        }
                        branches
                    }));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The most recently active m4code session that runs in `folder`.
    fn running_in(&self, folder: &Path) -> Option<&SessionRecord> {
        self.live.iter().filter_map(|l| self.store.session(&l.id)).filter(|s| crate::platform::is_same_or_under(&s.cwd, folder)).max_by_key(|s| s.last_active)
    }

    /// Whether a running session is in `folder`, which refuses removing it with a toast.
    fn refuse_while_running(&mut self, folder: &Path, cx: &mut Context<Self>) -> bool {
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(inside) = self.running_in(folder) else { return false };
        let title = inside.display_title().to_string();
        self.toast(format!("“{title}” is in {name}. Switch it to main first."), true, cx);
        true
    }

    /// Ask whether to remove `folder`, after reading what it holds. A running session in it refuses: it has to leave first.
    /// The question appears in the Worktrees page while Settings is open, and in a dialog otherwise.
    pub(super) fn ask_remove_worktree(&mut self, main_checkout: PathBuf, folder: PathBuf, cx: &mut Context<Self>) {
        if self.refuse_while_running(&folder, cx) {
            return;
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let dir = folder.clone();
            let found = cx.background_executor().spawn(async move { worktree::leftovers(&dir) }).await;
            let _ = this.update(cx, |ws, cx| match found {
                Ok(left) => {
                    let (title, body, confirm, danger) = removal_question(&name, left);
                    let act = Act::ConfirmRemoveWorktree { main_checkout, folder: folder.clone(), discard: left.files > 0 };
                    ws.show_removal_question(folder, title, body, confirm, danger, act);
                    cx.notify();
                }
                Err(e) => ws.toast(e, true, cx),
            });
        })
        .detach();
    }

    /// Ask whether to delete a folder of m4code's that git no longer lists, so only the folder can go.
    pub(super) fn ask_delete_unlisted(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        if self.refuse_while_running(&folder, cx) {
            return;
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let body = "Git no longer lists this worktree, so m4code cannot say what is in it. The folder and everything in it will be deleted.".to_string();
        self.show_removal_question(folder.clone(), format!("Delete the folder {name}?"), body, "Delete".into(), true, Act::ConfirmDeleteUnlisted(folder));
        cx.notify();
    }

    fn show_removal_question(&mut self, folder: PathBuf, title: String, body: String, confirm: String, danger: bool, act: Act) {
        if matches!(self.modal, Some(Modal::Settings { .. })) {
            self.worktrees_page.asking = Some(Asked { folder, body, confirm, danger, act });
        } else {
            self.modal = Some(Modal::Confirm { title, body, confirm, danger, act });
        }
    }

    /// A session that ran in `cwd` has ended: offer to remove its worktree when m4code made it and no
    /// other session is still in it.
    pub(super) fn offer_worktree_removal(&mut self, cwd: &Path, project: Option<&str>, cx: &mut Context<Self>) {
        let Some(git) = project.and_then(|id| self.store.project(id)).and_then(|p| p.git.clone()) else { return };
        let Some(folder) = git.folder_of(cwd).filter(|f| worktree::removable(git.main_checkout(), f) && f.is_dir()) else { return };
        if self.running_in(folder).is_none() {
            self.ask_remove_worktree(git.main_checkout().to_path_buf(), folder.to_path_buf(), cx);
        }
    }
}

/// The dialog's title, text, button and whether the button is red. Only changed and new files are thrown away: commits no
/// remote has stay on the branch, which is kept when it is not merged.
fn removal_question(name: &str, left: worktree::Leftovers) -> (String, String, String, bool) {
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut lines = vec![];
    if left.files > 0 {
        lines.push(format!("{} will be deleted.", plural(left.files, "uncommitted file", "uncommitted files")));
    }
    if left.unpushed > 0 {
        let (commits, verb) = if left.unpushed == 1 { ("1 commit".to_string(), "stays") } else { (format!("{} commits", left.unpushed), "stay") };
        lines.push(format!("{commits} that no remote branch has {verb} on the worktree's branch, which is kept."));
    }
    if !left.any() {
        lines.push("It has no uncommitted files, and every commit is on a remote branch.".into());
    }
    let discard = left.files > 0;
    (format!("Remove worktree {name}?"), lines.join(" "), if discard { "Remove and discard" } else { "Remove" }.into(), discard)
}

/// Whether `cwd` is in the working tree `folder` of the repository, not in another one nested inside it.
fn same_folder(git: &GitInfo, cwd: &Path, folder: &Path) -> bool {
    git.folder_of(cwd).is_some_and(|f| path_key(f) == path_key(folder))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_removal_dialog_says_what_is_deleted_and_only_turns_red_for_files() {
        let ask = |files, unpushed| removal_question("fix", worktree::Leftovers { files, unpushed });
        let (title, body, button, red) = ask(0, 0);
        assert_eq!((title.as_str(), button.as_str(), red), ("Remove worktree fix?", "Remove", false));
        assert!(body.contains("no uncommitted files"), "{body}");

        let (_, body, button, red) = ask(2, 1);
        assert!(body.contains("2 uncommitted files will be deleted") && body.contains("1 commit that no remote branch has stays")&& !body.contains("ends the session"), "{body}");
        assert_eq!((button.as_str(), red), ("Remove and discard", true));

        let (_, body, _, red) = ask(0, 3);
        assert!(body.contains("3 commits that no remote branch has stay") && !red, "{body}");
    }
}
