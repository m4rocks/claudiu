//! Usage limits shared by harnesses, saved as one reading per group of models, plus shared log/time helpers.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::store::UsageLimits;

/// One group of models that share limits, by the name the provider gives it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pool {
    pub name: String,
    pub usage: UsageLimits,
}

/// Merge `incoming` into `current` per window, never letting an older reading overwrite a newer one.
/// Returns `true` when any window changed.
pub fn merge_usage(current: &mut UsageLimits, incoming: &UsageLimits) -> bool {
    let mut changed = false;
    for (slot, next) in [(&mut current.five_hour, &incoming.five_hour), (&mut current.seven_day, &incoming.seven_day)] {
        if let Some(next) = next
            && slot.as_ref().is_none_or(|cur| next.seen_at >= cur.seen_at && cur != next)
        {
            *slot = Some(next.clone());
            changed = true;
        }
    }
    changed
}

/// Merge incoming pools with previously stored pools so a slower in-flight probe cannot clobber a newer live reading.
pub fn merge_pools(current: &[Pool], incoming: Vec<Pool>) -> Vec<Pool> {
    incoming
        .into_iter()
        .map(|mut pool| {
            if let Some(prev) = current.iter().find(|p| p.name == pool.name) {
                let mut merged = prev.usage.clone();
                merge_usage(&mut merged, &pool.usage);
                pool.usage = merged;
            }
            pool
        })
        .collect()
}

/// Whether any window in `usage` has crossed its `resets_at` boundary since it was observed.
pub fn has_expired_window(usage: &UsageLimits, now: i64) -> bool {
    [usage.five_hour.as_ref(), usage.seven_day.as_ref()]
        .into_iter()
        .flatten()
        .any(|w| w.resets_at.is_some_and(|reset| reset <= now && w.seen_at < reset))
}

const HEAD_BYTES: u64 = 128 * 1024;
const TAIL_BYTES: u64 = 768 * 1024;

/// Read the head and tail chunks of a potentially large log file.
pub(crate) fn read_chunks(path: &Path) -> std::io::Result<(String, String, bool, i64, i64)> {
    let mut f = File::open(path)?;
    let meta = f.metadata()?;
    let len = meta.len();
    let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
    let created = meta.created().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(modified, |d| d.as_secs() as i64);
    if len <= HEAD_BYTES + TAIL_BYTES {
        let mut buf = Vec::with_capacity(len as usize);
        f.read_to_end(&mut buf)?;
        let s = String::from_utf8_lossy(&buf).into_owned();
        return Ok((s, String::new(), true, created, modified));
    }
    let mut head = vec![0u8; HEAD_BYTES as usize];
    f.read_exact(&mut head)?;
    f.seek(SeekFrom::Start(len - TAIL_BYTES))?;
    let mut tail = vec![0u8; TAIL_BYTES as usize];
    f.read_exact(&mut tail)?;
    // Both chunks start/end mid-line: drop the partial leading line of the tail; head's last line fails to parse and is skipped.
    let tail = String::from_utf8_lossy(&tail).into_owned();
    let tail = tail.split_once('\n').map_or(String::new(), |(_, rest)| rest.to_string());
    Ok((String::from_utf8_lossy(&head).into_owned(), tail, false, created, modified))
}

/// Parse `2026-10-05T14:10:53.395Z` to unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok().map(|d| d.unix_timestamp())
}

/// Utilization threshold (90%) at which m4code nudges a working model about its remaining token runway.
pub const NUDGE_THRESHOLD: f32 = 0.90;

/// Drift tolerance when comparing `resets_at` timestamps for the same quota window.
/// Providers like Codex compute `resets_at` from remaining seconds, so consecutive readings
/// in the same 5-hour or 7-day window can jitter by ±1s (or a few seconds across sources).
const RESET_WINDOW_TOLERANCE_SECS: i64 = 300;

/// One-time runway warning emitted when a working model approaches its usage limit.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageNudge {
    pub window_label: &'static str,
    pub percent_used: u32,
    pub remaining_range: Option<(u64, u64)>,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
struct WindowSample {
    resets_at: Option<i64>,
    start_util: f32,
    start_tokens: u64,
    nudged: bool,
    exhausted_handoff_sent: bool,
}

/// Tracks per-window utilization and cumulative token burn so m4code can estimate remaining token runway
/// and nudge the model once per window before auto-handoff cuts in.
#[derive(Clone, Debug, Default)]
pub struct RunwayTracker {
    five_hour: Option<WindowSample>,
    seven_day: Option<WindowSample>,
}

impl RunwayTracker {
    /// Observe the latest limits for a session with `cumulative_tokens`. Returns a one-shot nudge when a window
    /// crosses `NUDGE_THRESHOLD` (90%–99%) while the model is working.
    pub fn observe(
        &mut self,
        usage: &UsageLimits,
        cumulative_tokens: u64,
        auto_handoff: bool,
        now: i64,
    ) -> Option<UsageNudge> {
        if let Some(limit) = usage.five_hour.as_ref()
            && let Some(nudge) = Self::observe_window(&mut self.five_hour, "5-hour", limit, cumulative_tokens, auto_handoff, now)
        {
            return Some(nudge);
        }
        if let Some(limit) = usage.seven_day.as_ref()
            && let Some(nudge) = Self::observe_window(&mut self.seven_day, "7-day", limit, cumulative_tokens, auto_handoff, now)
        {
            return Some(nudge);
        }
        None
    }

