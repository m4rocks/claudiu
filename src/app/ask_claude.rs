//! The editor's right-click requests to Claude: explain, fix or test the selected lines, or fix the problem a language
//! server reports on the caret's line. Each one sends the reference and the request to Claude as the next message
//! (`send_to_claude`), through the session's mod rather than its terminal.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ask {
    Explain,
    Fix,
    Tests,
    /// The language server's problem on the caret's line.
    Problem,
}

impl Workspace {
    pub fn ask_claude(&mut self, ask: Ask) {
        let Some(id) = self.active_id().cloned() else { return };
        let text = match ask {
            Ask::Problem => self.problem_at_caret(&id).map(|(reference, message)| request(ask, &reference, Some(&message))),
            _ => self.selection_reference(&id).or_else(|| self.caret_reference(&id)).map(|reference| request(ask, &reference, None)),
        };
        let Some(text) = text else {
            if ask == Ask::Problem {
                self.toast("The language server reports no problem on this line.", false);
            }
            return;
        };
        self.send_to_claude(&id, &text);
    }

    /// `@path#L..` for the shown tab's selection; `None` when nothing is selected.
    pub(super) fn selection_reference(&self, id: &Id) -> Option<String> {
        let (path, e) = self.shown_reference_target(id)?;
        if e.sel.is_empty() {
            return None;
        }
        let (a, b) = e.sel.range();
        Some(reference(&path, a.line, b.line, b.col))
    }

    /// `@path#L<n>` for the caret's line of the shown tab.
    fn caret_reference(&self, id: &Id) -> Option<String> {
        let (path, e) = self.shown_reference_target(id)?;
        Some(format!("@{path}#L{}", e.caret().line + 1))
    }

    /// The reference and message of the most severe problem on the caret's line of the shown tab.
    fn problem_at_caret(&self, id: &Id) -> Option<(String, String)> {
        let (path, e) = self.shown_reference_target(id)?;
        let line = e.caret().line;
        // Errors before warnings; `false` sorts first.
        let problem = e.problems.iter().filter(|p| (p.from.line..=p.to.line).contains(&line)).min_by_key(|p| !p.error)?;
        let (first, last) = (problem.from.line, problem.to.line);
        let reference = if first == last { format!("@{path}#L{}", first + 1) } else { format!("@{path}#L{}-{}", first + 1, last + 1) };
        Some((reference, problem.message.clone()))
    }

    /// The shown tab's path as Claude Code resolves `@` (relative to the listed folder, the session's own) and its editor.
    fn shown_reference_target(&self, id: &Id) -> Option<(String, &crate::editor::Editor)> {
        let pane = self.panes.get(id)?;
        let tab = pane.shown().filter(|t| t.note.is_none())?;
        Some((editor_mode::relative(&tab.path, &pane.root), &tab.editor))
    }
}

/// What goes into Claude's input for `ask` about the lines `reference` names; `problem` is the server's message.
fn request(ask: Ask, reference: &str, problem: Option<&str>) -> String {
    match ask {
        Ask::Explain => format!("{reference}: Explain this code."),
        Ask::Fix => format!("{reference}: Fix this code."),
        Ask::Tests => format!("{reference}: Write tests for this code."),
        Ask::Problem => format!("{reference}: Fix this problem: {}", problem.unwrap_or_default().replace('\n', " ")),
    }
}

/// `@path#L<first>-<last>` (1-based, or `#L<n>` for one line) for a selection from line `start` to line `end_line`,
/// column `end_col` (0-based). A selection that ends at the very start of a line doesn't take that line.
fn reference(path: &str, start: usize, end_line: usize, end_col: usize) -> String {
    let last = if end_line > start && end_col == 0 { end_line - 1 } else { end_line };
    match last == start {
        true => format!("@{path}#L{}", start + 1),
        false => format!("@{path}#L{}-{}", start + 1, last + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::{Ask, reference, request};

    #[test]
    fn requests_name_the_lines_and_the_problem_on_one_line() {
        assert_eq!(request(Ask::Explain, "@a.rs#L3-9", None), "@a.rs#L3-9: Explain this code.");
        assert_eq!(request(Ask::Tests, "@a.rs#L3", None), "@a.rs#L3: Write tests for this code.");
        assert_eq!(
            request(Ask::Problem, "@a.rs#L3", Some("mismatched types\nexpected `u8`")),
            "@a.rs#L3: Fix this problem: mismatched types expected `u8`",
            "the request stays on one line"
        );
    }

    #[test]
    fn references_name_the_selected_lines() {
        assert_eq!(reference("src/main.rs", 11, 29, 4), "@src/main.rs#L12-30");
        assert_eq!(reference("a.rs", 4, 4, 9), "@a.rs#L5", "within one line");
        assert_eq!(reference("a.rs", 4, 7, 0), "@a.rs#L5-7", "whole lines, ending at the start of the next");
        assert_eq!(reference("a.rs", 4, 5, 0), "@a.rs#L5");
    }
}
