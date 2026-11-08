//! The search bar over the shown file, as main's GPUI editor had it (gpui-kit's search panel): Primary+F opens it with
//! the selection or the last query, Enter and Shift+Enter step through the matches, and the replace row replaces one
//! match or all of them. Replace opens with Cmd+Shift+F on macOS and Ctrl+H elsewhere. The matching is
//! `Editor::find_all`; the bar is drawn by `panels/editor_pane.rs`.

use super::*;
use crate::editor::buffer::Pos;
use crate::terminal::keys::Keystroke;

/// One tab's search: kept while the bar is closed, so opening it again resumes the last query.
pub struct FindBar {
    pub open: bool,
    pub query: TextEdit,
    pub replacement: TextEdit,
    pub replace_mode: bool,
    /// ASCII letters match either case, the default.
    pub ignore_case: bool,
    pub matches: Vec<(Pos, Pos)>,
    pub current: usize,
    /// The query, case and text version the matches were found for.
    searched: Option<(String, bool, u64)>,
}

impl Default for FindBar {
    fn default() -> Self {
        Self { open: false, query: modal::field("", false), replacement: modal::field("", false), replace_mode: false, ignore_case: true, matches: vec![], current: 0, searched: None }
    }
}

impl FindBar {
    /// "2/5", or "0/0" with no match.
    pub fn label(&self) -> String {
        match self.matches.len() {
            0 => "0/0".into(),
            n => format!("{}/{n}", self.current + 1),
        }
    }
}

impl Workspace {
    /// The shown tab's search bar, while editor mode shows one.
    pub fn shown_find(&mut self) -> Option<&mut FindBar> {
        let id = self.active_id().filter(|_| self.editing()).cloned()?;
        self.panes.get_mut(&id)?.shown_mut().filter(|t| t.note.is_none()).map(|t| &mut t.find)
    }

    /// Open the bar over the shown file, with the replace row when `replace`. The selection, if any, becomes the query.
    pub fn open_find(&mut self, replace: bool) {
        let Some(tab) = self.active_id().filter(|_| self.editing()).cloned().and_then(|id| self.panes.get_mut(&id)).and_then(|p| p.shown_mut()) else { return };
        if tab.note.is_some() {
            return;
        }
        let (a, b) = tab.editor.sel.range();
        let selected = tab.editor.buffer.slice(a, b);
        let bar = &mut tab.find;
        bar.open = true;
        bar.replace_mode = replace;
        let query = if selected.is_empty() { bar.query.text.as_str().to_string() } else { selected };
        // The query has the keyboard, as in gpui-kit's panel, ready to be typed over.
        bar.query = modal::field(&query, true);
        bar.query.set_selection(0, query.len());
        bar.replacement.focused = false;
        self.update_find();
    }

    pub fn close_find(&mut self) {
        if let Some(bar) = self.shown_find() {
            bar.open = false;
            bar.query.focused = false;
            bar.replacement.focused = false;
        }
        self.update_find();
        self.focus_pane();
    }

    /// Each frame: find again when the query, its case or the text changed, and show the matches in the editor. A new
    /// query starts at the first match from the top of the screen; an edit keeps the current match's place.
    pub(super) fn update_find(&mut self) {
        let Some(tab) = self.active_id().cloned().and_then(|id| self.panes.get_mut(&id)).and_then(|p| p.shown_mut()) else { return };
        let (bar, e) = (&mut tab.find, &mut tab.editor);
        if !bar.open {
            if !e.found.is_empty() {
                e.show_found(vec![], None, false);
            }
            bar.searched = None;
            return;
        }
        let query = bar.query.text.as_str().to_string();
        let now = (query.clone(), bar.ignore_case, e.buffer.version());
        if bar.searched.as_ref() == Some(&now) {
            return;
        }
        let new_query = bar.searched.as_ref().is_none_or(|(q, case, _)| *q != query || *case != bar.ignore_case);
        bar.searched = Some(now);
        bar.matches = e.find_all(&query, bar.ignore_case);
        bar.current = if new_query {
            let top = e.first_visible_line();
            bar.matches.iter().position(|(_, b)| b.line >= top).unwrap_or(0)
        } else {
            bar.current.min(bar.matches.len().saturating_sub(1))
        };
        let current = (!bar.matches.is_empty()).then_some(bar.current);
        e.show_found(bar.matches.clone(), current, new_query);
    }

