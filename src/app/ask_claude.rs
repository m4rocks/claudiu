//! The editor's right-click requests to Claude: explain, fix or test the selected lines, or fix the problem a language
//! server reports on the clicked line. Each one sends the reference and the request to Claude as the next message (`send_to_claude`).

use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ask {
    Explain,
    Fix,
    Tests,
    /// The language server's problem on the cursor's line.
    Problem,
}

impl Workspace {
    pub fn ask_claude(&mut self, ask: Ask, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().cloned() else { return };
        let text = match ask {
            Ask::Problem => self.problem_at_cursor(&id, cx).map(|(reference, message)| request(ask, &reference, Some(&message))),
            _ => self.selection_reference(&id, cx).or_else(|| self.cursor_reference(&id, cx)).map(|reference| request(ask, &reference, None)),
        };
        let Some(text) = text else {
            if ask == Ask::Problem {
                self.toast("The language server reports no problem on this line.", false, cx);
            }
            return;
        };
        self.send_to_claude(&id, &text, window, cx);
    }

    /// The shown session when it is a running Claude session: where a request from anywhere in the app goes, rather than
    /// into a session of its own.
    pub(super) fn running_claude(&self, cx: &App) -> Option<Id> {
        self.active_id().filter(|id| self.is_running(id, cx) && self.store.session(id).is_some_and(|r| r.kind == SessionKind::Claude)).cloned()
    }

    /// Ask the Claude session `id` to take `text` as the user's next message, then move the keyboard to its terminal.
    /// It goes through the session's mod, which sends it once the session is idle, so nothing is typed into the
    /// terminal and a half-written prompt there is left alone.
    pub(super) fn send_to_claude(&mut self, id: &Id, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.live_view(id).filter(|v| v.read(cx).is_running()) else {
            self.toast("This session isn't running. Resume it, then try again.", false, cx);
            return;
        };
        usage::send_ctl(&usage::ctl_file(id), "prompt", text.into());
        // Only a change of focus raises `on_terminal_focus`; otherwise the flag would outlive this request.
        self.skip_selection_reference = !view.read(cx).focus_handle(cx).is_focused(window);
        self.focus_active(window, cx);
    }

    /// `@path#L<n>` for the cursor's line of the shown tab.
    fn cursor_reference(&self, id: &Id, cx: &App) -> Option<String> {
        let pane = self.panes.get(id)?;
        let tab = pane.shown().filter(|t| t.note.is_none())?;
        let line = tab.buffer.read(cx).cursor_position().line as usize;
        Some(format!("@{}#L{}", super::picker::relative(&tab.path, &pane.root), line + 1))
    }

    /// The reference and message of the most severe problem on the cursor's line of the shown tab.
    fn problem_at_cursor(&self, id: &Id, cx: &App) -> Option<(String, String)> {
        use gpui_kit::base::input::DiagnosticSeverity as S;
        let pane = self.panes.get(id)?;
        let tab = pane.shown().filter(|t| t.note.is_none())?;
        let b = tab.buffer.read(cx);
        let line = b.cursor_position().line;
        let rank = |s: &S| match s {
            S::Error => 0,
            S::Warning => 1,
            S::Info => 2,
            S::Hint => 3,
        };
        let problem = b.diagnostics()?.iter().filter(|d| (d.diagnostic.range.start.line..=d.diagnostic.range.end.line).contains(&line)).min_by_key(|d| rank(&d.severity))?;
        let (first, last) = (problem.diagnostic.range.start.line as usize, problem.diagnostic.range.end.line as usize);
        let path = super::picker::relative(&tab.path, &pane.root);
        let reference = if first == last { format!("@{path}#L{}", first + 1) } else { format!("@{path}#L{}-{}", first + 1, last + 1) };
        Some((reference, problem.message.to_string()))
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

#[cfg(test)]
mod tests {
    use super::{Ask, request};

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
}
