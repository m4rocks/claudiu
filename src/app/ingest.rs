//! Taking in what other processes left in the data folder: the per-process mod's usage snapshots (limits, context,
//! settings), and the links and GitHub items sessions asked for. Reading the files is
//! off the UI thread; applying them is a handful of small, separately testable steps.

use super::*;
use crate::harness::claude::usage::Snapshot;
use crate::harness::limits::Pool;
use crate::harness::subagents::Agent;
use crate::store::RateLimit;

impl Workspace {
    /// Costs no tokens. The mod's files are tiny, and Codex's logs are read again only after Codex writes to them.
    pub(super) fn ingest_usage(&mut self, cx: &mut Context<Self>) {
        self.ingest_subagent_limits(cx);
        self.in_background(
            || (usage::read_snapshots(&usage::snapshot_dir()), mcp::read_attached()),
            |ws, (snaps, items), cx| {
                let mut changed = snaps.iter().fold(false, |c, snap| ws.apply_snapshot(snap, cx) | c);
                changed |= items.into_iter().fold(false, |c, (stem, item)| ws.apply_attachment(&stem, item) | c);
                if changed {
                    ws.store.mark_dirty();
                }
            },
            cx,
        );
    }

    /// Each enabled harness with a limits adapter refreshes on its own cadence. Failed reads keep the saved reading.
    fn ingest_subagent_limits(&mut self, cx: &mut Context<Self>) {
        for agent in Agent::ALL {
            let provider = agent.provider();
            let Some(limits) = &provider.limits else { continue };
            let harness = match agent {
                Agent::Codex => Harness::Codex,
                Agent::OpenCode => Harness::OpenCode,
                Agent::Antigravity => Harness::Antigravity,
            };
            let active = self.store.data.settings.harness_enabled(harness) || self.store.data.settings.custom_subagents.get(agent).on;
            if !active || self.limits_asked.get(agent).is_some_and(|at| at.elapsed() < limits.every) {
                continue;
            }
            // Set before asking, so a slow answer isn't asked for twice.
            *self.limits_asked.get_mut(agent) = Some(std::time::Instant::now());
            self.in_background(limits.read, move |ws, read, _| ws.apply_subagent_limits(provider.key, read), cx);
        }
    }

    pub(super) fn apply_subagent_limits(&mut self, key: &str, read: Result<Vec<Pool>, String>) {
        if let Ok(pools) = read
            && self.store.data.subagent_limits.get(key) != Some(&pools)
        {
            self.store.data.subagent_limits.insert(key.into(), pools);
            self.store.mark_dirty();
        }
    }

    /// One mod report: Claude's limits, and the session's own figures and settings.
    fn apply_snapshot(&mut self, snap: &Snapshot, cx: &mut Context<Self>) -> bool {
        let mut changed = self.merge_claude_usage(snap.five_hour.clone(), snap.seven_day.clone());
        let id = self.store.data.sessions.iter().find(|s| {
            s.agent.as_ref().is_some_and(|meta| meta.harness == Harness::Claude && meta.session_id == snap.session_id)
        }).map(|s| s.id.clone());
        if let Some(id) = id {
            if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.agent.as_mut()) {
                changed |= apply_to_meta(meta, snap);
            }
            if let Some(native) = snap.auto_compact
                && let Some(view) = self.live_view(&id).and_then(|v| v.thread())
            {
                view.update(cx, |view, cx| {
                    if view.compaction.native_controlled == native {
                        view.compaction.native_controlled = !native;
                        cx.notify();
                    }
                });
            }
        }
        changed
    }

    /// Keep the newest reading per window. Returns whether anything changed (the same snapshot is re-read
    /// every tick, so an unconditional save would rewrite state.json every few seconds).
    pub(super) fn merge_claude_usage(&mut self, five: Option<RateLimit>, seven: Option<RateLimit>) -> bool {
        let usage = &mut self.store.data.claude_usage;
        let mut changed = false;
        for (slot, new) in [(&mut usage.five_hour, five), (&mut usage.seven_day, seven)] {
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
fn apply_to_meta(meta: &mut AgentMeta, snap: &Snapshot) -> bool {
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
    // The conversation keeps the advisor it declared. A null set means it is off (or an older mod did not report
    // the active value at all), in which case the saved setting is the best thing to show.
    let advisor_active = match &snap.advisor_active {
        Some(active) => Some(active.clone()),
        None if snap.advisor.is_none() => None,
        None => meta.advisor_active.clone().or_else(|| snap.advisor.clone()),
    };
    if meta.advisor_active != advisor_active {
        meta.advisor_active = advisor_active;
        changed = true;
    }
    if !meta.managed_compaction && snap.auto_compact.is_some() && meta.auto_compact != snap.auto_compact {
        meta.auto_compact = snap.auto_compact;
        changed = true;
    }
    // `None` is real too: Claude Code lists the row only while its feature flag is on.
    if meta.precompute_compact != snap.precompute_compact {
        meta.precompute_compact = snap.precompute_compact;
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
        let mut meta = AgentMeta::default();
        let first = snap(r#"{"session_id":"s","five_hour":null,"seven_day":null,"context_fraction":0.5,"context_tokens":500,"context_window":1000,"model":"opus","captured_at":1,"auto_compact":true,"advisor":"sonnet"}"#);
        assert!(apply_to_meta(&mut meta, &first));
        assert!(!apply_to_meta(&mut meta, &first), "the same report again changes nothing");
        assert_eq!((meta.context_tokens, meta.auto_compact, meta.advisor.as_deref()), (Some(500), Some(true), Some("sonnet")));
        assert_eq!(meta.advisor_active.as_deref(), Some("sonnet"), "no active report yet: the saved model is the one to show");
        let pending = snap(r#"{"session_id":"s","five_hour":null,"seven_day":null,"context_fraction":null,"context_tokens":null,"context_window":null,"model":null,"captured_at":2,"advisor":"opus","advisor_active":"sonnet"}"#);
        assert!(apply_to_meta(&mut meta, &pending));
        assert_eq!((meta.advisor.as_deref(), meta.advisor_active.as_deref()), (Some("opus"), Some("sonnet")), "the conversation keeps Sonnet until /clear or /compact");
        let later = snap(r#"{"session_id":"s","five_hour":null,"seven_day":null,"context_fraction":null,"context_tokens":null,"context_window":null,"model":null,"captured_at":3}"#);
        assert!(apply_to_meta(&mut meta, &later), "advisor off is a real change");
        assert_eq!((meta.context_tokens, meta.auto_compact, meta.advisor.clone()), (Some(500), Some(true), None));
    }

    #[test]
    fn background_compaction_is_offered_only_while_the_mod_reports_it() {
        let mut meta = AgentMeta::default();
        let offered = snap(r#"{"session_id":"s","five_hour":null,"seven_day":null,"context_fraction":null,"context_tokens":null,"context_window":null,"model":null,"captured_at":1,"precompute_compact":true}"#);
        assert!(apply_to_meta(&mut meta, &offered));
        assert_eq!(meta.precompute_compact, Some(true));
        let withdrawn = snap(r#"{"session_id":"s","five_hour":null,"seven_day":null,"context_fraction":null,"context_tokens":null,"context_window":null,"model":null,"captured_at":2}"#);
        assert!(apply_to_meta(&mut meta, &withdrawn), "Claude Code stopped listing the row: the switch goes away");
        assert_eq!(meta.precompute_compact, None);
    }
}