    /// The next match (or the one before), wrapping at the ends.
    pub fn find_step(&mut self, forward: bool) {
        self.update_find();
        let Some(tab) = self.active_id().cloned().and_then(|id| self.panes.get_mut(&id)).and_then(|p| p.shown_mut()) else { return };
        let bar = &mut tab.find;
        let n = bar.matches.len();
        if n == 0 {
            return;
        }
        bar.current = if forward { (bar.current + 1) % n } else { (bar.current + n - 1) % n };
        tab.editor.show_found(bar.matches.clone(), Some(bar.current), true);
    }

    /// Replace the current match; the next one becomes current.
    pub fn replace_one(&mut self) {
        self.update_find();
        let Some(tab) = self.active_id().cloned().and_then(|id| self.panes.get_mut(&id)).and_then(|p| p.shown_mut()) else { return };
        let Some(&(a, b)) = tab.find.matches.get(tab.find.current) else { return };
        let with = tab.find.replacement.text.as_str().to_string();
        let sel = tab.editor.sel;
        tab.editor.replace(a, b, &with, false);
        // The caret stays where the user left it; the bar moves on.
        tab.editor.sel = crate::editor::buffer::Selection { anchor: tab.editor.buffer.clamp(sel.anchor), head: tab.editor.buffer.clamp(sel.head) };
        if tab.find.current + 1 >= tab.find.matches.len() {
            tab.find.current = 0;
        }
        self.update_find();
    }

    /// Replace every match, as one step that one undo takes back.
    pub fn replace_all(&mut self) {
        self.update_find();
        let Some(tab) = self.active_id().cloned().and_then(|id| self.panes.get_mut(&id)).and_then(|p| p.shown_mut()) else { return };
        if tab.find.matches.is_empty() {
            return;
        }
        let with = tab.find.replacement.text.as_str().to_string();
        let e = &mut tab.editor;
        let mut text = e.buffer.text();
        let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        for &(a, b) in tab.find.matches.iter().rev() {
            text.replace_range(starts[a.line] + a.col..starts[b.line] + b.col, &with);
        }
        let sel = e.sel;
        e.replace(Pos::default(), e.buffer.last(), &text, false);
        e.sel = crate::editor::buffer::Selection { anchor: e.buffer.clamp(sel.anchor), head: e.buffer.clamp(sel.head) };
        tab.find.current = 0;
        self.update_find();
    }

    /// A key in one of the bar's fields: Enter and Shift+Enter step through the matches, Escape closes the bar, Tab moves
    /// between the two fields, and the replace keys show or hide the replace row.
    pub fn find_key(&mut self, ev: &rune::KeyEvent) -> bool {
        let shift = ev.mods.shift;
        let chord = Keystroke::from_rune(ev, None).and_then(|ks| find_chord(&ks));
        match &ev.key {
            rune::Key::Enter => self.find_step(!shift),
            rune::Key::Escape => self.close_find(),
            rune::Key::Tab => {
                let Some(bar) = self.shown_find().filter(|b| b.replace_mode) else { return true };
                let to_replacement = bar.query.focused;
                (bar.query.focused, bar.replacement.focused) = (!to_replacement, to_replacement);
            }
            _ if chord == Some(true) => {
                let Some(bar) = self.shown_find() else { return false };
                bar.replace_mode = !bar.replace_mode;
                (bar.query.focused, bar.replacement.focused) = (!bar.replace_mode, bar.replace_mode);
            }
            _ => return false,
        }
        true
    }
}

/// The keys of main's editor search: Primary+F finds (`Some(false)`), and Cmd+Shift+F on macOS or Ctrl+H elsewhere
/// replaces (`Some(true)`).
pub fn find_chord(ks: &Keystroke) -> Option<bool> {
    let replace = if cfg!(target_os = "macos") { "secondary-shift-f" } else { "secondary-h" };
    if commands::is_chord(ks, "secondary-f") {
        Some(false)
    } else if commands::is_chord(ks, replace) {
        Some(true)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_f_finds_and_the_platforms_replace_key_replaces() {
        let key = |spec: &str| find_chord(&Keystroke::parse(spec).unwrap());
        let primary = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
        assert_eq!(key(&format!("{primary}-f")), Some(false));
        let replace = if cfg!(target_os = "macos") { "cmd-shift-f" } else { "ctrl-h" };
        assert_eq!(key(replace), Some(true));
        assert_eq!(key("f"), None);
        assert_eq!(key(&format!("{primary}-alt-f")), None);
    }
}
