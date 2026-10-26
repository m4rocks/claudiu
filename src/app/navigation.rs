//! Moving around in editor mode: back and forward through the places the editor left, go to line, and the recently
//! shown files that quick open lists first.

use std::path::Path;

use gpui_kit::component::input::Position;

use super::*;

/// Most places kept for back and forward, and most recent files.
const KEPT: usize = 50;

impl Workspace {
    /// Leave a note of where the shown tab is before showing `to` (at `line`), for `go_back`. A new jump forgets the
    /// way forward. Showing the tab that is already shown, without a line, goes nowhere.
    pub(super) fn remember_place(&mut self, id: &Id, to: &Path, line: Option<usize>, cx: &App) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        let Some(here) = current_place(pane, cx) else { return };
        if here.0 == to && line.is_none() {
            return;
        }
        push_kept(&mut pane.back, here);
        pane.forward.clear();
    }

    /// Put `path` first among the recently shown files.
    pub(super) fn note_recent(&mut self, id: &Id, path: &Path) {
        if let Some(pane) = self.panes.get_mut(id) {
            pane.recent.retain(|p| p != path);
            pane.recent.insert(0, path.to_path_buf());
            pane.recent.truncate(KEPT);
        }
    }

    /// A file or folder was renamed: the places and recent files under it follow, so Back does not open a name that is gone.
    pub(super) fn retarget_history(&mut self, id: &Id, from: &Path, to: &Path) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        for path in pane.back.iter_mut().chain(pane.forward.iter_mut()).map(|(p, _)| p).chain(pane.recent.iter_mut()) {
            moved_path(path, from, to);
        }
    }

    /// A file or folder was deleted: nothing under it stays among the places and recent files.
    pub(super) fn forget_history(&mut self, id: &Id, gone: &Path) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        forget(&mut pane.back, gone, |(p, _)| p);
        forget(&mut pane.forward, gone, |(p, _)| p);
        forget(&mut pane.recent, gone, |p| p);
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step_through_places(true, window, cx);
    }

    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step_through_places(false, window, cx);
    }

    fn step_through_places(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().filter(|_| self.editing()).cloned() else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        let here = current_place(pane, cx);
        let (from, to) = if back { (&mut pane.back, &mut pane.forward) } else { (&mut pane.forward, &mut pane.back) };
        let Some((path, line)) = from.pop() else { return };
        if let Some(here) = here {
            push_kept(to, here);
        }
        self.note_recent(&id, &path);
        self.show_file(&id, path, Some(line as usize), window, cx);
    }

    /// Ask for a line to put the cursor on in the shown tab.
    pub fn ask_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().filter(|_| self.editing()).cloned() else { return };
        if self.panes.get(&id).and_then(|p| p.shown()).is_none_or(|t| t.note.is_some()) {
            return;
        }
        let input = self.modal_input("", "Line, or line:column", window, cx);
        self.modal = Some(Modal::GoToLine { id, input });
    }

    /// Put the shown tab's cursor where `text` says; `Err` says why it can't, and the dialog stays open.
    pub(super) fn go_to_line(&mut self, id: &Id, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let (line, column) = parse_line(text).ok_or("Type a line number, such as 42 or 42:7.")?;
        let path = self.panes.get(id).and_then(|p| p.shown()).map(|t| t.path.clone()).ok_or("No file is shown.")?;
        self.remember_place(id, &path, Some(line as usize), cx);
        let Some(tab) = self.panes.get(id).and_then(|p| p.shown()) else { return Ok(()) };
        tab.buffer.update(cx, |b, cx| {
            b.set_cursor_position(Position::new(line, column), window, cx);
            b.focus(window, cx);
        });
        Ok(())
    }
}

/// The shown tab's file and cursor line.
fn current_place(pane: &EditorPane, cx: &App) -> Option<(PathBuf, u32)> {
    let tab = pane.shown()?;
    Some((tab.path.clone(), tab.buffer.read(cx).cursor_position().line))
}

/// `path` after `from` (itself or a folder above it) became `to`.
fn moved_path(path: &mut PathBuf, from: &Path, to: &Path) {
    if path == from {
        *path = to.to_path_buf();
    } else if let Ok(rest) = path.strip_prefix(from) {
        *path = to.join(rest);
    }
}

fn forget<T>(list: &mut Vec<T>, gone: &Path, path_of: impl Fn(&T) -> &PathBuf) {
    list.retain(|item| !crate::platform::is_same_or_under(path_of(item), gone));
}

fn push_kept(places: &mut Vec<(PathBuf, u32)>, place: (PathBuf, u32)) {
    if places.last() != Some(&place) {
        places.push(place);
    }
    if places.len() > KEPT {
        places.remove(0);
    }
}

/// `42` or `42:7` (1-based, as people count) as a 0-based line and column. Past the end is fine: the editor clamps it.
fn parse_line(text: &str) -> Option<(u32, u32)> {
    let (line, column) = text.trim().split_once(':').unwrap_or((text.trim(), "1"));
    let (line, column): (u32, u32) = (line.trim().parse().ok()?, column.trim().parse().ok()?);
    Some((line.checked_sub(1)?, column.saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::{forget, moved_path, parse_line, push_kept};
    use std::path::{Path, PathBuf};

    #[test]
    fn a_renamed_folder_takes_the_places_under_it_along() {
        let (from, to) = (Path::new("/p/old"), Path::new("/p/new"));
        let mut path = PathBuf::from("/p/old/src/a.rs");
        moved_path(&mut path, from, to);
        assert_eq!(path, Path::new("/p/new/src/a.rs"));
        let mut itself = PathBuf::from("/p/old");
        moved_path(&mut itself, from, to);
        assert_eq!(itself, to, "the folder itself, without a trailing separator");
        let mut other = PathBuf::from("/p/older/a.rs");
        moved_path(&mut other, from, to);
        assert_eq!(other, Path::new("/p/older/a.rs"), "a name that merely starts the same stays");
    }

    #[test]
    fn a_deleted_file_or_folder_leaves_no_place_behind() {
        let mut places = vec![(PathBuf::from("/p/a.rs"), 3), (PathBuf::from("/p/dir/b.rs"), 1), (PathBuf::from("/p/dir2/c.rs"), 2)];
        forget(&mut places, Path::new("/p/dir"), |(p, _)| p);
        assert_eq!(places.iter().map(|(p, _)| p.to_str().unwrap()).collect::<Vec<_>>(), ["/p/a.rs", "/p/dir2/c.rs"]);
        let mut recent = vec![PathBuf::from("/p/a.rs")];
        forget(&mut recent, Path::new("/p/a.rs"), |p| p);
        assert!(recent.is_empty());
    }

    #[test]
    fn a_line_and_column_count_from_one() {
        assert_eq!(parse_line("42"), Some((41, 0)));
        assert_eq!(parse_line(" 42:7 "), Some((41, 6)));
        assert_eq!(parse_line("1:0"), Some((0, 0)), "column 0 is taken as the first");
        assert_eq!(parse_line("0"), None, "there is no line 0");
        assert_eq!(parse_line("abc"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn places_skip_repeats_and_keep_the_latest() {
        let mut places = vec![];
        push_kept(&mut places, (PathBuf::from("a"), 1));
        push_kept(&mut places, (PathBuf::from("a"), 1));
        assert_eq!(places.len(), 1, "the same place twice in a row is one step");
        for line in 0..60 {
            push_kept(&mut places, (PathBuf::from("b"), line));
        }
        assert_eq!(places.len(), super::KEPT);
        assert_eq!(places.last(), Some(&(PathBuf::from("b"), 59)));
    }
}
