//! Point Claude at the editor's selection: moving the keyboard to the session's terminal while lines are selected puts
//! `@path#L12-30: ` into Claude's input, so the user types their comment after it on the same line. Leaving the
//! terminal without typing takes it back out; once the user typed after it, it stays.

use gpui_kit::component::input::RopeExt;

use super::*;

impl Workspace {
    /// Have the session's terminal watch its keyboard focus, for `on_terminal_focus` and `on_terminal_blur`.
    pub(super) fn watch_terminal_focus(&self, id: &Id, view: &Entity<TerminalView>, window: &mut Window, cx: &mut Context<Self>) -> [Subscription; 2] {
        let focus = view.read(cx).focus_handle(cx);
        let (focused, blurred) = (id.clone(), id.clone());
        [
            cx.on_focus(&focus, window, move |ws, _w, cx| ws.on_terminal_focus(&focused, cx)),
            cx.on_blur(&focus, window, move |ws, _w, cx| ws.on_terminal_blur(&blurred, cx)),
        ]
    }

    fn on_terminal_focus(&mut self, id: &Id, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.skip_selection_reference) {
            return;
        }
        if !self.editing() || self.active_id() != Some(id) || self.store.session(id).is_none_or(|r| r.kind != SessionKind::Claude) {
            return;
        }
        let Some(reference) = self.selection_reference(id, cx) else { return };
        let Some(view) = self.live_view(id) else { return };
        // Still in Claude's input from an earlier visit, where the user kept it by typing after it. Once sent, it can
        // be put in again.
        if view.read(cx).input_holds(&reference) {
            return;
        }
        view.update(cx, |v, cx| v.insert_reference(&reference, cx));
    }

    fn on_terminal_blur(&mut self, id: &Id, cx: &mut Context<Self>) {
        self.skip_selection_reference = false;
        let Some(view) = self.live_view(id) else { return };
        view.update(cx, |v, cx| v.withdraw_reference(cx));
    }

    /// `@path#L..` for the shown tab's selection; `None` when nothing is selected.
    pub(super) fn selection_reference(&self, id: &Id, cx: &App) -> Option<String> {
        let pane = self.panes.get(id)?;
        let tab = pane.shown().filter(|t| t.note.is_none())?;
        let b = tab.buffer.read(cx);
        let range = b.selected_range();
        if range.is_empty() {
            return None;
        }
        let start = b.text().offset_to_point(range.start);
        let end = b.text().offset_to_point(range.end);
        // Relative to the listed folder, which is the session's own: the path Claude Code resolves `@` against.
        let path = super::picker::relative(&tab.path, &pane.root);
        Some(reference(&path, start.row, end.row, end.column))
    }
}

/// `@path#L<first>-<last>` (1-based, or `#L<n>` for one line) for a selection from row `start` to the point `end_row`,
/// `end_col` (0-based). A selection that ends at the very start of a line doesn't take that line.
fn reference(path: &str, start: usize, end_row: usize, end_col: usize) -> String {
    let last = if end_row > start && end_col == 0 { end_row - 1 } else { end_row };
    match last == start {
        true => format!("@{path}#L{}", start + 1),
        false => format!("@{path}#L{}-{}", start + 1, last + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::reference;

    #[test]
    fn references_name_the_selected_lines() {
        assert_eq!(reference("src/main.rs", 11, 29, 4), "@src/main.rs#L12-30");
        assert_eq!(reference("a.rs", 4, 4, 9), "@a.rs#L5", "within one line");
        assert_eq!(reference("a.rs", 4, 7, 0), "@a.rs#L5-7", "whole lines, ending at the start of the next");
        assert_eq!(reference("a.rs", 4, 5, 0), "@a.rs#L5");
    }
}
