//! Hand-off: a running Claude session moves to the active account. Claude Code can't change the account of a running
//! process, so the session compacts itself (the mod runs `/compact`), then its process is replaced by a new one that
//! resumes the same conversation under the active account. The Claudiu session is the same record and keeps its shell
//! and cards; the old process ends. The left sidebar's warning only switches the account; the right sidebar offers
//! the hand-off for a session that runs under another one.

use super::*;
use crate::usage::Snapshot;

/// Used share of a limit at which the left sidebar offers to switch accounts.
pub const HIGH: f32 = 0.95;
/// How long a compaction may take before the hand-off is given up.
const TIMEOUT_SECS: i64 = 180;

/// A session waiting to be compacted before it is resumed under the active account.
pub struct Handoff {
    pub asked: i64,
    /// Set once the mod reports a compaction after `asked`.
    pub compacted: bool,
}

/// The left sidebar warning: the active account is at `HIGH` and `to` has used less.
pub struct Offer {
    pub to: Id,
    pub to_name: String,
    pub text: String,
}

impl Workspace {
    /// Derived on every render, so the warning stays until the use drops or the account changes.
    pub fn switch_offer(&self) -> Option<Offer> {
        let t = crate::store::now();
        let active = self.store.active_account()?;
        let peak = active.usage.peak(t);
        if peak < HIGH {
            return None;
        }
        let (best, best_peak) = self
            .store
            .data
            .accounts
            .iter()
            .filter(|a| a.id != active.id && (a.is_main() || a.email.is_some()))
            .map(|a| (a, a.usage.peak(t)))
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .filter(|(_, p)| *p < peak)?;
        let text = format!("{} has used {:.0}% of a usage limit. {} is at {:.0}%.", active.name(), peak * 100.0, best.name(), best_peak * 100.0);
        Some(Offer { to: best.id.clone(), to_name: best.name(), text })
    }

    /// The right sidebar offer: a running session that runs under another account than the active one.
    /// Returns the account it runs under, and the active account's name.
    pub fn session_away(&self, id: &str, cx: &App) -> Option<(String, String)> {
        let live = self.live.iter().find(|l| l.id == id)?;
        if !self.is_running(id, cx) {
            return None;
        }
        let active = self.store.active_account()?;
        if live.account == active.id {
            return None;
        }
        let from = self.store.account(&live.account).map_or_else(|| "another account".into(), |a| a.name());
        Some((from, active.name()))
    }

    /// The right sidebar button. Asks first; the shown session is compacted and restarted under the active account.
    pub fn ask_handoff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.active_id().cloned().filter(|id| self.is_running(id, cx) && self.store.session(id).is_some_and(|r| r.claude.is_some()));
        let Some(id) = shown else { return };
        let to_name = self.store.active_account().map_or_else(|| "the active account".into(), |a| a.name());
        self.modal = Some(Modal::Confirm {
            title: format!("Hand off to {to_name}?"),
            body: format!(
                "Claude Code can't change accounts in a running session. Claudiu has this session compact itself, then restarts \
                 it under {to_name} with the same conversation. Its history is kept."
            ),
            confirm: "Compact and hand off".into(),
            danger: false,
            act: Act::ConfirmHandoff(id),
        });
        window.focus(&self.modal_focus, cx);
    }

    /// Confirmed: the session asks to compact, and its skeleton says it is handing over.
    pub fn start_handoff(&mut self, id: &str, cx: &mut Context<Self>) {
        let to_name = self.store.active_account().map_or_else(|| "the active account".into(), |a| a.name());
        self.handoffs.insert(id.into(), Handoff { asked: crate::store::now(), compacted: false });
        self.set_handover(id, Some(to_name), cx);
        usage::send_ctl(&usage::ctl_file(id), "compact", serde_json::json!(true));
    }

    /// Mod reports from this ingest pass: a session that compacted after its hand-off was asked is ready.
    pub fn note_compactions(&mut self, snaps: &[Snapshot]) {
        for (id, h) in self.handoffs.iter_mut() {
            let Some(cid) = self.store.session(id).and_then(|r| r.claude_id()) else { continue };
            if snaps.iter().any(|s| s.session_id == cid && s.compacted_at.is_some_and(|c| c >= h.asked)) {
                h.compacted = true;
            }
        }
    }

    /// Runs each second with the window: a compacted session is restarted under the active account; one that never
    /// compacts is given up on.
    pub fn finish_handoffs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let t = crate::store::now();
        let due: Vec<(Id, bool)> = self
            .handoffs
            .iter()
            .filter_map(|(id, h)| if h.compacted { Some((id.clone(), true)) } else if t - h.asked > TIMEOUT_SECS { Some((id.clone(), false)) } else { None })
            .collect();
        for (id, compacted) in due {
            self.handoffs.remove(&id);
            if !compacted {
                self.set_handover(&id, None, cx);
                self.toast("The hand-off was cancelled: the session did not compact.", true, cx);
            } else if self.is_running(&id, cx) {
                self.restart_handed_off(&id, window, cx);
            } else {
                self.set_handover(&id, None, cx);
            }
        }
    }

    /// The old process ends and a new one resumes the same conversation. The shell and cards belong to the session,
    /// so they stay.
    fn restart_handed_off(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let was_shown = self.active_id().is_some_and(|a| a == id);
        let shell = self.shells.remove(id);
        self.live.retain(|l| l.id != id); // dropping the view closes the old process
        self.resume(id, window, cx);
        if let Some(shell) = shell {
            self.shells.insert(id.into(), shell);
        }
        if was_shown {
            self.show_session(id, window, cx);
        }
        let to_name = self.store.active_account().map_or_else(|| "the active account".into(), |a| a.name());
        self.set_handover(id, Some(to_name), cx);
    }

    /// The skeleton says "Handing over to …" while this is set; the new process clears it once its prompt is ready.
    fn set_handover(&self, id: &str, label: Option<String>, cx: &mut App) {
        if let Some(view) = self.live_view(id) {
            view.update(cx, |v, cx| {
                v.handover = label;
                cx.notify();
            });
        }
    }
}
