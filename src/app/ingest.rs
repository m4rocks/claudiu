//! Taking in what other processes left in the data folder: the per-process mod's usage snapshots (limits, context,
//! settings), and the links and GitHub items sessions asked for. Reading the files is
//! off the UI thread; applying them is a handful of small, separately testable steps.

use std::time::{Duration, Instant};

use super::*;
use crate::harness::claude::usage::Snapshot;
use crate::harness::limits::{Pool, has_expired_window, merge_pools, merge_usage};
use crate::store::{RateLimit, UsageLimits};

/// Retry delay after a failed usage-limits probe, matching t3code's 30s recovery backoff.
const FAILED_RETRY_DELAY: Duration = Duration::from_secs(30);
/// Minimum gap between automatic probes triggered by a window crossing its `resets_at` boundary.
const RESET_RETRY_DELAY: Duration = Duration::from_secs(30);
/// Fast local check cadence for Codex rollout logs (zero network / zero subprocesses).
const CODEX_LOG_POLL: Duration = Duration::from_secs(3);

/// Per-harness background probe state for usage limits.
#[derive(Clone, Debug, Default)]
pub struct LimitsState {
    /// True while a background probe is in flight.
    pub loading: bool,
    /// When the last probe started.
    pub last_asked: Option<Instant>,
    /// Error message from the most recent failed probe (cleared on success or live update).
    pub error: Option<String>,
    /// Earliest instant the next retry may run after a failure.
    pub retry_after: Option<Instant>,
    /// A refresh was requested while a probe was already in flight; run once more when it finishes.
    pub queued: bool,
}

