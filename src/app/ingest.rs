//! Taking in what other processes left in the data folder: the per-process mod's usage snapshots (limits, context,
//! settings), the tab titles Claude chose, and the links and GitHub items sessions asked for. Reading the files is
//! off the UI thread; applying them is a handful of small, separately testable steps.

use super::*;
use crate::store::RateLimit;
use crate::usage::Snapshot;

impl Workspace {
    /// Costs no tokens; the files are tiny.
    pub(super) fn ingest_usage(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (snaps, titles, items) = cx
                .background_executor()
                .spawn(async move { (usage::read_snapshots(&usage::snapshot_dir()), mcp::read_titles(), mcp::read_attached()) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                let mut changed = false;
                changed |= titles.into_iter().fold(false, |c, (stem, title, stamp)| ws.apply_title(&stem, title, stamp) | c);
                changed |= snaps.iter().fold(false, |c, snap| ws.apply_snapshot(snap) | c);
                changed |= items.into_iter().fold(false, |c, (stem, item)| ws.apply_attachment(&stem, item) | c);
                if changed {
                    ws.store.mark_dirty();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A title Claude chose for its session through the MCP tool.
    fn apply_title(&mut self, stem: &str, title: String, stamp: Option<SystemTime>) -> bool {
        let Some(rec) = self.store.data.sessions.iter_mut().find(|s| s.claude_id().is_some_and(|id| datadir::safe_file_stem(id) == stem)) else { return false };
        if let Some(l) = self.live.iter_mut().find(|l| l.id == rec.id)
            && l.title_stamp != stamp
        {
            // The first read only records the stamp: a resumed session already got `--name`.
            if l.title_stamp.is_some() {
                l.claude_name = None;
            }
            l.title_stamp = stamp;
        }
        if rec.auto_title.as_deref() == Some(title.as_str()) {
            return false;
        }
        rec.auto_title = Some(title);
        true
    }

    /// One mod report: the account's limits, and the session's own figures and settings.
    fn apply_snapshot(&mut self, snap: &Snapshot) -> bool {
        let account = snap.account.as_deref().unwrap_or(MAIN_ACCOUNT);
        let mut changed = self.merge_account(account, snap.five_hour.clone(), snap.seven_day.clone());
        if let Some(meta) = self.store.data.sessions.iter_mut().find(|s| s.claude_id() == Some(snap.session_id.as_str())).and_then(|s| s.claude.as_mut()) {
            changed |= apply_to_meta(meta, snap);
        }
        changed
    }

    /// Keep the newest reading per window. Returns whether anything changed (the same snapshot is re-read
    /// every tick, so an unconditional save would rewrite state.json every few seconds).
    pub(super) fn merge_account(&mut self, account: &str, five: Option<RateLimit>, seven: Option<RateLimit>) -> bool {
        // A removed account's leftover snapshots are ignored.
        let Some(acct) = self.store.data.accounts.iter_mut().find(|a| a.id == account).map(|a| &mut a.usage) else { return false };
        let mut changed = false;
        for (slot, new) in [(&mut acct.five_hour, five), (&mut acct.seven_day, seven)] {
            if let Some(new) = new
                && slot.as_ref().is_none_or(|cur| new.seen_at >= cur.seen_at && *cur != new)
            {
                *slot = Some(new);
                changed = true;
            }
        }
        if changed {
            self.store.mark_dirty();
        }
        changed
    }
}

/// Copy a snapshot's figures onto the session's record; true when anything differed.
fn apply_to_meta(meta: &mut ClaudeMeta, snap: &Snapshot) -> bool {
    let mut changed = false;
    if snap.context_fraction.is_some() && (meta.context_fraction != snap.context_fraction || meta.context_window != snap.context_window) {
        meta.context_fraction = snap.context_fraction;
        meta.context_window = snap.context_window;
        if snap.context_tokens.is_some() {
            meta.context_tokens = snap.context_tokens;
        }
        changed = true;
    }
    if snap.model.is_some() && meta.model != snap.model {
        meta.model = snap.model.clone();
        changed = true;
    }
    // Advisor `None` is real (it is off); auto-compact is only known once the mod has seen it.
    if meta.advisor != snap.advisor {
        meta.advisor = snap.advisor.clone();
        changed = true;
    }
    if snap.auto_compact.is_some() && meta.auto_compact != snap.auto_compact {
        meta.auto_compact = snap.auto_compact;
        changed = true;
    }
    if meta.compact_at != snap.compact_at {
        meta.compact_at = snap.compact_at;
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(json: &str) -> Snapshot {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_snapshot_updates_the_record_once_and_a_missing_figure_keeps_the_old_one() {
        let mut meta = ClaudeMeta::default();
        let first = snap(r#"{"session_id":"s","five_hour":null,"seven_day":null,"context_fraction":0.5,"context_tokens":500,"context_window":1000,"model":"opus","captured_at":1,"auto_compact":true,"advisor":"sonnet"}"#);
        assert!(apply_to_meta(&mut meta, &first));
        assert!(!apply_to_meta(&mut meta, &first), "the same report again changes nothing");
        assert_eq!((meta.context_tokens, meta.auto_compact, meta.advisor.as_deref()), (Some(500), Some(true), Some("sonnet")));
        let later = snap(r#"{"session_id":"s","five_hour":null,"seven_day":null,"context_fraction":null,"context_tokens":null,"context_window":null,"model":null,"captured_at":2}"#);
        assert!(apply_to_meta(&mut meta, &later), "advisor off is a real change");
        assert_eq!((meta.context_tokens, meta.auto_compact, meta.advisor.clone()), (Some(500), Some(true), None));
    }
}
