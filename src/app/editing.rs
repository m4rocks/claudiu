//! Editing files in editor mode: saving, following changes Claude makes on disk, and never dropping unsaved work.
//! A clean tab reloads when its file changes; a tab with unsaved edits shows a conflict instead, and the user picks.

use std::path::Path;

use gpui_kit::component::input::Position;

use super::*;
use crate::files::{self, Contents, SaveError, Stamp};

/// Why a tab's file and its buffer disagree in a way the user has to settle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Conflict {
    /// Changed on disk while the tab had unsaved edits.
    Changed,
    Deleted,
}

impl Workspace {
    /// Save the shown tab of the shown session. Refuses, and shows the conflict, when the file changed on disk meanwhile.
    pub fn save_shown(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        let Some(tab) = pane.active.and_then(|ix| pane.tabs.get_mut(ix)).filter(|t| t.note.is_none()) else { return };
        tab.saving = true;
        let (path, format, expected) = (tab.path.clone(), tab.format.clone(), tab.stamp);
        let text = tab.buffer.read(cx).value().to_string();
        cx.spawn(async move |this, cx| {
            let (write, written) = (path.clone(), text.clone());
            let saved = cx.background_executor().spawn(async move { files::save(&write, &written, format, expected) }).await;
            let _ = this.update(cx, |ws, cx| {
                let mut tab = ws.panes.get_mut(&id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path));
                if let Some(tab) = tab.as_deref_mut() {
                    tab.saving = false;
                }
                match (saved, tab) {
                    (Ok(stamp), Some(tab)) => {
                        tab.stamp = Some(stamp);
                        // Typing while the file was written leaves edits the file doesn't have yet.
                        tab.dirty = tab.buffer.read(cx).value() != text.as_str();
                        tab.conflict = None;
                        ws.tell_server_saved(&id, &path);
                        ws.refresh_marks(cx);
                    }
                    (Ok(_), None) => {}
                    (Err(SaveError::Changed), tab) => {
                        if let Some(tab) = tab {
                            tab.conflict = Some(Conflict::Changed);
                        }
                        ws.toast("Not saved: the file changed on disk. Reload it or keep your version.", true, cx);
                    }
                    (Err(SaveError::Io(e)), _) => ws.toast(format!("Could not save {}: {e}", path.display()), true, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Look at the shown session's open files on disk (once a second, off the UI thread): reload the clean ones that
    /// changed, and flag a conflict on the ones with unsaved edits.
    pub(super) fn check_open_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().filter(|_| self.editing()).cloned() else { return };
        let Some(pane) = self.panes.get(&id) else { return };
        // A tab without a stamp follows nothing: it was never read, or the user chose to keep their version.
        let watched: Vec<(PathBuf, Stamp)> = pane.tabs.iter().filter(|t| !t.saving).filter_map(|t| Some((t.path.clone(), t.stamp?))).collect();
        if watched.is_empty() {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let changed: Vec<(PathBuf, Stamp, Option<Stamp>)> = cx
                .background_executor()
                .spawn(async move {
                    watched
                        .into_iter()
                        .filter_map(|(path, seen)| {
                            let now = files::stamp(&path);
                            (now != Some(seen)).then_some((path, seen, now))
                        })
                        .collect()
                })
                .await;
            if changed.is_empty() {
                return;
            }
            let _ = this.update_in(cx, |ws, window, cx| {
                for (path, seen, now) in changed {
                    let Some(tab) = ws.panes.get_mut(&id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { continue };
                    // Saved or reloaded since the look began (or being saved now): the new stamp is the tab's own.
                    if tab.saving || tab.stamp != Some(seen) {
                        continue;
                    }
                    match (now, tab.dirty) {
                        (Some(_), false) => ws.reload_tab(&id, path, window, cx),
                        (Some(_), true) => tab.conflict = Some(Conflict::Changed),
                        (None, _) => tab.conflict = Some(Conflict::Deleted),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Read the tab's file again, dropping unsaved edits; the cursor stays on its line.
    pub fn reload_tab(&mut self, id: &Id, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = path.clone();
            let (contents, now) = cx.background_executor().spawn(async move { (files::read(&read), files::stamp(&read)) }).await;
            let _ = this.update_in(cx, |ws, window, cx| {
                let Some(tab) = ws.panes.get_mut(&id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
                match contents {
                    Ok(Contents::Text { text, format, stamp }) => {
                        tab.buffer.update(cx, |b, cx| {
                            let at = b.cursor_position();
                            b.set_tab_size(super::editor_mode::tab_size(&format), cx);
                            b.set_value(text, window, cx);
                            b.set_cursor_position(Position::new(at.line, 0), window, cx);
                        });
                        (tab.format, tab.stamp, tab.dirty, tab.conflict) = (format, Some(stamp), false, None);
                        ws.sync_with_server(&id, &path, true, cx);
                    }
                    // Grew too large, stopped being text, or vanished: keep what is shown and let the user decide. The
                    // stamp moves on so the once-a-second check does not read the same file again until it changes.
                    _ => {
                        tab.conflict = Some(if now.is_some() { Conflict::Changed } else { Conflict::Deleted });
                        tab.stamp = now.or(tab.stamp);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Settle a conflict by keeping the buffer: the next save writes it over whatever is on disk.
    pub fn keep_mine(&mut self, id: &Id, path: &Path, cx: &mut Context<Self>) {
        if let Some(tab) = self.panes.get_mut(id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) {
            tab.conflict = None;
            tab.stamp = None;
            tab.dirty = true;
            cx.notify();
        }
    }

    /// Close tab `ix`, asking first when it has unsaved edits.
    pub fn request_close_tab(&mut self, id: &Id, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.panes.get(id).and_then(|p| p.tabs.get(ix)) else { return };
        if !tab.dirty {
            self.close_tab(id, ix, cx);
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
        window.focus(&self.modal_focus, cx);
        cx.notify();
    }

    /// Files with unsaved edits, in one session's pane or (with `None`) in all of them.
    pub fn unsaved(&self, id: Option<&Id>) -> usize {
        self.panes.iter().filter(|(pid, _)| id.is_none_or(|id| id == *pid)).flat_map(|(_, p)| &p.tabs).filter(|t| t.dirty).count()
    }

    /// End a session, asking first when its editor has unsaved files.
    pub(super) fn request_close_live(&mut self, id: &Id, window: &mut Window, cx: &mut Context<Self>) {
        let unsaved = self.unsaved(Some(id));
        if unsaved == 0 {
            self.close_live(id, window, cx);
            return;
        }
        self.modal = Some(Modal::Confirm {
            title: "End session?".into(),
            body: format!("{unsaved} file{} in its editor {} unsaved changes that will be lost.", plural(unsaved), if unsaved == 1 { "has" } else { "have" }),
            confirm: "End session".into(),
            danger: true,
            act: Act::ConfirmCloseLive(id.clone()),
        });
        window.focus(&self.modal_focus, cx);
        cx.notify();
    }
}

pub(super) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
