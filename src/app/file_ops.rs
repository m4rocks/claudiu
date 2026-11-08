//! Creating, renaming and deleting files from editor mode's file tree. The disk work is `files::create_file`,
//! `create_folder`, `rename` and `delete`, which keep to the session's folder. Open tabs follow a rename and close
//! with a delete.

use std::path::Path;

use super::navigation::moved_path;
use super::*;
use crate::files;

/// What a file name the user types is for.
#[derive(Clone, Debug)]
pub enum NameFor {
    /// A new file in this folder.
    FileIn(PathBuf),
    /// A new folder in this folder.
    FolderIn(PathBuf),
    /// A new name for this file or folder.
    Rename(PathBuf),
}

impl Workspace {
    /// The tree's right-click menu at window point `x`, `y`: for a file or folder, or for the session folder (`None`).
    pub fn open_file_menu(&mut self, id: &Id, path: Option<&Path>, folder: bool, x: f32, y: f32) {
        let Some(pane) = self.panes.get(id) else { return };
        let new_items = |dir: &Path| {
            vec![
                MenuItem::new("New file…", Act::AskFileName(id.clone(), NameFor::FileIn(dir.to_path_buf()))),
                MenuItem::new("New folder…", Act::AskFileName(id.clone(), NameFor::FolderIn(dir.to_path_buf()))),
            ]
        };
        let items = match path {
            None => new_items(&pane.root),
            Some(path) => {
                let dir = if folder { path.to_path_buf() } else { path.parent().map_or_else(|| pane.root.clone(), Path::to_path_buf) };
                let mut items = new_items(&dir);
                items.push(MenuItem::new("Rename…", Act::AskFileName(id.clone(), NameFor::Rename(path.to_path_buf()))));
                let reveal = if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Reveal in File Explorer" };
                items.push(MenuItem { separator_before: true, ..MenuItem::new("Copy relative path", Act::CopyText(editor_mode::relative(path, &pane.root))) });
                items.push(MenuItem::new(reveal, Act::Reveal(path.to_path_buf())));
                items.push(MenuItem { danger: true, separator_before: true, ..MenuItem::new("Delete", Act::AskDelete(id.clone(), path.to_path_buf())) });
                items
            }
        };
        self.open_menu(MenuAnchor::Point(x, y), 0.0, items);
    }

    pub(super) fn ask_file_name(&mut self, id: Id, purpose: NameFor) {
        let value = match &purpose {
            NameFor::FileIn(_) | NameFor::FolderIn(_) => String::new(),
            NameFor::Rename(path) => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        self.modal = Some(Modal::FileName { id, purpose, input: modal::field(&value, true) });
    }

    pub(super) fn ask_delete(&mut self, id: Id, path: PathBuf) {
        let Some(pane) = self.panes.get(&id) else { return };
        let name = editor_mode::relative(&path, &pane.root);
        let unsaved = pane.tabs.iter().any(|t| t.dirty && crate::platform::is_same_or_under(&t.path, &path));
        let mut body = if path.is_dir() { "Deletes the folder and everything in it from disk." } else { "Deletes the file from disk." }.to_string();
        body.push_str(" This can't be undone.");
        if unsaved {
            body.push_str(" Unsaved edits in its open tabs are lost.");
        }
        self.modal = Some(Modal::Confirm { title: format!("Delete {name}?"), body, confirm: "Delete".into(), danger: true, act: Act::ConfirmDelete(id, path) });
    }

    /// Carry out a typed file name; `Err` says why it can't be used, and the dialog stays open.
    pub(super) fn apply_file_name(&mut self, id: &Id, purpose: &NameFor, name: &str) -> Result<(), String> {
        let root = self.panes.get(id).map(|p| p.root.clone()).ok_or("The session's files are gone.")?;
        match purpose {
            NameFor::FileIn(dir) => {
                let path = files::create_file(&root, dir, name).map_err(|e| e.to_string())?;
                self.open_folders(id, &[dir]);
                self.refresh_files();
                self.open_file(id, path, None);
            }
            NameFor::FolderIn(dir) => {
                let path = files::create_folder(&root, dir, name).map_err(|e| e.to_string())?;
                self.open_folders(id, &[dir, &path]);
                self.refresh_files();
            }
            NameFor::Rename(from) => {
                let to = files::rename(&root, from, name).map_err(|e| e.to_string())?;
                self.follow_rename(id, from, to);
                self.refresh_files();
            }
        }
        Ok(())
    }

    fn open_folders(&mut self, id: &Id, folders: &[&Path]) {
        if let Some(pane) = self.panes.get_mut(id) {
            pane.expanded.extend(folders.iter().map(|f| f.to_path_buf()));
        }
    }

    /// The open tab of a renamed file shows it under its new name, and its language server hears of it as a new file.
    /// The shown tab, when its language changed and it has no unsaved edits, is opened afresh. For a renamed folder,
    /// every tab inside it follows, and its open folders stay open.
    fn follow_rename(&mut self, id: &Id, from: &Path, to: PathBuf) {
        self.retarget_history(id, from, &to);
        let Some(pane) = self.panes.get_mut(id) else { return };
        if to.is_dir() {
            pane.expanded = pane.expanded.drain().map(|mut p| {
                moved_path(&mut p, from, &to);
                p
            }).collect();
        }
        let moved: Vec<(PathBuf, PathBuf)> = pane
            .tabs
            .iter()
            .filter_map(|t| {
                let mut path = t.path.clone();
                moved_path(&mut path, from, &to);
                (path != t.path).then(|| (t.path.clone(), path))
            })
            .collect();
        // By path, since reopening a tab moves the others.
        for (old, path) in moved {
            let Some(pane) = self.panes.get(id) else { return };
            let Some(ix) = pane.tabs.iter().position(|t| t.path == old) else { continue };
            let tab = &pane.tabs[ix];
            let language = files::language(&path);
            if pane.active == Some(ix) && language != tab.language && !tab.dirty && tab.note.is_none() {
                self.close_tab(id, ix);
                self.open_file(id, path, None);
                continue;
            }
            let Some(tab) = self.panes.get_mut(id).map(|p| &mut p.tabs[ix]) else { return };
            Self::tell_server_closed(tab);
            (tab.path, tab.server, tab.version) = (path.clone(), None, 0);
            if tab.note.is_none() && language != tab.language {
                tab.language = language;
                // Colours for the old language would be wrong; plain until the new ones arrive.
                tab.editor.spans.clear();
                tab.editor.coloured = None;
            }
            self.attach_server(id, &path);
        }
    }

    /// Delete off the UI thread (a folder can hold many files); tabs inside it close once it is gone.
    pub(super) fn delete_path(&mut self, id: Id, path: PathBuf) {
        let Some(root) = self.panes.get(&id).map(|p| p.root.clone()) else { return };
        let target = path.clone();
        self.rt.spawn(
            move || files::delete(&root, &target),
            move |ws: &mut Workspace, done: std::io::Result<()>| {
                match done {
                    Ok(()) => {
                        ws.forget_history(&id, &path);
                        while let Some(ix) = ws.panes.get(&id).and_then(|p| p.tabs.iter().position(|t| crate::platform::is_same_or_under(&t.path, &path))) {
                            ws.close_tab(&id, ix);
                        }
                    }
                    Err(e) => ws.toast(format!("Could not delete it: {e}"), true),
                }
                ws.refresh_files();
            },
        );
    }
}
