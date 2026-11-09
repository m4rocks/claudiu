//! Hand-off: a running Claude session moves to the active account. Claude Code can't change the account of a running
//! process, so its process is replaced by a new one that resumes the same conversation under the active account. The Claudiu session is the same record and keeps its shell
//! and cards; the old process ends. The record remembers its new account, so it starts there from then on. The right sidebar offers the hand-off for a session that
//! runs under another account than the active one.

use super::*;
use crate::terminal::view::Handover;
use std::time::Instant;

impl Workspace {
    /// The right sidebar offer: a running session that runs under another account than the active one.
    /// Returns the account it runs under, and the active account's name.
    pub fn session_away(&self, id: &str) -> Option<(String, String)> {
        let live = self.live.iter().find(|l| l.id == id)?;
        if !self.is_running(id) {
            return None;
        }
        let active = self.store.active_account()?;
        if live.account == active.id {
            return None;
        }
        let from = self.store.account(&live.account).map_or_else(|| "another account".into(), |a| a.name());
        Some((from, active.name()))
    }

    /// The right sidebar button. Asks first; the shown session is restarted under the active account.
    pub fn ask_handoff(&mut self) {
        let shown = self.active_id().cloned().filter(|id| self.is_running(id) && self.store.session(id).is_some_and(|r| r.claude.is_some()));
        let Some(id) = shown else { return };
        let to_name = self.store.active_account().map_or_else(|| "the active account".into(), |a| a.name());
        self.modal = Some(Modal::Confirm {
            title: format!("Hand off to {to_name}?"),
            body: format!(
                "Claude Code can't change accounts in a running session. Claudiu restarts this session \
                 under {to_name} with the same conversation. Its history is kept."
            ),
            confirm: "Hand off".into(),
            danger: false,
            act: Act::ConfirmHandoff(id),
        });
    }

    /// Confirmed: the process is replaced at once, without compacting; the new one resumes the same conversation.
    pub fn start_handoff(&mut self, id: &str) {
        self.restart_handed_off(id, Instant::now());
    }

    /// The old process ends and a new one resumes the same conversation. The shell and cards belong to the session,
    /// so they stay.
    fn restart_handed_off(&mut self, id: &str, since: Instant) {
        let to = self.store.active_account().map(|a| a.id.clone());
        if let Some(meta) = self.store.session_mut(id).and_then(|r| r.claude.as_mut()) {
            meta.account = to;
        }
        self.restart_session(id);
        let to_name = self.store.active_account().map_or_else(|| "the active account".into(), |a| a.name());
        self.set_handover(id, Some(Handover { message: format!("Handing over to {to_name}…"), since }));
    }

    /// The skeleton says "Handing over to …" while this is set; the new process clears it once its prompt is ready.
    pub(super) fn set_handover(&mut self, id: &str, handover: Option<Handover>) {
        if let Some(view) = self.terminal_mut(id) {
            view.handover = handover;
        }
    }
}