    /// Returns `true` once per exhausted window (`utilization >= 1.0`) so auto-handoff triggers at most once.
    pub fn take_exhausted_handoff(&mut self, usage: &UsageLimits, now: i64) -> bool {
        let mut trigger = false;
        for (slot, limit) in [(&mut self.five_hour, usage.five_hour.as_ref()), (&mut self.seven_day, usage.seven_day.as_ref())] {
            let Some(limit) = limit else { continue };
            if limit.resets_at.is_some_and(|r| r <= now) {
                continue;
            }
            if limit.utilization.is_some_and(|u| u >= 1.0) {
                let sample = Self::slot_for(slot, limit, 0);
                if !sample.exhausted_handoff_sent {
                    sample.exhausted_handoff_sent = true;
                    trigger = true;
                }
            }
        }
        trigger
    }

    fn slot_for<'a>(slot: &'a mut Option<WindowSample>, limit: &crate::store::RateLimit, tokens: u64) -> &'a mut WindowSample {
        let util = limit.utilization.unwrap_or(0.0);
        let reset_changed = slot.as_ref().is_some_and(|s| match (s.resets_at, limit.resets_at) {
            (Some(prev), Some(next)) => (next - prev).abs() >= RESET_WINDOW_TOLERANCE_SECS,
            _ => util + 0.10 < s.start_util,
        });
        if slot.is_none() || reset_changed {
            *slot = Some(WindowSample {
                resets_at: limit.resets_at,
                start_util: util,
                start_tokens: tokens,
                nudged: false,
                exhausted_handoff_sent: false,
            });
        } else if let Some(sample) = slot.as_mut()
            && sample.resets_at.is_none()
            && limit.resets_at.is_some()
        {
            sample.resets_at = limit.resets_at;
        }
        slot.as_mut().unwrap()
    }

    fn observe_window(
        slot: &mut Option<WindowSample>,
        label: &'static str,
        limit: &crate::store::RateLimit,
        cumulative_tokens: u64,
        auto_handoff: bool,
        now: i64,
    ) -> Option<UsageNudge> {
        if limit.resets_at.is_some_and(|r| r <= now) {
            *slot = None;
            return None;
        }
        let util = limit.utilization?;
        let sample = Self::slot_for(slot, limit, cumulative_tokens);
        if util < NUDGE_THRESHOLD || util >= 1.0 || sample.nudged {
            return None;
        }
        sample.nudged = true;
        let percent_used = (util * 100.0).round().clamp(90.0, 99.0) as u32;
        let remaining_range = estimate_remaining_range(util, sample.start_util, cumulative_tokens.saturating_sub(sample.start_tokens), cumulative_tokens);
        let rate_clause = match remaining_range {
            Some((lo, hi)) => format!("; at your current rate you will hit the limit in roughly {}–{} tokens", format_token_count(lo), format_token_count(hi)),
            None => String::new(),
        };
        let handoff_clause = if auto_handoff {
            " Auto-handoff will happen then, so wrap up your current step cleanly without leaving anything half-finished—you don't need to interrupt or switch models yourself."
        } else {
            " Wrap up your current step cleanly so nothing is left half-finished before the cutoff."
        };
        let message = format!(
            "[m4code usage notice] You are approaching your {label} usage limit ({percent_used}% used{rate_clause}).{handoff_clause}"
        );
        Some(UsageNudge { window_label: label, percent_used, remaining_range, message })
    }
}

fn estimate_remaining_range(util: f32, start_util: f32, delta_tokens: u64, total_tokens: u64) -> Option<(u64, u64)> {
    let remaining_util = (1.0 - util).max(0.005) as f64;
    let delta_util = (util - start_util) as f64;
    let est = if delta_util >= 0.01 && delta_tokens >= 1_000 {
        (remaining_util / delta_util) * delta_tokens as f64
    } else if total_tokens >= 10_000 && util > 0.05 {
        (remaining_util / util as f64) * total_tokens as f64
    } else {
        return None;
    };
    let lo = round_tokens((est * 0.8) as u64);
    let hi = round_tokens((est * 1.2) as u64).max(lo + 10_000);
    Some((lo, hi))
}

fn round_tokens(n: u64) -> u64 {
    if n >= 1_000_000 {
        ((n + 50_000) / 100_000) * 100_000
    } else if n >= 100_000 {
        ((n + 25_000) / 50_000) * 50_000
    } else if n >= 10_000 {
        ((n + 5_000) / 10_000) * 10_000
    } else {
        ((n + 500) / 1_000).max(1) * 1_000
    }
}

fn format_token_count(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if (m.fract() - 0.0).abs() < 0.05 { format!("{:.0}M", m) } else { format!("{:.1}M", m) }
    } else {
        format!("{}k", n / 1_000)
    }
}

