//! Active zero-token usage limits probe for Claude Code.
//!
//! Runs the embedded `bridge/claude.mjs --probe-usage` script, which starts a Claude Agent SDK
//! query with a never-yielding prompt (spending zero tokens), awaits initialization, calls the
//! CLI's `get_usage` control RPC, and exits.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::store::{RateLimit, UsageLimits};

/// Periodic background refresh interval for Claude usage limits.
pub const REFRESH_EVERY: Duration = Duration::from_secs(300);

/// Safety timeout for the Node + Claude CLI probe process.
const TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_MAX: u64 = 1 << 20;

/// Probe Claude Code's current 5-hour and 7-day usage limits without sending a prompt.
pub fn read(claude_exe: &Path) -> Result<UsageLimits, String> {
    let (node, mut args) = super::server(&crate::datadir::data_dir())?;
    args.push("--probe-usage".into());

    let mut cmd = Command::new(&node);
    cmd.args(&args)
        .current_dir(crate::datadir::data_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in super::probe_env(claude_exe) {
        cmd.env(k, v);
    }
    crate::platform::no_window(&mut cmd);
    crate::platform::with_login_path(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| format!("Claude usage probe could not start ({e})"))?;
    let stdout = child.stdout.take().ok_or("Claude usage probe had no stdout")?;
    let stderr = child.stderr.take().ok_or("Claude usage probe had no stderr")?;
    let out_reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.take(OUTPUT_MAX).read_to_string(&mut out);
        out
    });
    let err_reader = std::thread::spawn(move || {
        let mut err = String::new();
        let _ = stderr.take(OUTPUT_MAX).read_to_string(&mut err);
        err
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Claude usage probe timed out after {}s", TIMEOUT.as_secs()));
            }
        }
    };

    let out = out_reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    if !status.success() {
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        return Err(format!("Claude usage probe failed: {last}"));
    }
    parse(&out, time::OffsetDateTime::now_utc().unix_timestamp())
        .ok_or_else(|| "Claude did not return usage limits".into())
}

/// Parse the JSON printed by `bridge/claude.mjs --probe-usage` or a `_m4code/usage_limits` update.
pub fn parse(json: &str, fallback_seen_at: i64) -> Option<UsageLimits> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    parse_value(&v, fallback_seen_at)
}

/// Parse a JSON object containing `five_hour` and/or `seven_day` windows.
pub fn parse_value(v: &Value, fallback_seen_at: i64) -> Option<UsageLimits> {
    let pick = |key: &str| -> Option<RateLimit> {
        let w = v.get(key)?;
        if w.is_null() {
            return None;
        }
        let utilization = w
            .get("utilization")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite() && (0.0..=1.0).contains(f))
            .map(|f| f as f32);
        let resets_at = w.get("resets_at").and_then(Value::as_i64).filter(|&t| t > 0);
        let seen_at = w.get("seen_at").and_then(Value::as_i64).filter(|&t| t > 0).unwrap_or(fallback_seen_at);
        (utilization.is_some() || resets_at.is_some()).then_some(RateLimit {
            utilization,
            resets_at,
            seen_at,
        })
    };
    let usage = UsageLimits {
        five_hour: pick("five_hour"),
        seven_day: pick("seven_day"),
    };
    (usage.five_hour.is_some() || usage.seven_day.is_some()).then_some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe_json_with_both_windows() {
        let json = r#"{"available":true,"five_hour":{"utilization":0.42,"resets_at":1800000000,"seen_at":1799990000},"seven_day":{"utilization":0.15,"resets_at":1800500000,"seen_at":1799990000}}"#;
        let usage = parse(json, 999).unwrap();
        assert_eq!(usage.five_hour, Some(RateLimit { utilization: Some(0.42), resets_at: Some(1800000000), seen_at: 1799990000 }));
        assert_eq!(usage.seven_day, Some(RateLimit { utilization: Some(0.15), resets_at: Some(1800500000), seen_at: 1799990000 }));
    }

    #[test]
    fn parses_partial_and_rejects_empty() {
        let one = parse(r#"{"five_hour":{"utilization":0.8,"resets_at":1800000000},"seven_day":null}"#, 1234).unwrap();
        assert_eq!(one.five_hour.unwrap().seen_at, 1234);
        assert_eq!(one.seven_day, None);
        assert_eq!(parse(r#"{"available":false,"five_hour":null,"seven_day":null}"#, 1234), None);
        assert_eq!(parse("not json", 1234), None);
    }
}
