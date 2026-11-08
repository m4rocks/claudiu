//! Git worktrees in the app: the dialog for a new one, the top-bar menu that switches the shown session between them
//! (see `switch.rs`), and removing them. Claudiu makes the worktrees with git (see `git/worktree.rs`).

use std::path::Path;

use super::*;
use crate::git::GitInfo;
use crate::git::worktree;
use crate::panels::widgets::tick;
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

impl Workspace {
    /// Show a page of the Settings dialog. The Worktrees page looks at the disk every time it opens.
    pub(crate) fn set_settings_page(&mut self, page: SettingsPage) {
        self.settings_page = page;
        self.worktrees_page.asking = None;
        if page == SettingsPage::Worktrees {
            self.load_stored_worktrees();
        }
    }

    pub(super) fn load_stored_worktrees(&mut self) {
        self.rt.spawn(
            move || { worktree::stored() },
            move |ws: &mut Workspace, found| {
                ws.worktrees_page.stored = Some(found);
            },
        );
    }

    pub(super) fn ask_worktree(&mut self, main_checkout: PathBuf) {
        let input = super::modal::field("", true);
        self.modal = Some(Modal::NewWorktree { main_checkout, input, base: Base::default() });
    }

    /// The top-bar menu: the repository's working trees, each switching the shown session into it, and a new worktree.
    pub fn open_worktree_menu(&mut self, project: Id, cwd: PathBuf, at: MenuAnchor) {
        let Some(git) = self.store.project(&project).and_then(|p| p.git.clone()) else { return };
        let here = git.folder_of(&cwd).map(path_key);
        // A switch restarts the process, so it needs one that is running.
        let can_switch = self.active_id().is_some_and(|id| self.is_running(id));
        let mut title = MenuItem::new("Worktrees", Act::Dismiss);
        title.disabled = true;
        title.hint = (!can_switch).then(|| "Resume the session to switch".to_string());
        let mut items = vec![title];
        items.extend(worktree::folders(&git).into_iter().map(|folder| {
            let name = worktree::label(&git, folder).unwrap_or_default();
            let running = self.live.iter().any(|l| self.store.session(&l.id).is_some_and(|s| same_folder(&git, &s.cwd, folder)));
            let own = here.as_deref() == Some(path_key(folder).as_str());
            let remove = worktree::removable(git.main_checkout(), folder).then(|| Act::RemoveWorktree { main_checkout: git.main_checkout().to_path_buf(), folder: folder.to_path_buf() });
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
        self.open_menu(at, 340.0, items);
    }

    /// The most recently active Claudiu session that runs in `folder`.
    fn running_in(&self, folder: &Path) -> Option<&SessionRecord> {
        self.live.iter().filter_map(|l| self.store.session(&l.id)).filter(|s| crate::platform::is_same_or_under(&s.cwd, folder)).max_by_key(|s| s.last_active)
    }

    /// Whether a running session is in `folder`, which refuses removing it with a toast.
    fn refuse_while_running(&mut self, folder: &Path) -> bool {
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(inside) = self.running_in(folder) else { return false };
        let title = inside.display_title().to_string();
        self.toast(format!("“{title}” is in {name}. Switch it to main first."), true);
        true
    }

    /// Ask whether to remove `folder`, after reading what it holds. A running session in it refuses: it has to leave first.
    /// The question appears in the Worktrees page while Settings is open, and in a dialog otherwise.
    pub(super) fn ask_remove_worktree(&mut self, main_checkout: PathBuf, folder: PathBuf) {
        if self.refuse_while_running(&folder) {
            return;
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let dir = folder.clone();
            
        self.rt.spawn(
            move || { worktree::leftovers(&dir) },
            move |ws: &mut Workspace, found| match found {
                Ok(left) => {
                    let (title, body, confirm, danger) = removal_question(&name, left);
                    let act = Act::ConfirmRemoveWorktree { main_checkout, folder: folder.clone(), discard: left.files > 0 };
                    ws.show_removal_question(folder, title, body, confirm, danger, act);
                }
                Err(e) => ws.toast(e, true),
            },
        );
    }

    /// Ask whether to delete a folder of Claudiu's that git no longer lists, so only the folder can go.
    pub(super) fn ask_delete_unlisted(&mut self, folder: PathBuf) {
        if self.refuse_while_running(&folder) {
            return;
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let body = "Git no longer lists this worktree, so Claudiu cannot say what is in it. The folder and everything in it will be deleted.".to_string();
        self.show_removal_question(folder.clone(), format!("Delete the folder {name}?"), body, "Delete".into(), true, Act::ConfirmDeleteUnlisted(folder));
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
    pub(super) fn offer_worktree_removal(&mut self, cwd: &Path, project: Option<&str>) {
        let Some(git) = project.and_then(|id| self.store.project(id)).and_then(|p| p.git.clone()) else { return };
        let Some(folder) = git.folder_of(cwd).filter(|f| worktree::removable(git.main_checkout(), f) && f.is_dir()) else { return };
        if self.running_in(folder).is_none() {
            self.ask_remove_worktree(git.main_checkout().to_path_buf(), folder.to_path_buf());
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
