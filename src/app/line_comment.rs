//! Editor mode's comments on lines. A small button beside the selection, or beside the line under the mouse
//! (`panels::editor_pane::change_marks` draws it), opens a box to type a request about those lines. Enter adds
//! `@path#L12-30: request` to the session's message box on a line of its own; Cmd/Ctrl+Enter sends it straight away.

use gpui::{Pixels, Point};
use gpui_kit::component::input::{InputEvent, RopeExt};

use super::*;
use crate::panels::comment_box::comment_input;

/// The comment box while it is up: the lines it is about, where it opens, and its input.
pub struct LineComment {
    pub reference: String,
    pub at: Point<Pixels>,
    pub input: Entity<gpui_kit::component::input::InputState>,
    _sub: Subscription,
}

impl Workspace {
    /// Open the comment box at `at` in the window, about the shown tab's selection, or about `line` (0-based) when
    /// nothing is selected.
    pub(crate) fn start_line_comment(&mut self, line: usize, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(reference) = self.selection_reference(&id, cx).or_else(|| self.line_reference(&id, line)) else { return };
        let input = comment_input(&reference, window, cx);
        let sub = cx.subscribe_in(&input, window, |ws: &mut Self, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { secondary, .. } = ev {
                let text = input.read(cx).value().trim().to_string();
                ws.finish_line_comment(&text, *secondary, window, cx);
            }
        });
        let Some(pane) = self.panes.get_mut(&id) else { return };
        pane.comment = Some(LineComment { reference, at, input, _sub: sub });
        cx.notify();
    }

    /// Close the comment box. A comment with text goes into the message box, or is sent when `send`; an empty one is
    /// dropped. The keyboard goes back to the editor, or to the chat once a comment was sent.
    pub(crate) fn finish_line_comment(&mut self, text: &str, send: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(comment) = self.panes.get_mut(&id).and_then(|p| p.comment.take()) else { return };
        cx.notify();
        if text.is_empty() {
            self.focus_editor(&id, window, cx);
            return;
        }
        let message = format!("{}: {text}", comment.reference);
        if send {
            self.send_to_claude(&id, &message, window, cx);
            return;
        }
        let Some(view) = self.live_view(&id).and_then(|v| v.thread()).cloned() else {
            self.toast("This session isn't running. Resume it, then try again.", false, cx);
            return;
        };
        view.update(cx, |v, cx| v.add_comment(&message, window, cx));
        self.focus_editor(&id, window, cx);
    }

    fn focus_editor(&self, id: &Id, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.panes.get(id).and_then(|p| p.shown()) {
            tab.buffer.update(cx, |b, cx| b.focus(window, cx));
        }
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
        // Relative to the listed folder, which is the session's own: the path the agent resolves `@` against.
        let path = super::picker::relative(&tab.path, &pane.root);
        Some(reference(&path, start.row, end.row, end.column))
    }

    /// `@path#L<n>` for `line` (0-based) of the shown tab.
    fn line_reference(&self, id: &Id, line: usize) -> Option<String> {
        let pane = self.panes.get(id)?;
        let tab = pane.shown().filter(|t| t.note.is_none())?;
        Some(reference(&super::picker::relative(&tab.path, &pane.root), line, line, 1))
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
