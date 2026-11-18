//! Codex's 5-hour and 7-day limits, read from the session logs Codex writes under `$CODEX_HOME/sessions` (default
//! `~/.codex`). Each turn Codex logs a `token_count` event carrying its current `rate_limits`; the newest one is the
//! reading. Read-only, no network, no tokens. The log format is Codex's own and undocumented, so anything that doesn't
//! parse is skipped and the sidebar shows no Codex row.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::store::{AccountUsage, RateLimit};

/// Codex files a log under the day its session started, so a session still running from yesterday writes into
/// yesterday's folder. Looking at the last few days finds it.
const DAYS_TO_LOOK_AT: usize = 3;
/// Logs tried, newest first, before giving up: a session that hasn't finished a turn yet has no reading. Kept small
/// because every try reads up to 896 KiB, every few seconds, and a sign-in by API key never logs a reading.
const LOGS_TO_TRY: usize = 3;

pub fn codex_home() -> Option<PathBuf> {
    match std::env::var_os("CODEX_HOME") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => crate::platform::home_dir().map(|h| h.join(".codex")),
    }
}

/// The newest limits Codex logged, or `None` when there are none to read.
pub fn read(codex_home: &Path) -> Option<AccountUsage> {
    newest_logs(&codex_home.join("sessions")).iter().find_map(|log| read_log(log))
}

/// Rollout logs in the last few `YYYY/MM/DD` folders, most recently written first.
fn newest_logs(sessions: &Path) -> Vec<PathBuf> {
    let days: Vec<PathBuf> = subdirs_newest_first(sessions)
        .into_iter()
        .flat_map(|year| subdirs_newest_first(&year))
        .flat_map(|month| subdirs_newest_first(&month))
        .take(DAYS_TO_LOOK_AT)
        .collect();
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = days
        .iter()
        .filter_map(|day| std::fs::read_dir(day).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    logs.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    logs.into_iter().take(LOGS_TO_TRY).map(|(_, path)| path).collect()
}

/// Folder names are zero-padded numbers, so sorting by name sorts by date. Walking stays lazy: only the newest years
/// and months are opened.
fn subdirs_newest_first(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort_by(|a, b| b.cmp(a));
    dirs
}

fn read_log(path: &Path) -> Option<AccountUsage> {
    let (head, tail, whole, _, _) = super::read_chunks(path).ok()?;
    let text = if whole { head } else { tail };
    text.lines().rev().filter(|line| line.contains("\"rate_limits\"")).find_map(parse_line)
}

fn parse_line(line: &str) -> Option<AccountUsage> {
    let v: Value = serde_json::from_str(line).ok()?;
    let payload = v.get("payload")?;
    if payload.get("type")?.as_str()? != "token_count" {
        return None;
    }
    let limits = payload.get("rate_limits")?;
    // One log line carries one bucket; only the `codex` bucket is the account's plan limit.
    if limits.get("limit_id").and_then(Value::as_str).is_some_and(|id| id != "codex") {
        return None;
    }
    let seen_at = super::parse_rfc3339(v.get("timestamp")?.as_str()?)?;
    let mut usage = AccountUsage::default();
    for window in [limits.get("primary"), limits.get("secondary")].into_iter().flatten() {
        let Some(minutes) = window.get("window_minutes").and_then(Value::as_i64) else { continue };
        let limit = RateLimit {
            utilization: window.get("used_percent").and_then(Value::as_f64).map(|p| (p / 100.0) as f32),
            resets_at: window.get("resets_at").and_then(Value::as_i64),
            seen_at,
        };
        match minutes {
            300 => usage.five_hour = Some(limit),
            10080 => usage.seven_day = Some(limit),
            _ => {}
        }
    }
    (usage.five_hour.is_some() || usage.seven_day.is_some()).then_some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shape Codex writes, trimmed of the token counts.
    const READING: &str = r#"{"timestamp":"2026-11-18T07:54:11.728Z","type":"event_msg","payload":{"type":"token_count","info":{},"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":24.0,"window_minutes":300,"resets_at":1794999616},"secondary":{"used_percent":19.0,"window_minutes":10080,"resets_at":1795510335},"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"plan_type":"plus"}}}"#;
    const SEEN_AT: i64 = 1_794_988_451;

    fn write_log(home: &Path, day: &str, name: &str, lines: &[&str]) -> PathBuf {
        let dir = home.join("sessions").join(day);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn reads_both_windows_from_a_token_count_event() {
        let home = tempfile::tempdir().unwrap();
        write_log(home.path(), "2026/11/18", "rollout-a.jsonl", &[r#"{"type":"session_meta","payload":{}}"#, READING]);
        let usage = read(home.path()).unwrap();
        assert_eq!(usage.five_hour, Some(RateLimit { utilization: Some(0.24), resets_at: Some(1794999616), seen_at: SEEN_AT }));
        assert_eq!(usage.seven_day, Some(RateLimit { utilization: Some(0.19), resets_at: Some(1795510335), seen_at: SEEN_AT }));
    }

    #[test]
    fn a_later_event_without_limits_does_not_hide_the_earlier_reading() {
        let home = tempfile::tempdir().unwrap();
        let without = r#"{"timestamp":"2026-11-18T08:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{},"rate_limits":null}}"#;
        write_log(home.path(), "2026/11/18", "rollout-a.jsonl", &[READING, without, "not json"]);
        assert_eq!(read(home.path()).unwrap().five_hour.unwrap().seen_at, SEEN_AT);
    }

    #[test]
    fn nothing_readable_gives_none() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(read(home.path()), None, "no sessions folder");
        write_log(home.path(), "2026/11/18", "rollout-a.jsonl", &["garbage \"rate_limits\"", r#"{"payload":{"type":"token_count","rate_limits":{}}}"#]);
        assert_eq!(read(home.path()), None);
    }

    #[test]
    fn unknown_windows_and_other_buckets_are_ignored() {
        let home = tempfile::tempdir().unwrap();
        let odd_window = READING.replace("\"window_minutes\":300", "\"window_minutes\":60");
        let other_bucket = READING.replace("\"limit_id\":\"codex\"", "\"limit_id\":\"codex_other\"");
        write_log(home.path(), "2026/11/18", "rollout-a.jsonl", &[&odd_window, &other_bucket]);
        let usage = read(home.path()).unwrap();
        assert_eq!(usage.five_hour, None);
        assert!(usage.seven_day.is_some());
    }

    #[test]
    fn a_session_started_on_an_earlier_day_still_counts() {
        let home = tempfile::tempdir().unwrap();
        let older = write_log(home.path(), "2026/11/17", "rollout-old.jsonl", &[READING]);
        let newer = write_log(home.path(), "2026/11/18", "rollout-new.jsonl", &[r#"{"type":"session_meta","payload":{}}"#]);
        assert!(read(home.path()).is_some(), "today's log has no reading yet; yesterday's running session has one");

        // The most recently written log wins, whichever day folder it sits in.
        std::fs::write(&newer, READING).unwrap();
        std::fs::write(&older, READING.replace("24.0", "50.0")).unwrap();
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&newer).unwrap().set_modified(hour_ago).unwrap();
        assert_eq!(read(home.path()).unwrap().five_hour.unwrap().utilization, Some(0.5));
    }
}
