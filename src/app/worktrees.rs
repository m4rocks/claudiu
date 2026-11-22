//! Git worktrees in the app: making one from the Branch & Worktree dialog, switching the shown session between them
//! (see `switch.rs`), and removing them. Claudiu makes the worktrees with git (see `git/worktree.rs`).

use std::path::Path;

use super::*;
use crate::git::GitInfo;
use crate::git::worktree;
use crate::platform::path_key;

/// What the Worktrees page of Settings shows.
#[derive(Default)]
pub struct WorktreesPage {
    /// The worktree folders in Claudiu's data folder; none until the first look at the disk ends.
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
}

/// One branch row inside the unified Branch & Worktree dialog.
pub struct GitPickerBranch {
    pub name: String,
    pub current: bool,
    pub held_tree: Option<(String, PathBuf)>,
    pub remove: Option<Act>,
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

    /// Create a new worktree from the Welcome screen and select it for the upcoming session.
    pub(super) fn create_home_worktree(&mut self, main_checkout: PathBuf, folder: PathBuf, base: Base, cx: &mut Context<Self>) {
        if self.git_busy.is_some() {
            return;
        }
        self.git_busy = Some(GitOp::Worktree);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let dir = folder.clone();
            let result = cx.background_executor().spawn(async move { worktree::add(&main_checkout, &dir, base) }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.git_busy = None;
                ws.store.refresh_git();
                ws.store.save_if_dirty();
                match result {
                    Ok(()) => {
                        let name = folder.file_name().map_or_else(|| "worktree".into(), |n| n.to_string_lossy().into_owned());
                        ws.home.worktree = Some(folder);
                        ws.toast(format!("Created worktree {name}"), false, cx);
                    }
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
                    let deletable = crate::git::commit::deletable_branches(&dir, list.current.as_deref());
                    let items = list
                        .names
                        .into_iter()
                        .map(|name| {
                            let current = list.current.as_deref() == Some(name.as_str());
                            let held = list.elsewhere.iter().find(|h| h.branch == name);
                            let held_tree = held.map(|h| (h.tree.clone(), h.folder.clone()));
                            let remove = (held.is_none() && deletable.contains(&name)).then(|| Act::DeleteBranch(dir.clone(), name.clone()));
                            GitPickerBranch { name, current, held_tree, remove }
                        })
                        .collect::<Vec<_>>();
                    Ok::<_, String>(items)
                })
                .await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(AnchoredPicker { kind: WorkspacePicker::Git(picker), .. }) = ws.picker.as_mut()
                    && picker.cwd == cwd
                {
                    picker.branches = Some(result);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The most recently active Claudiu session that runs in `folder`.
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

    /// Ask whether to delete a folder of Claudiu's that git no longer lists, so only the folder can go.
    pub(super) fn ask_delete_unlisted(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        if self.refuse_while_running(&folder, cx) {
            return;
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let body = "Git no longer lists this worktree, so Claudiu cannot say what is in it. The folder and everything in it will be deleted.".to_string();
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

    /// A session that ran in `cwd` has ended: offer to remove its worktree when Claudiu made it and no
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