impl Workspace {
    /// Costs no tokens. The mod's files are tiny, Codex's logs are read again only after Codex writes to them,
    /// and active probes run on their own 5-minute cadence (plus startup, reset crossing, turn end, and manual click).
    pub(super) fn ingest_usage(&mut self, cx: &mut Context<Self>) {
        if !cfg!(test) {
            self.ingest_limits(cx);
            self.ingest_codex_logs(cx);
        }
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

    /// Whether `harness` is enabled either as a main harness or as a custom subagent.
    fn limits_harness_active(&self, harness: Harness) -> bool {
        harness.is_registered() && (self.store.data.settings.harness_enabled(harness)
            || harness.agent().is_some_and(|a| self.store.data.settings.custom_subagents.get(a).on))
    }

    /// Whether any stored window for `harness` has crossed its `resets_at` boundary.
    fn harness_has_expired_window(&self, harness: Harness, now: i64) -> bool {
        match harness {
            Harness::Claude => has_expired_window(&self.store.data.claude_usage, now),
            Harness::Codex | Harness::Antigravity => {
                let Some(key) = harness.agent().map(|a| a.provider().key) else { return false };
                self.store
                    .data
                    .subagent_limits
                    .get(key)
                    .is_some_and(|pools| pools.iter().any(|p| has_expired_window(&p.usage, now)))
            }
            Harness::OpenCode => false,
        }
    }

    /// Active zero-token probes for Claude, Codex, and Antigravity.
    fn ingest_limits(&mut self, cx: &mut Context<Self>) {
        let now_instant = Instant::now();
        let now_unix = crate::store::now();
        for harness in [Harness::Claude, Harness::Codex, Harness::Antigravity] {
            if !self.limits_harness_active(harness) {
                continue;
            }
            let every = match harness {
                Harness::Claude => crate::harness::claude::limits::REFRESH_EVERY,
                Harness::Codex | Harness::Antigravity => {
                    let Some(limits) = harness.agent().and_then(|a| a.provider().limits.as_ref()) else { continue };
                    limits.every
                }
                Harness::OpenCode => continue,
            };
            let state = self.limits_state.get(harness);
            if state.loading {
                continue;
            }
            let expired = self.harness_has_expired_window(harness, now_unix);
            let due = state.queued
                || state.last_asked.is_none()
                || state.last_asked.is_some_and(|at| now_instant.saturating_duration_since(at) >= every)
                || (state.error.is_some() && state.retry_after.is_some_and(|at| now_instant >= at))
                || (expired && state.last_asked.is_some_and(|at| now_instant.saturating_duration_since(at) >= RESET_RETRY_DELAY));
            if due {
                self.spawn_limits_probe(harness, cx);
            }
        }
    }

    /// Fast zero-cost local watcher for Codex rollout logs so mid-turn updates appear within seconds.
    fn ingest_codex_logs(&mut self, cx: &mut Context<Self>) {
        if !self.limits_harness_active(Harness::Codex) {
            return;
        }
        if self.codex_logs_asked.is_some_and(|at| at.elapsed() < CODEX_LOG_POLL) {
            return;
        }
        self.codex_logs_asked = Some(Instant::now());
        self.in_background(
            || crate::harness::codex::limits::codex_home().and_then(|home| crate::harness::codex::limits::read(&home)),
            |ws, usage, cx| {
                if let Some(usage) = usage {
                    ws.apply_live_limits(Harness::Codex, usage, cx);
                }
            },
            cx,
        );
    }

    fn spawn_limits_probe(&mut self, harness: Harness, cx: &mut Context<Self>) {
        let state = self.limits_state.get_mut(harness);
        state.loading = true;
        state.queued = false;
        state.last_asked = Some(Instant::now());
        state.retry_after = None;
        cx.notify();

        match harness {
            Harness::Claude => {
                let Some(claude_exe) = self.claude_exe.clone() else {
                    self.finish_limits_probe(Harness::Claude, Err("Could not find the `claude` CLI on PATH.".into()), cx);
                    return;
                };
                self.in_background(
                    move || {
                        let usage = crate::harness::claude::limits::read(&claude_exe)?;
                        Ok(vec![Pool { name: "Claude".into(), usage }])
                    },
                    |ws, read, cx| ws.finish_limits_probe(Harness::Claude, read, cx),
                    cx,
                );
            }
            Harness::Codex | Harness::Antigravity => {
                let Some(agent) = harness.agent() else { return };
                let Some(limits) = &agent.provider().limits else { return };
                self.in_background(
                    limits.read,
                    move |ws, read, cx| ws.finish_limits_probe(harness, read, cx),
                    cx,
                );
            }
            Harness::OpenCode => {}
        }
    }

    fn finish_limits_probe(&mut self, harness: Harness, read: Result<Vec<Pool>, String>, cx: &mut Context<Self>) {
        let state = self.limits_state.get_mut(harness);
        state.loading = false;
        let run_again = state.queued;
        state.queued = false;
        match read {
            Ok(pools) => {
                state.error = None;
                state.retry_after = None;
                match harness {
                    Harness::Claude => {
                        if let Some(pool) = pools.into_iter().next() {
                            self.merge_claude_usage(pool.usage.five_hour, pool.usage.seven_day);
                        }
                    }
                    Harness::Codex | Harness::Antigravity => {
                        if let Some(key) = harness.agent().map(|a| a.provider().key) {
                            self.apply_subagent_limits(key, Ok(pools));
                        }
                    }
                    Harness::OpenCode => {}
                }
                self.notify_threads_usage(harness, cx);
            }
            Err(err) => {
                state.error = Some(err);
                state.retry_after = Some(Instant::now() + FAILED_RETRY_DELAY);
            }
        }
        cx.notify();
        if run_again && self.limits_harness_active(harness) {
            self.spawn_limits_probe(harness, cx);
        }
    }

    /// Merge live streamed limits (e.g. from `bridge/claude.mjs` or Codex rollout logs) immediately.
    pub(crate) fn apply_live_limits(&mut self, harness: Harness, usage: UsageLimits, cx: &mut Context<Self>) {
        let changed = match harness {
            Harness::Claude => self.merge_claude_usage(usage.five_hour, usage.seven_day),
            Harness::Codex => {
                let current = self.store.data.subagent_limits.get("codex").cloned().unwrap_or_default();
                let merged = merge_pools(&current, vec![Pool { name: "Codex".into(), usage }]);
                if merged != current {
                    self.store.data.subagent_limits.insert("codex".into(), merged);
                    self.store.mark_dirty();
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if changed {
            let state = self.limits_state.get_mut(harness);
            state.error = None;
            state.retry_after = None;
            self.notify_threads_usage(harness, cx);
            cx.notify();
        }
    }

    /// The stored usage limits for `harness`, if any.
    pub(crate) fn usage_for_harness(&self, harness: Harness) -> Option<UsageLimits> {
        match harness {
            Harness::Claude => Some(self.store.data.claude_usage.clone()),
            Harness::Codex | Harness::Antigravity => {
                let key = harness.agent()?.provider().key;
                self.store.data.subagent_limits.get(key)?.first().map(|p| p.usage.clone())
            }
            Harness::OpenCode => None,
        }
    }

    /// Pick the best available fallback model from another enabled harness that is not at its usage limit.
    pub(crate) fn best_fallback_model(&self, excluded: Harness) -> Option<crate::harness::CatalogModel> {
        let now = crate::store::now();
        let enabled = self.store.data.settings.enabled_harnesses_list();
        let mut candidates: Vec<(f32, Harness)> = enabled
            .into_iter()
            .filter(|&h| h != excluded && h.is_registered())
            .filter_map(|h| {
                let util = self
                    .usage_for_harness(h)
                    .and_then(|u| {
                        [u.five_hour.as_ref(), u.seven_day.as_ref()]
                            .into_iter()
                            .flatten()
                            .filter(|l| l.resets_at.is_none_or(|r| r > now))
                            .filter_map(|l| l.utilization)
                            .reduce(f32::max)
                    })
                    .unwrap_or(0.0);
                (util < 0.95).then_some((util, h))
            })
            .collect();
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (_, harness) = candidates.into_iter().next()?;
        let all = self.store.data.settings.aggregated_models();
        all.into_iter().find(|m| m.harness == harness).or_else(|| {
            Some(crate::harness::CatalogModel { harness, id: String::new(), name: harness.name().into() })
        })
    }

    /// Push fresh usage readings into every running thread on `harness` so runway nudges and auto-handoff fire live.
    pub(crate) fn notify_threads_usage(&mut self, harness: Harness, cx: &mut Context<Self>) {
        let Some(usage) = self.usage_for_harness(harness) else { return };
        let auto_handoff = self.store.data.settings.auto_handoff_on_limit;
        let fallback = self.best_fallback_model(harness);
        let threads: Vec<_> = self
            .live
            .iter()
            .filter_map(|l| l.view.thread().cloned())
            .filter(|t| t.read(cx).harness == harness)
            .collect();
        for thread in threads {
            let fallback = fallback.clone();
            thread.update(cx, |t, cx| {
                t.fallback_model = fallback;
                t.observe_usage_limits(&usage, auto_handoff, cx);
            });
        }
    }

    /// Request a fresh usage-limits probe for `harness` (e.g. after a turn finishes).
    pub(crate) fn request_limits_refresh(&mut self, harness: Harness, cx: &mut Context<Self>) {
        if !matches!(harness, Harness::Claude | Harness::Codex | Harness::Antigravity) || !self.limits_harness_active(harness) {
            return;
        }
        if self.limits_state.get(harness).loading {
            self.limits_state.get_mut(harness).queued = true;
            return;
        }
        self.spawn_limits_probe(harness, cx);
    }

    /// Manual refresh triggered from the sidebar usage footer.
    pub(crate) fn refresh_all_limits(&mut self, cx: &mut Context<Self>) {
        for harness in [Harness::Claude, Harness::Codex, Harness::Antigravity] {
            if !self.limits_harness_active(harness) {
                continue;
            }
            if self.limits_state.get(harness).loading {
                self.limits_state.get_mut(harness).queued = true;
            } else {
                self.spawn_limits_probe(harness, cx);
            }
        }
    }

    pub(super) fn apply_subagent_limits(&mut self, key: &str, read: Result<Vec<Pool>, String>) {
        if let Ok(pools) = read {
            let current = self.store.data.subagent_limits.get(key).cloned().unwrap_or_default();
            let merged = merge_pools(&current, pools);
            if merged != current {
                self.store.data.subagent_limits.insert(key.into(), merged);
                self.store.mark_dirty();
            }
        }
    }

    /// One mod report: Claude's limits, and the session's own figures and settings.
    fn apply_snapshot(&mut self, snap: &Snapshot, cx: &mut Context<Self>) -> bool {
        let mut changed = self.merge_claude_usage(snap.five_hour.clone(), snap.seven_day.clone());
        if changed {
            self.notify_threads_usage(Harness::Claude, cx);
        }
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
        let incoming = UsageLimits { five_hour: five, seven_day: seven };
        let changed = merge_usage(&mut self.store.data.claude_usage, &incoming);
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
