//! Taking in what other processes left in the data folder: the reports agents write outside ACP (limits, context,
//! settings; Claude Code's mod writes them), and the links and GitHub items sessions asked for. Reading the files is
//! off the UI thread; applying them is a handful of small, separately testable steps.

use std::time::{Duration, Instant};

use super::*;
use crate::harness::capabilities::{Initiator, handoff};
use crate::harness::SessionReport;
use crate::harness::limits::{Pool, has_expired_window};
use crate::store::UsageLimits;

/// Retry delay after a failed usage-limits probe, matching t3code's 30s recovery backoff.
const FAILED_RETRY_DELAY: Duration = Duration::from_secs(30);
/// Minimum gap between automatic probes triggered by a window crossing its `resets_at` boundary.
const RESET_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Per-harness background probe state for usage limits.
#[derive(Clone, Debug, Default)]
pub struct LimitsState {
    /// True while a background probe is in flight.
    pub loading: bool,
    pub authentication_required: bool,
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
            self.ingest_live_usage(cx);
        }
        self.in_background(
            || {
                let reports: Vec<(Harness, SessionReport)> = Harness::registered()
                    .filter_map(|harness| Some((harness, harness.adapter().reports?)))
                    .flat_map(|(harness, read)| read().into_iter().map(move |report| (harness, report)))
                    .collect();
                (reports, mcp::read_attached())
            },
            |ws, (reports, items), cx| {
                let mut changed = reports.iter().fold(false, |c, (harness, report)| ws.apply_report(*harness, report, cx) | c);
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
        harness.adapter().usage.is_some() && self.stored_pools(harness).iter().any(|p| has_expired_window(&p.usage, now))
    }

    /// The stored limits for `harness`.
    fn stored_pools(&self, harness: Harness) -> Vec<Pool> {
        self.store.data.usage_pools(harness)
    }

    /// Merges `pools` into the stored limits for `harness`. Returns whether anything changed: a report is read again
    /// every tick, so an unconditional save would rewrite state.json every few seconds.
    fn store_pools(&mut self, harness: Harness, pools: Vec<Pool>) -> bool {
        let changed = self.store.data.merge_usage_pools(harness, pools);
        if changed {
            self.store.mark_dirty();
        }
        changed
    }

    /// Active zero-token probes, on each adapter's own cadence.
    fn ingest_limits(&mut self, cx: &mut Context<Self>) {
        let now_instant = Instant::now();
        let now_unix = crate::store::now();
        for harness in Harness::registered() {
            let Some(usage) = &harness.adapter().usage else { continue };
            if !self.limits_harness_active(harness) {
                continue;
            }
            let every = usage.every;
            let state = self.limits_state.get(harness);
            if state.loading || state.authentication_required {
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

    /// The adapters' cheap local readings, such as Codex's rollout logs, so mid-turn updates appear within seconds.
    fn ingest_live_usage(&mut self, cx: &mut Context<Self>) {
        for harness in Harness::registered() {
            let Some(crate::harness::LiveUsage { read, every }) = harness.adapter().usage.as_ref().and_then(|usage| usage.live) else { continue };
            if !self.limits_harness_active(harness) || self.live_usage_asked.get(harness).is_some_and(|at| at.elapsed() < every) {
                continue;
            }
            *self.live_usage_asked.get_mut(harness) = Some(Instant::now());
            self.in_background(
                read,
                move |ws, usage, cx| {
                    if let Some(usage) = usage {
                        ws.apply_live_limits(harness, usage, cx);
                    }
                },
                cx,
            );
        }
    }

    fn spawn_limits_probe(&mut self, harness: Harness, cx: &mut Context<Self>) {
        let Some(usage) = &harness.adapter().usage else { return };
        let read = usage.read;
        let state = self.limits_state.get_mut(harness);
        state.loading = true;
        state.queued = false;
        state.last_asked = Some(Instant::now());
        state.retry_after = None;
        cx.notify();
        let executable = self.executable(harness).map(std::path::Path::to_path_buf);
        self.in_background(
            move || read(executable.as_deref()),
            move |ws, read, cx| ws.finish_limits_probe(harness, read, cx),
            cx,
        );
    }

    fn finish_limits_probe(&mut self, harness: Harness, read: Result<Vec<Pool>, String>, cx: &mut Context<Self>) {
        let state = self.limits_state.get_mut(harness);
        state.loading = false;
        let run_again = state.queued;
        state.queued = false;
        match read {
            Ok(pools) => {
                state.error = None;
                state.authentication_required = false;
                state.retry_after = None;
                self.store_pools(harness, pools);
                self.notify_threads_usage(harness, cx);
            }
            Err(err) => {
                state.authentication_required = crate::harness::acp::looks_like_auth_error(&err);
                state.error = Some(if state.authentication_required { format!("Usage is stale. {}", harness.adapter().sign_in.session_hint) } else { err });
                state.retry_after = (!state.authentication_required).then(|| Instant::now() + FAILED_RETRY_DELAY);
            }
        }
        cx.notify();
        if run_again && !self.limits_state.get(harness).authentication_required && self.limits_harness_active(harness) {
            self.spawn_limits_probe(harness, cx);
        }
    }

    /// Merge live streamed limits (e.g. from `bridge/claude.mjs` or Codex rollout logs) immediately.
    pub(crate) fn apply_live_limits(&mut self, harness: Harness, usage: UsageLimits, cx: &mut Context<Self>) {
        if harness.adapter().usage.is_none() || !self.store_pools(harness, vec![Pool { name: harness.name().into(), usage }]) {
            return;
        }
        let state = self.limits_state.get_mut(harness);
        state.error = None;
        state.authentication_required = false;
        state.retry_after = None;
        self.notify_threads_usage(harness, cx);
        cx.notify();
    }

    /// The stored usage limits for `harness`, for an adapter that reports them. `_model_id` picks the pool for one model
    /// once an adapter splits its limits by model.
    pub(crate) fn usage_for_harness_model(&self, harness: Harness, _model_id: Option<&str>) -> Option<UsageLimits> {
        harness.adapter().usage.as_ref()?;
        self.stored_pools(harness).into_iter().next().map(|pool| pool.usage)
    }

    fn harness_probably_available(&self, harness: Harness) -> bool {
        if !harness.is_registered() {
            return false;
        }
        // An adapter whose executable Settings can point at is found before anything runs, so its answer is final.
        // The others are found in the background, so until then a cached model catalog or usage reading counts.
        if harness.adapter().custom_path || self.executable(harness).is_some() {
            return self.executable(harness).is_some();
        }
        let has_models = self.store.data.settings.aggregated_models().iter().any(|m| m.harness == harness && !m.id.is_empty());
        let has_limits = self.store.data.subagent_limits.contains_key(harness.key());
        has_models || has_limits
    }

    /// Pick the best available fallback model from another enabled harness that is not at its usage limit.
    /// Honors `settings.auto_handoff_model` first when configured, enabled, and below 95% utilization.
    pub(crate) fn best_fallback_model(&self, excluded: Harness) -> Option<crate::harness::CatalogModel> {
        let now = crate::store::now();
        let enabled = self.store.data.settings.enabled_harnesses_list();
        let all = self.store.data.settings.aggregated_models();
        let max_util = |h: Harness, model_id: Option<&str>| {
            self.usage_for_harness_model(h, model_id)
                .and_then(|u| {
                    [u.five_hour.as_ref(), u.seven_day.as_ref()]
                        .into_iter()
                        .flatten()
                        .filter(|l| l.resets_at.is_none_or(|r| r > now))
                        .filter_map(|l| l.utilization)
                        .reduce(f32::max)
                })
                .unwrap_or(0.0)
        };
        let may_take_over = |h: Harness| h != excluded && handoff(excluded, h, Initiator::Automatic).allowed();
        if let Some(pref) = &self.store.data.settings.auto_handoff_model
            && may_take_over(pref.harness)
            && enabled.contains(&pref.harness)
            && self.harness_probably_available(pref.harness)
            && max_util(pref.harness, (!pref.id.is_empty()).then_some(pref.id.as_str())) < 0.95
        {
            let name = all
                .iter()
                .find(|m| m.harness == pref.harness && m.id == pref.id)
                .map_or_else(|| pref.id.clone(), |m| m.name.clone());
            return Some(crate::harness::CatalogModel { harness: pref.harness, id: pref.id.clone(), name });
        }
        let mut candidates: Vec<(f32, crate::harness::CatalogModel)> = enabled
            .into_iter()
            .filter(|&h| may_take_over(h) && self.harness_probably_available(h))
            .filter_map(|h| {
                let model = all.iter().find(|m| m.harness == h).cloned().unwrap_or_else(|| {
                    crate::harness::CatalogModel { harness: h, id: String::new(), name: h.name().into() }
                });
                let util = max_util(h, (!model.id.is_empty()).then_some(model.id.as_str()));
                (util < 0.95).then_some((util, model))
            })
            .collect();
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        candidates.into_iter().next().map(|(_, model)| model)
    }

    /// Push fresh usage readings into every running thread on `harness` so runway nudges and auto-handoff fire live.
    pub(crate) fn notify_threads_usage(&mut self, harness: Harness, cx: &mut Context<Self>) {
        let notify_on_limit = self.store.data.settings.notify_model_on_limit;
        let auto_handoff = self.store.data.settings.auto_handoff_on_limit;
        let fallback = self.best_fallback_model(harness);
        let threads: Vec<_> = self
            .live
            .iter()
            .filter_map(|l| l.view.thread().cloned())
            .filter(|t| t.read(cx).harness == harness)
            .collect();
        for thread in threads {
            let model_id = thread.read(cx).thread.option("model").map(|o| o.current.clone());
            let Some(usage) = self.usage_for_harness_model(harness, model_id.as_deref()) else { continue };
            let fallback = fallback.clone();
            thread.update(cx, |t, cx| {
                t.fallback_model = fallback;
                t.observe_usage_limits(&usage, notify_on_limit, auto_handoff, cx);
            });
        }
    }

    /// Request a fresh usage-limits probe for `harness` (e.g. after a turn finishes).
    pub(crate) fn request_limits_refresh(&mut self, harness: Harness, cx: &mut Context<Self>) {
        if self.limits_state.get(harness).authentication_required { return; }
        if !harness.adapter().usage.as_ref().is_some_and(|usage| usage.after_turn) || !self.limits_harness_active(harness) {
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
        for harness in Harness::registered() {
            self.limits_state.get_mut(harness).authentication_required = false;
        }
        for harness in Harness::registered().filter(|harness| harness.adapter().usage.is_some()) {
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

    /// One report from `harness`: its limits, and the session's own figures and settings.
    fn apply_report(&mut self, harness: Harness, report: &SessionReport, cx: &mut Context<Self>) -> bool {
        let usage = UsageLimits { five_hour: report.five_hour.clone(), seven_day: report.seven_day.clone() };
        let mut changed = self.store_pools(harness, vec![Pool { name: harness.name().into(), usage }]);
        if changed {
            self.notify_threads_usage(harness, cx);
        }
        let id = self.store.data.sessions.iter().find(|s| {
            s.agent.as_ref().is_some_and(|meta| meta.harness == harness && meta.session_id == report.session_id)
        }).map(|s| s.id.clone());
        if let Some(id) = id {
            if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.agent.as_mut()) {
                changed |= apply_to_meta(meta, report);
            }
            if let Some(native) = report.auto_compact
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
}

/// Copy a report's figures onto the session's record; true when anything differed.
fn apply_to_meta(meta: &mut AgentMeta, snap: &SessionReport) -> bool {
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

    fn snap(json: &str) -> SessionReport {
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
