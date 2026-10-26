//! Quick open and find in files: a box over the editor that lists files (by name) or lines (by text) as the user types,
//! and opens the chosen one. Ranking and searching live in `files`; text search runs off the UI thread.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::input::{InputEvent, InputState};

use super::*;
use crate::files::{self, Hit};

/// Most rows a picker lists.
const SHOWN: usize = 200;
/// Find in files waits this long after the last keystroke, so typing a word searches once.
const SEARCH_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Files,
    Text,
}

pub struct Picker {
    pub kind: PickerKind,
    pub input: Entity<InputState>,
    pub hits: Vec<Hit>,
    pub selected: usize,
    /// Bumped on every keystroke and when the picker goes, so an older search stops early and can't replace a newer
    /// one's results. Shared with the search running off the UI thread.
    generation: Arc<AtomicU64>,
    _sub: Subscription,
}

impl Drop for Picker {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Workspace {
    /// Open quick open or find in files over the shown session's editor; asking for the one already up closes it.
    pub fn open_picker(&mut self, kind: PickerKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().filter(|_| self.editing()).cloned() else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        if pane.picker.as_ref().is_some_and(|p| p.kind == kind) {
            self.close_picker(window, cx);
            return;
        }
        let placeholder = match kind {
            PickerKind::Files => "Go to file",
            PickerKind::Text => "Find in files",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let owner = id.clone();
        let sub = cx.subscribe_in(&input, window, move |ws: &mut Workspace, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => ws.update_picker(&owner, cx),
            InputEvent::PressEnter { .. } => ws.choose_picked(window, cx),
            _ => {}
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        pane.picker = Some(Picker { kind, input, hits: vec![], selected: 0, generation: Default::default(), _sub: sub });
        self.update_picker(&id, cx);
        cx.notify();
    }

    pub fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.active_id().cloned();
        if let Some(pane) = id.and_then(|id| self.panes.get_mut(&id)) {
            pane.picker = None;
            if let Some(buffer) = pane.shown().map(|t| t.buffer.clone()) {
                buffer.update(cx, |b, cx| b.focus(window, cx));
            }
        }
        cx.notify();
    }

    fn shown_picker(&mut self) -> Option<&mut Picker> {
        let id = self.active_id().cloned()?;
        self.panes.get_mut(&id)?.picker.as_mut()
    }

    /// Move the highlighted row by `by`, staying within the list.
    pub fn move_picked(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(p) = self.shown_picker() else { return };
        p.selected = p.selected.saturating_add_signed(by).min(p.hits.len().saturating_sub(1));
        cx.notify();
    }

    /// Highlight row `ix`.
    pub fn pick(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(p) = self.shown_picker() {
            p.selected = ix.min(p.hits.len().saturating_sub(1));
            cx.notify();
        }
    }

    pub fn choose_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(p) = self.panes.get(&id).and_then(|p| p.picker.as_ref()) else { return };
        let Some(hit) = p.hits.get(p.selected).cloned() else { return };
        let line = (p.kind == PickerKind::Text).then_some(hit.line);
        self.close_picker(window, cx);
        self.open_file(&id, hit.path, line, window, cx);
    }

    pub(super) fn update_picker(&mut self, id: &Id, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        let (all, rel, recent) = (pane.files.clone(), pane.relative.clone(), pane.recent.clone());
        let Some(p) = pane.picker.as_mut() else { return };
        let query = p.input.read(cx).value().to_string();
        let generation = p.generation.fetch_add(1, Ordering::Relaxed) + 1;
        p.selected = 0;
        match p.kind {
            PickerKind::Files => {
                p.hits = recent_first(&query, &all, &rel, &recent)
                    .into_iter()
                    .map(|i| Hit { path: all[i].clone(), line: 0, text: rel[i].clone() })
                    .collect();
                cx.notify();
            }
            PickerKind::Text => {
                let latest = p.generation.clone();
                let id = id.clone();
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SEARCH_DELAY).await;
                    let stale = move || latest.load(Ordering::Relaxed) != generation;
                    if stale() {
                        return;
                    }
                    let searching = stale.clone();
                    let hits = cx.background_executor().spawn(async move { files::search(&all, &query, SHOWN, &searching) }).await;
                    let _ = this.update(cx, |ws, cx| {
                        if !stale()
                            && let Some(p) = ws.panes.get_mut(&id).and_then(|p| p.picker.as_mut())
                        {
                            p.hits = hits;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
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

/// `path` under the listed folder, with `/` separators, for showing and for matching.
pub fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
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
