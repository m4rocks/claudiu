//! Quick open and find in files: a box over the editor that lists files (by name) or lines (by text) as the user types,
//! and opens the chosen one. Ranking and searching live in `files`; text search runs off the UI thread. The box is
//! drawn by `panels/editor_pane.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::files::{self, Hit};

/// Most rows a picker lists.
const SHOWN: usize = 200;
/// Find in files waits this long after the last keystroke, so typing a word searches once.
const SEARCH_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKind {
    Files,
    Text,
}

pub struct Picker {
    pub kind: PickerKind,
    pub input: TextEdit,
    pub hits: Vec<Hit>,
    pub selected: usize,
    /// The query the hits were asked for; the field differing from it means the user typed.
    searched: Option<String>,
    /// Bumped for every query and when the picker goes, so an older search stops early and can't replace a newer
    /// one's results. Shared with the search running off the UI thread.
    generation: Arc<AtomicU64>,
}

impl Drop for Picker {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Workspace {
    /// Open quick open or find in files over the shown session's editor; asking for the one already up closes it.
    pub fn open_picker(&mut self, kind: PickerKind) {
        let Some(id) = self.active_id().filter(|_| self.editing()).cloned() else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        if pane.picker.as_ref().is_some_and(|p| p.kind == kind) {
            self.close_picker();
            return;
        }
        pane.picker = Some(Picker { kind, input: modal::field("", true), hits: vec![], selected: 0, searched: None, generation: Default::default() });
        self.update_picker();
    }

    /// Close the picker; the editor (or the tree) has the keyboard again.
    pub fn close_picker(&mut self) {
        if let Some(pane) = self.active_id().cloned().and_then(|id| self.panes.get_mut(&id)) {
            pane.picker = None;
        }
        self.focus_pane();
    }

    pub fn shown_picker(&mut self) -> Option<&mut Picker> {
        let id = self.active_id().cloned()?;
        self.panes.get_mut(&id)?.picker.as_mut()
    }

    /// Move the highlighted row by `by`, staying within the list.
    pub fn move_picked(&mut self, by: isize) {
        let Some(p) = self.shown_picker() else { return };
        p.selected = p.selected.saturating_add_signed(by).min(p.hits.len().saturating_sub(1));
    }

    /// Highlight row `ix`.
    pub fn pick(&mut self, ix: usize) {
        if let Some(p) = self.shown_picker() {
            p.selected = ix.min(p.hits.len().saturating_sub(1));
        }
    }

    pub fn choose_picked(&mut self) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(p) = self.panes.get(&id).and_then(|p| p.picker.as_ref()) else { return };
        let Some(hit) = p.hits.get(p.selected).cloned() else { return };
        let line = (p.kind == PickerKind::Text).then_some(hit.line);
        self.close_picker();
        self.open_file(&id, hit.path, line);
    }

    /// Each frame: list again when the shown picker's query changed since it was last listed.
    pub(super) fn update_picker(&mut self) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        let (all, rel, recent) = (pane.files.clone(), pane.relative.clone(), pane.recent.clone());
        let Some(p) = pane.picker.as_mut() else { return };
        let query = p.input.text.as_str().to_string();
        if p.searched.as_deref() == Some(query.as_str()) {
            return;
        }
        p.searched = Some(query.clone());
        let generation = p.generation.fetch_add(1, Ordering::Relaxed) + 1;
        p.selected = 0;
        match p.kind {
            PickerKind::Files => {
                p.hits = recent_first(&query, &all, &rel, &recent).into_iter().map(|i| Hit { path: all[i].clone(), line: 0, text: rel[i].clone() }).collect();
            }
            PickerKind::Text => {
                let latest = p.generation.clone();
                self.rt.after(SEARCH_DELAY, move |ws: &mut Workspace| {
                    let stale = move || latest.load(Ordering::Relaxed) != generation;
                    if stale() {
                        return;
                    }
                    let searching = stale.clone();
                    ws.rt.spawn(move || files::search(&all, &query, SHOWN, &searching), move |ws: &mut Workspace, hits| {
                        if !stale()
                            && let Some(p) = ws.panes.get_mut(&id).and_then(|p| p.picker.as_mut())
                        {
                            p.hits = hits;
                        }
                    });
                });
            }
        }
    }

    /// A key in the picker's field: Up and Down move through the list, Escape closes it. Enter is `choose_picked`.
    pub fn picker_key(&mut self, key: &rune::Key) -> bool {
        match key {
            rune::Key::Up => self.move_picked(-1),
            rune::Key::Down => self.move_picked(1),
            rune::Key::Escape => self.close_picker(),
            _ => return false,
        }
        true
    }
}

/// Quick open's order: by how well each path matches `query`, except that with nothing typed the recently shown
/// files come first, latest first.
fn recent_first(query: &str, all: &[PathBuf], rel: &[String], recent: &[PathBuf]) -> Vec<usize> {
    if !query.trim().is_empty() {
        return files::rank_paths(query, rel, SHOWN);
    }
    let mut order = files::rank_paths(query, rel, all.len());
    // Stable, so the rest keep their order.
    order.sort_by_key(|&i| recent.iter().position(|r| *r == all[i]).unwrap_or(usize::MAX));
    order.truncate(SHOWN);
    order
}

#[cfg(test)]
mod tests {
    use super::recent_first;
    use std::path::PathBuf;

    #[test]
    fn recent_files_come_first_until_something_is_typed() {
        let rel: Vec<String> = ["a.rs", "b.rs", "c.rs"].map(String::from).to_vec();
        let all: Vec<PathBuf> = rel.iter().map(PathBuf::from).collect();
        let recent = [PathBuf::from("c.rs"), PathBuf::from("b.rs")];
        assert_eq!(recent_first("", &all, &rel, &recent), [2, 1, 0]);
        assert_eq!(recent_first("a", &all, &rel, &recent), [0], "typing ranks by the match alone");
    }
}