/// Whether a provider error message (or short standalone provider error reply) indicates the usage/rate limit was hit.
pub fn looks_like_usage_limit_error(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 320 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("you've hit your usage limit")
        || lower.starts_with("you\u{2019}ve hit your usage limit")
        || lower.starts_with("usage limit reached")
        || lower.starts_with("rate limit exceeded")
        || lower.starts_with("rate_limit_error")
        || lower.contains("429 too many requests")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-10-05T14:10:53.395Z"), Some(1791209453));
        assert_eq!(parse_rfc3339("nope"), None);
    }

    #[test]
    fn merge_usage_keeps_newer_readings_and_detects_expired_windows() {
        use crate::store::RateLimit;
        let mut cur = UsageLimits {
            five_hour: Some(RateLimit { utilization: Some(0.4), resets_at: Some(500), seen_at: 200 }),
            seven_day: None,
        };
        let older = UsageLimits {
            five_hour: Some(RateLimit { utilization: Some(0.1), resets_at: Some(500), seen_at: 100 }),
            seven_day: Some(RateLimit { utilization: Some(0.2), resets_at: Some(900), seen_at: 100 }),
        };
        assert!(merge_usage(&mut cur, &older));
        assert_eq!(cur.five_hour.as_ref().unwrap().utilization, Some(0.4), "older probe must not overwrite newer live reading");
        assert_eq!(cur.seven_day.as_ref().unwrap().utilization, Some(0.2), "missing window is still filled");
        assert!(!has_expired_window(&cur, 499));
        assert!(has_expired_window(&cur, 500));
    }

    #[test]
    fn runway_tracker_estimates_remaining_tokens_and_nudges_once_per_window() {
        use crate::store::RateLimit;
        let mut tracker = RunwayTracker::default();
        let at = |util, reset| UsageLimits {
            five_hour: Some(RateLimit { utilization: Some(util), resets_at: Some(reset), seen_at: 100 }),
            seven_day: None,
        };
        // Baseline at 80% with 100k tokens burned so far: below threshold.
        assert!(tracker.observe(&at(0.80, 1000), 100_000, true, 100).is_none());
        // Jump to 92% after burning 360k more tokens (12% = 360k -> 8% left ≈ 240k -> 200k–300k).
        let nudge = tracker.observe(&at(0.92, 1000), 460_000, true, 200).expect("should nudge at 92%");
        assert_eq!(nudge.window_label, "5-hour");
        assert_eq!(nudge.percent_used, 92);
        assert_eq!(nudge.remaining_range, Some((200_000, 300_000)));
        assert!(nudge.message.contains("200k–300k tokens"), "{}", nudge.message);
        assert!(nudge.message.contains("Auto-handoff will happen"), "{}", nudge.message);
        // Same window does not nudge again.
        assert!(tracker.observe(&at(0.95, 1000), 550_000, true, 250).is_none());
        // Exhausted triggers auto-handoff once.
        assert!(tracker.take_exhausted_handoff(&at(1.0, 1000), 300));
        assert!(!tracker.take_exhausted_handoff(&at(1.0, 1000), 310));
        // New reset window re-arms the tracker.
        assert!(tracker.observe(&at(0.91, 2000), 600_000, false, 1100).is_some());
    }

    #[test]
    fn runway_tracker_ignores_resets_at_jitter_within_same_window() {
        use crate::store::RateLimit;
        let mut tracker = RunwayTracker::default();
        let at = |util, reset| UsageLimits {
            five_hour: Some(RateLimit { utilization: Some(util), resets_at: reset, seen_at: 100 }),
            seven_day: None,
        };
        assert!(tracker.observe(&at(0.89, Some(1_795_743_798)), 100_000, true, 100).is_none());
        assert!(tracker.observe(&at(0.90, Some(1_795_743_797)), 120_000, true, 110).is_some());
        // ±1s jitter or missing resets_at in the same window must not re-nudge.
        assert!(tracker.observe(&at(0.90, Some(1_795_743_798)), 130_000, true, 115).is_none());
        assert!(tracker.observe(&at(0.91, Some(1_795_743_797)), 140_000, true, 120).is_none());
        assert!(tracker.observe(&at(0.92, None), 150_000, true, 125).is_none());
        assert!(tracker.observe(&at(0.92, Some(1_795_743_798)), 160_000, true, 130).is_none());
        // Exhausted handoff also fires once despite resets_at jitter.
        assert!(tracker.take_exhausted_handoff(&at(1.0, Some(1_795_743_797)), 200));
        assert!(!tracker.take_exhausted_handoff(&at(1.0, Some(1_795_743_798)), 205));
    }

    #[test]
    fn detects_common_usage_limit_error_messages() {
        assert!(looks_like_usage_limit_error("You’ve hit your usage limit. Upgrade to Pro."));
        assert!(looks_like_usage_limit_error("You've hit your usage limit."));
        assert!(looks_like_usage_limit_error("rate_limit_error: 429"));
        assert!(!looks_like_usage_limit_error("Build succeeded."));
        assert!(!looks_like_usage_limit_error("To handle HTTP errors, retry when the API returns `rate limit exceeded` with backoff."));
    }
}
