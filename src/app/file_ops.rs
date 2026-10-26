//! Creating, renaming and deleting files from editor mode's file tree (the disk work is `files::create_file`,
//! `create_folder`, `rename` and `delete`, which keep to the session's folder). Open tabs follow a rename and close with a delete.

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
    /// The tree's right-click menu: for a file or folder, or for the session folder (`None`).
    pub fn file_menu(&self, id: &Id, path: Option<&std::path::Path>, folder: bool) -> Vec<MenuItem> {
        let Some(pane) = self.panes.get(id) else { return vec![] };
        let Some(path) = path else {
            return Self::new_items(id, &pane.root);
        };
        let dir = if folder { path.to_path_buf() } else { path.parent().map_or_else(|| pane.root.clone(), |p| p.to_path_buf()) };
        let mut items = Self::new_items(id, &dir);
        items.push(MenuItem::new("Rename…", Act::AskFileName(id.clone(), NameFor::Rename(path.to_path_buf()))));
        let reveal = if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Reveal in File Explorer" };
        items.push(MenuItem { separator_before: true, ..MenuItem::new("Copy relative path", Act::CopyText(super::picker::relative(path, &pane.root))) });
        items.push(MenuItem::new(reveal, Act::Reveal(path.to_path_buf())));
        items.push(MenuItem { danger: true, separator_before: true, ..MenuItem::new("Delete", Act::AskDelete(id.clone(), path.to_path_buf())) });
        items
    }

    fn new_items(id: &Id, dir: &std::path::Path) -> Vec<MenuItem> {
        vec![
            MenuItem::new("New file…", Act::AskFileName(id.clone(), NameFor::FileIn(dir.to_path_buf()))),
            MenuItem::new("New folder…", Act::AskFileName(id.clone(), NameFor::FolderIn(dir.to_path_buf()))),
        ]
    }

    pub(super) fn ask_file_name(&mut self, id: Id, purpose: NameFor, window: &mut Window, cx: &mut Context<Self>) {
        let (value, placeholder) = match &purpose {
            NameFor::FileIn(_) => (String::new(), "name.ext"),
            NameFor::FolderIn(_) => (String::new(), "name"),
            NameFor::Rename(path) => (path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), "name"),
        };
        let input = self.modal_input(&value, placeholder, window, cx);
        self.modal = Some(Modal::FileName { id, purpose, input });
    }

    pub(super) fn ask_delete(&mut self, id: Id, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get(&id) else { return };
        let name = super::picker::relative(&path, &pane.root);
        let folder = path.is_dir();
        let unsaved = pane.tabs.iter().any(|t| t.dirty && crate::platform::is_same_or_under(&t.path, &path));
        let mut body = if folder { "Deletes the folder and everything in it from disk." } else { "Deletes the file from disk." }.to_string();
        body.push_str(" This can't be undone.");
        if unsaved {
            body.push_str(" Unsaved edits in its open tabs are lost.");
        }
        self.modal = Some(Modal::Confirm { title: format!("Delete {name}?"), body, confirm: "Delete".into(), danger: true, act: Act::ConfirmDelete(id, path) });
        window.focus(&self.modal_focus, cx);
    }

    /// Carry out a typed file name; `Err` says why it can't be used, and the dialog stays open.
    pub(super) fn apply_file_name(&mut self, id: &Id, purpose: &NameFor, name: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let root = self.panes.get(id).map(|p| p.root.clone()).ok_or("The session's files are gone.")?;
        match purpose {
            NameFor::FileIn(dir) => {
                let path = files::create_file(&root, dir, name).map_err(|e| e.to_string())?;
                if let Some(pane) = self.panes.get_mut(id) {
                    pane.expanded.insert(dir.to_string_lossy().into_owned());
                }
                self.refresh_files(cx);
                self.open_file(id, path, None, window, cx);
            }
            NameFor::FolderIn(dir) => {
                let path = files::create_folder(&root, dir, name).map_err(|e| e.to_string())?;
                if let Some(pane) = self.panes.get_mut(id) {
                    pane.expanded.insert(dir.to_string_lossy().into_owned());
                    pane.expanded.insert(path.to_string_lossy().into_owned());
                }
                self.refresh_files(cx);
            }
            NameFor::Rename(from) => {
                let to = files::rename(&root, from, name).map_err(|e| e.to_string())?;
                self.follow_rename(id, from, to, window, cx);
                self.refresh_files(cx);
            }
        }
        Ok(())
    }

    /// The open tab of a renamed file shows it under its new name. The shown tab, when its language changed and it has
    /// no unsaved edits, is opened afresh, since a buffer keeps the highlighting it was made with. For a renamed folder,
    /// every tab inside it follows, and its open folders stay open.
    fn follow_rename(&mut self, id: &Id, from: &std::path::Path, to: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if to.is_dir() {
            self.follow_folder_rename(id, from, &to, cx);
            return;
        }
        let Some(pane) = self.panes.get(id) else { return };
        let Some(ix) = pane.tabs.iter().position(|t| t.path == from) else { return };
        let language = files::language(&to);
        let tab = &pane.tabs[ix];
        if pane.active == Some(ix) && language != tab.language && !tab.dirty && tab.note.is_none() {
            self.close_tab(id, ix, cx);
            self.open_file(id, to, None, window, cx);
            return;
        }
        let Some(tab) = self.panes.get_mut(id).map(|p| &mut p.tabs[ix]) else { return };
        Self::tell_server_closed(tab);
        (tab.path, tab.server, tab.version) = (to.clone(), None, 0);
        if tab.note.is_none() {
            tab.language = language;
        }
        self.attach_server(id, &to, cx);
        cx.notify();
    }

    fn follow_folder_rename(&mut self, id: &Id, from: &std::path::Path, to: &std::path::Path, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        let moved = |path: &std::path::Path| path.strip_prefix(from).ok().map(|rest| to.join(rest));
        pane.expanded = pane.expanded.iter().map(|key| moved(std::path::Path::new(key)).map_or_else(|| key.clone(), |p| p.to_string_lossy().into_owned())).collect();
        let mut retargeted = vec![];
        for tab in pane.tabs.iter_mut() {
            let Some(path) = moved(&tab.path) else { continue };
            Self::tell_server_closed(tab);
            (tab.path, tab.server, tab.version) = (path.clone(), None, 0);
            retargeted.push(path);
        }
        for path in retargeted {
            self.attach_server(id, &path, cx);
        }
        cx.notify();
    }

    /// Delete off the UI thread (a folder can hold many files); tabs inside it close once it is gone.
    pub(super) fn delete_path(&mut self, id: Id, path: PathBuf, cx: &mut Context<Self>) {
        let Some(root) = self.panes.get(&id).map(|p| p.root.clone()) else { return };
        let target = path.clone();
        cx.spawn(async move |this, cx| {
            let done = cx.background_executor().spawn(async move { files::delete(&root, &target) }).await;
            let _ = this.update(cx, |ws, cx| {
                match done {
                    Ok(()) => {
                        while let Some(ix) = ws.panes.get(&id).and_then(|p| p.tabs.iter().position(|t| crate::platform::is_same_or_under(&t.path, &path))) {
                            ws.close_tab(&id, ix, cx);
                        }
                    }
                    Err(e) => ws.toast(format!("Could not delete it: {e}"), true, cx),
                }
                ws.refresh_files(cx);
            });
        })
        .detach();
    }
}
