//! Hand-off: a running Claude session moves to the active account. Claude Code can't change the account of a running
//! process, so the session compacts itself (the mod runs `/compact`), then its process is replaced by a new one that
//! resumes the same conversation under the active account. The Claudiu session is the same record and keeps its shell
//! and cards; the old process ends. The record remembers its new account, so it starts there from then on. The left sidebar's warning only switches the account; the right sidebar offers
//! the hand-off for a session that runs under another one.

use super::*;
use crate::claude::usage::Snapshot;
use crate::terminal::view::Handover;
use std::time::Instant;

/// Used share of a limit at which the left sidebar shows its notice.
pub const HIGH: f32 = 0.95;
/// How long a compaction may take before the hand-off is given up.
const TIMEOUT_SECS: i64 = 180;

/// A session waiting to be compacted before it is resumed under the active account.
pub struct Handoff {
    pub asked: i64,
    /// Set once the mod reports a compaction after `asked`.
    pub compacted: bool,
    /// The skeleton counts from here, across the restart.
    pub since: Instant,
}

/// The left sidebar notice: the active account is at `HIGH` of a limit. It only reports; the user picks any account.
impl Workspace {
    /// Derived on every render, so the notice stays until the use drops or the account changes.
    pub fn switch_offer(&self) -> Option<String> {
        let t = crate::store::now();
        let active = self.store.active_account()?;
        let peak = active.usage.peak(t);
        if peak < HIGH {
            return None;
        }
        // The reset shown belongs to the limit whose percentage is shown; none if that limit has no known reset.
        let reset = [&active.usage.five_hour, &active.usage.seven_day]
            .into_iter()
            .flatten()
            .filter(|l| l.resets_at.is_none_or(|r| r > t))
            .max_by(|a, b| a.utilization.unwrap_or(0.0).total_cmp(&b.utilization.unwrap_or(0.0)))
            .and_then(|l| l.resets_at);
        let mut text = format!("{} is at {:.0}% of a usage limit.", active.name(), peak * 100.0);
        if let Some(r) = reset {
            text.push_str(&format!(" It resets in {}.", crate::panels::widgets::countdown(r - t)));
        }
        Some(text)
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
        let since = Instant::now();
        self.handoffs.insert(id.into(), Handoff { asked: crate::store::now(), compacted: false, since });
        self.set_handover(id, Some(Handover { to: to_name, since }), cx);
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
        let due: Vec<(Id, bool, Instant)> = self
            .handoffs
            .iter()
            .filter(|(_, h)| h.compacted || t - h.asked > TIMEOUT_SECS)
            .map(|(id, h)| (id.clone(), h.compacted, h.since))
            .collect();
        for (id, compacted, since) in due {
            self.handoffs.remove(&id);
            if !compacted {
                self.set_handover(&id, None, cx);
                self.toast("The hand-off was cancelled: the session did not compact.", true, cx);
            } else if self.is_running(&id, cx) {
                self.restart_handed_off(&id, since, window, cx);
            } else {
                self.set_handover(&id, None, cx);
            }
        }
    }

    /// The old process ends and a new one resumes the same conversation. The shell and cards belong to the session,
    /// so they stay.
    fn restart_handed_off(&mut self, id: &str, since: Instant, window: &mut Window, cx: &mut Context<Self>) {
        let to = self.store.active_account().map(|a| a.id.clone());
        if let Some(meta) = self.store.session_mut(id).and_then(|r| r.claude.as_mut()) {
            meta.account = to;
        }
        self.restart_session(id, window, cx);
        let to_name = self.store.active_account().map_or_else(|| "the active account".into(), |a| a.name());
        self.set_handover(id, Some(Handover { to: to_name, since }), cx);
    }

    /// The skeleton says "Handing over to …" while this is set; the new process clears it once its prompt is ready.
    fn set_handover(&self, id: &str, handover: Option<Handover>, cx: &mut App) {
        if let Some(view) = self.live_view(id) {
            view.update(cx, |v, cx| {
                v.handover = handover;
                cx.notify();
            });
        }
    }
}
