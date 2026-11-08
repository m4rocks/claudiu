//! Point Claude at the editor's selection: moving the keyboard to the session's terminal while lines are selected puts
//! `@path#L12-30: ` into Claude's input, so the user types their comment after it on the same line. Leaving the
//! terminal without typing takes it back out; once the user typed after it, it stays. This is the one place Claudiu
//! types into a session's terminal (see CLAUDE.md).

use super::*;

impl Workspace {
    /// After every handler: when the keyboard moved, take the reference back out of the terminal it left and put one
    /// into the terminal it reached.
    pub(super) fn follow_terminal_focus(&mut self) {
        let now = self.keys_to.clone().filter(|_| !self.search.focused && self.modal.is_none());
        if now == self.focus_seen {
            return;
        }
        let before = std::mem::replace(&mut self.focus_seen, now.clone());
        if let Some(t) = before.and_then(|key| self.terminal_mut(&key)) {
            t.withdraw_reference();
        }
        if let Some(id) = now {
            self.on_terminal_focus(&id);
        }
    }

    /// Moving the keyboard to `key` on purpose (a request sent to Claude) without putting a reference in.
    pub(super) fn focus_without_reference(&mut self) {
        self.focus_seen = self.keys_to.clone();
    }

    fn on_terminal_focus(&mut self, id: &Id) {
        if !self.editing() || self.active_id() != Some(id) || self.store.session(id).is_none_or(|r| r.kind != SessionKind::Claude) {
            return;
        }
        let Some(reference) = self.selection_reference(id) else { return };
        let Some(view) = self.terminal_mut(id) else { return };
        // Still in Claude's input from an earlier visit, where the user kept it by typing after it. Once sent, it can
        // be put in again.
        if !view.input_holds(&reference) {
            view.insert_reference(&reference);
        }
    }
}
