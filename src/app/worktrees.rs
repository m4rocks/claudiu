//! Git worktrees in the app: the dialog for a new one, the top-bar menu that switches the shown session between them
//! (see `switch.rs`), removing them, and the Worktrees page of Settings. Claudiu makes the worktrees with git (see
//! `git/worktree.rs`).

use std::path::Path;

use super::*;
use crate::git::GitInfo;
use crate::git::worktree;
use crate::panels::widgets::tick;
use crate::platform::path_key;

/// A worktree folder in Claudiu's data folder, as a row of the Worktrees page in Settings.
#[derive(Clone, Debug)]
pub struct WorktreeRow {
    /// The repository's folder under the worktrees root; rows of one repository follow each other.
    pub repo: String,
    /// What the page calls the repository: its main checkout when a project of Claudiu's has it, else its name.
    pub label: String,
    pub folder: PathBuf,
    /// Set when git lists the folder as a worktree of a project Claudiu knows; the folder can then be removed with git.
    pub main_checkout: Option<PathBuf>,
}

impl Workspace {
    pub(super) fn ask_worktree(&mut self, main_checkout: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.modal_input("", "Name, or leave blank for a random one", window, cx);
        self.modal = Some(Modal::NewWorktree { main_checkout, input, base: Base::default() });
    }

    /// The top-bar menu: the repository's working trees, each switching the shown session into it, and a new worktree.
    pub fn open_worktree_menu(&mut self, project: Id, cwd: PathBuf, at: crate::panels::widgets::Dropdown, cx: &mut Context<Self>) {
        let Some(git) = self.store.project(&project).and_then(|p| p.git.clone()) else { return };
        let here = git.folder_of(&cwd).map(path_key);
        let root = worktree::root();
        // A switch restarts the process, so it needs one that is running.
        let can_switch = self.active_id().is_some_and(|id| self.is_running(id, cx));
        let mut title = MenuItem::new("Worktrees", Act::Dismiss);
        title.disabled = true;
        title.hint = (!can_switch).then(|| "Resume the session to switch".to_string());
        let mut items = vec![title];
        items.extend(worktree::folders(&git).into_iter().map(|folder| {
            let name = worktree::label(&git, folder).unwrap_or_default();
            let running = self.live.iter().any(|l| self.store.session(&l.id).is_some_and(|s| same_folder(&git, &s.cwd, folder)));
            let own = here.as_deref() == Some(path_key(folder).as_str());
            let remove = worktree::removable(&root, git.main_checkout(), folder).then(|| Act::RemoveWorktree { main_checkout: git.main_checkout().to_path_buf(), folder: folder.to_path_buf() });
            MenuItem {
                label: format!("{}{name}{}", tick(own), if running { "  ●" } else { "" }),
                hint: git.branch_in(folder).map(str::to_string),
                act: if own { Act::Dismiss } else { Act::SwitchWorktree { folder: folder.to_path_buf(), base: Base::default() } },
                disabled: !can_switch,
                remove,
                ..MenuItem::new("", Act::Dismiss)
            }
        }));
        let source = worktree::source(Some(&git));
        items.push(MenuItem {
            hint: source.err().map(str::to_string),
            disabled: source.is_err() || !can_switch,
            separator_before: true,
            ..MenuItem::new("New worktree…", Act::NewWorktree(git.main_checkout().to_path_buf()))
        });
        // Each row holds a name, its branch and a hover button; the default 232 px leaves the name no room.
        self.open_dropdown(crate::panels::widgets::Dropdown { min_w: at.min_w.max(gpui::px(340.0)), ..at }, items, cx);
    }

    /// The most recently active Claudiu session that runs in `folder`.
    fn running_in(&self, folder: &Path) -> Option<&SessionRecord> {
        self.live.iter().filter_map(|l| self.store.session(&l.id)).filter(|s| crate::platform::is_same_or_under(&s.cwd, folder)).max_by_key(|s| s.last_active)
    }

    /// Read the worktree folders in the data folder for the Settings page, off the UI thread.
    pub(super) fn refresh_worktree_rows(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let made = cx.background_executor().spawn(async move { worktree::made(&worktree::root()) }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.worktree_rows = ws.rows_for(made);
                cx.notify();
            });
        })
        .detach();
    }

    fn rows_for(&self, made: Vec<worktree::Made>) -> Vec<WorktreeRow> {
        let root = worktree::root();
        let gits: Vec<&GitInfo> = self.store.data.projects.iter().filter_map(|p| p.git.as_ref()).collect();
        made.into_iter()
            .map(|m| {
                let owner = gits.iter().find(|g| worktree::belongs_to(&root, g.main_checkout(), &m.folder));
                let listed = owner.filter(|g| g.worktrees.iter().chain([&g.root]).any(|w| path_key(w) == path_key(&m.folder)));
                let label = owner.map_or_else(|| worktree::repo_name(&m.repo).to_string(), |g| g.main_checkout().display().to_string());
                WorktreeRow { repo: m.repo, label, folder: m.folder, main_checkout: listed.map(|g| g.main_checkout().to_path_buf()) }
            })
            .collect()
    }

    /// The Settings page's Remove on a folder git doesn't list: nothing but the folder is deleted, so it asks first.
    pub(super) fn ask_delete_worktree_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(inside) = self.running_in(&folder) {
            let title = inside.display_title().to_string();
            self.toast(format!("“{title}” is in {name}. Switch it to main first."), true, cx);
            return;
        }
        self.modal = Some(Modal::Confirm {
            title: format!("Delete folder {name}?"),
            body: "Git no longer lists this worktree, or its repository is gone. The folder and everything in it is deleted.".into(),
            confirm: "Delete".into(),
            danger: true,
            act: Act::ConfirmDeleteWorktreeFolder(folder),
        });
    }

    /// Ask whether to remove `folder`, after reading what it holds. A running session in it refuses: it has to leave first.
    pub(super) fn ask_remove_worktree(&mut self, main_checkout: PathBuf, folder: PathBuf, cx: &mut Context<Self>) {
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(inside) = self.running_in(&folder) {
            let title = inside.display_title().to_string();
            self.toast(format!("“{title}” is in {name}. Switch it to main first."), true, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let dir = folder.clone();
            let found = cx.background_executor().spawn(async move { worktree::leftovers(&dir) }).await;
            let _ = this.update(cx, |ws, cx| match found {
                Ok(left) => {
                    let (title, body, confirm, danger) = removal_question(&name, left);
                    ws.modal = Some(Modal::Confirm { title, body, confirm, danger, act: Act::ConfirmRemoveWorktree { main_checkout, folder, discard: left.files > 0 } });
                    cx.notify();
                }
                Err(e) => ws.toast(e, true, cx),
            });
        })
        .detach();
    }

    /// A session that ran in `cwd` has ended: offer to remove its worktree when Claudiu made it and no other session is
    /// still in it.
    pub(super) fn offer_worktree_removal(&mut self, cwd: &Path, project: Option<&str>, cx: &mut Context<Self>) {
        let Some(git) = project.and_then(|id| self.store.project(id)).and_then(|p| p.git.clone()) else { return };
        let Some(folder) = git.folder_of(cwd).filter(|f| worktree::removable(&worktree::root(), git.main_checkout(), f) && f.is_dir()) else { return };
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
