//! Editing files in editor mode: saving, following changes Claude makes on disk, and never dropping unsaved work.
//! A clean tab reloads when its file changes; a tab with unsaved edits shows a conflict instead, and the user picks.

use std::path::Path;

use super::*;
use crate::editor::Mark;
use crate::files::{self, Contents, SaveError, Stamp};

/// Why a tab's file and its text disagree in a way the user has to settle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Conflict {
    /// Changed on disk while the tab had unsaved edits.
    Changed,
    Deleted,
}

impl Workspace {
    /// Save the shown tab of the shown session. Refuses, and shows the conflict, when the file changed on disk meanwhile.
    pub fn save_shown(&mut self) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(tab) = self.panes.get_mut(&id).and_then(|p| p.shown_mut()).filter(|t| t.note.is_none()) else { return };
        tab.saving = true;
        let (path, format, expected) = (tab.path.clone(), tab.format.clone(), tab.stamp);
        let text = tab.editor.buffer.text();
        let version = tab.editor.buffer.version();
        let write = path.clone();
        self.rt.spawn(move || files::save(&write, &text, format, expected), move |ws: &mut Workspace, saved| ws.on_saved(&id, &path, version, saved));
    }

    fn on_saved(&mut self, id: &Id, path: &Path, version: u64, saved: Result<Stamp, SaveError>) {
        let mut tab = self.panes.get_mut(id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path));
        if let Some(tab) = tab.as_deref_mut() {
            tab.saving = false;
        }
        match (saved, tab) {
            (Ok(stamp), Some(tab)) => {
                tab.stamp = Some(stamp);
                // Typing while the file was written leaves edits the file doesn't have yet.
                if tab.editor.buffer.version() == version {
                    tab.editor.buffer.mark_saved();
                }
                tab.dirty = tab.editor.buffer.is_dirty();
                tab.conflict = None;
                self.refresh_marks();
            }
            (Ok(_), None) => {}
            (Err(SaveError::Changed), tab) => {
                if let Some(tab) = tab {
                    tab.conflict = Some(Conflict::Changed);
                }
                self.toast("Not saved: the file changed on disk. Reload it or keep your version.", true);
            }
            (Err(SaveError::Io(e)), _) => self.toast(format!("Could not save {}: {e}", path.display()), true),
        }
    }

    /// Look at the shown session's open files on disk (once a second, off the UI thread): reload the clean ones that
    /// changed, and flag a conflict on the ones with unsaved edits.
    pub(super) fn check_open_files(&mut self) {
        let Some(id) = self.active_id().filter(|_| self.editing()).cloned() else { return };
        let Some(pane) = self.panes.get(&id) else { return };
        // A tab without a stamp follows nothing: it was never read, or the user chose to keep their version.
        let watched: Vec<(PathBuf, Stamp)> = pane.tabs.iter().filter(|t| !t.saving).filter_map(|t| Some((t.path.clone(), t.stamp?))).collect();
        if watched.is_empty() {
            return;
        }
        self.rt.spawn(
            move || {
                watched
                    .into_iter()
                    .filter_map(|(path, seen)| {
                        let now = files::stamp(&path);
                        (now != Some(seen)).then_some((path, seen, now))
                    })
                    .collect::<Vec<_>>()
            },
            move |ws: &mut Workspace, changed| {
                for (path, seen, now) in changed {
                    let Some(tab) = ws.panes.get_mut(&id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { continue };
                    // Saved or reloaded since the look began (or being saved now): the new stamp is the tab's own.
                    if tab.saving || tab.stamp != Some(seen) {
                        continue;
                    }
                    match (now, tab.dirty) {
                        (Some(_), false) => ws.reload_tab(&id, path),
                        (Some(_), true) => tab.conflict = Some(Conflict::Changed),
                        (None, _) => tab.conflict = Some(Conflict::Deleted),
                    }
                }
            },
        );
    }

    /// Read the tab's file again, dropping unsaved edits; the caret stays where it was.
    pub fn reload_tab(&mut self, id: &Id, path: PathBuf) {
        let id = id.clone();
        let read = path.clone();
        self.rt.spawn(
            move || (files::read(&read), files::stamp(&read)),
            move |ws: &mut Workspace, (contents, now)| {
                let Some(tab) = ws.panes.get_mut(&id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
                match contents {
                    Ok(Contents::Text { text, format, stamp }) => {
                        tab.editor.set_text(&text);
                        tab.editor.edited = false;
                        (tab.format, tab.stamp, tab.dirty, tab.conflict) = (format, Some(stamp), false, None);
                        ws.refresh_marks();
                    }
                    // Grew too large, stopped being text, or vanished: keep what is shown and let the user decide. The
                    // stamp moves on so the once-a-second check does not read the same file again until it changes.
                    _ => {
                        tab.conflict = Some(if now.is_some() { Conflict::Changed } else { Conflict::Deleted });
                        tab.stamp = now.or(tab.stamp);
                    }
                }
            },
        );
    }

    /// Settle a conflict by keeping the editor's text: the next save writes it over whatever is on disk.
    pub fn keep_mine(&mut self, id: &Id, path: &Path) {
        if let Some(tab) = self.panes.get_mut(id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) {
            tab.conflict = None;
            tab.stamp = None;
            tab.dirty = true;
        }
    }

    /// Ctrl+W: close the shown tab, asking first when it has unsaved edits.
    pub fn close_shown_tab(&mut self) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(ix) = self.panes.get(&id).and_then(|p| p.active) else { return };
        self.request_close_tab(&id, ix);
    }

    /// Close tab `ix`, asking first when it has unsaved edits.
    pub fn request_close_tab(&mut self, id: &Id, ix: usize) {
        let Some(tab) = self.panes.get(id).and_then(|p| p.tabs.get(ix)) else { return };
        if !tab.dirty {
            self.close_tab(id, ix);
            return;
        }
        let name = tab.path.file_name().unwrap_or_default().to_string_lossy().to_string();
        self.modal = Some(Modal::Confirm {
            title: format!("Discard changes to {name}?"),
            body: "Your unsaved edits to this file will be lost.".into(),
            confirm: "Discard".into(),
            danger: true,
            act: Act::DiscardTab(id.clone(), tab.path.clone()),
        });
        self.modal_serial += 1;
    }

    /// Files with unsaved edits, in one session's pane or (with `None`) in all of them.
    pub fn unsaved(&self, id: Option<&Id>) -> usize {
        self.panes.iter().filter(|(pid, _)| id.is_none_or(|id| id == *pid)).flat_map(|(_, p)| &p.tabs).filter(|t| t.dirty).count()
    }

    /// End a session, asking first when its editor has unsaved files.
    pub(super) fn request_close_live(&mut self, id: &Id) {
        let unsaved = self.unsaved(Some(id));
        if unsaved == 0 {
            self.close_live(id);
            return;
        }
        self.modal = Some(Modal::Confirm {
            title: "End session?".into(),
            body: format!("{unsaved} file{} in its editor {} unsaved changes that will be lost.", plural(unsaved), if unsaved == 1 { "has" } else { "have" }),
            confirm: "End session".into(),
            danger: true,
            act: Act::ConfirmCloseLive(id.clone()),
        });
        self.modal_serial += 1;
    }

    /// Re-read, off the UI thread, which lines of the shown file differ from the last commit.
    pub(super) fn refresh_marks(&mut self) {
        let Some(id) = self.active_id().filter(|_| self.editor_mode).cloned() else { return };
        let Some(path) = self.panes.get(&id).and_then(|p| p.shown()).filter(|t| t.note.is_none()).map(|t| t.path.clone()) else { return };
        let read = path.clone();
        self.rt.spawn(
            move || marks_of(&crate::git::lines::changed_lines(&read)),
            move |ws: &mut Workspace, marks| {
                let Some(tab) = ws.panes.get_mut(&id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
                if tab.editor.marks != marks {
                    tab.editor.marks = marks;
                    tab.editor.dirty = true;
                }
            },
        );
    }
}

/// Git's changes as the editor's gutter shows them: a mark on each added or changed line, and one where lines went.
fn marks_of(changes: &[crate::git::lines::Change]) -> Vec<(usize, Mark)> {
    use crate::git::lines::Kind;
    let mut marks = vec![];
    for c in changes {
        match c.kind {
            Kind::Deleted => marks.push((c.lines.start, Mark::Removed)),
            Kind::Added => marks.extend(c.lines.clone().map(|l| (l, Mark::Added))),
            Kind::Modified => marks.extend(c.lines.clone().map(|l| (l, Mark::Modified))),
        }
    }
    marks
}

pub(super) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::lines::{Change, Kind};

    #[test]
    fn each_changed_line_gets_a_mark_and_a_removal_marks_the_line_below_it() {
        let changes = [Change { lines: 0..2, kind: Kind::Added }, Change { lines: 5..5, kind: Kind::Deleted }, Change { lines: 7..8, kind: Kind::Modified }];
        assert_eq!(marks_of(&changes), [(0, Mark::Added), (1, Mark::Added), (5, Mark::Removed), (7, Mark::Modified)]);
    }
}
