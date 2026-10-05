//! One-shot read of the account's 5-hour / 7-day usage at app launch, via `claude -p "/usage"`.
//!
//! The status-line tee (statusline.rs) keeps the meters live, but Claude Code only reports limits after the
//! first reply of a session, so a fresh launch would show nothing. This probe fills that gap once per launch
//! (never polled). It is Claude Code's own CLI: no API calls from Claudiu and no config changes;
//! `--no-session-persistence` keeps it out of the session history. Parsing is tolerant: unrecognised
//! output yields nothing and the meters keep their last (age-labelled) values.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use jiff::civil;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use regex::Regex;
use wait_timeout::ChildExt;

use crate::store::RateLimit;

#[derive(Debug, Default, PartialEq)]
pub struct Reading {
    pub five_hour: Option<RateLimit>,
    pub seven_day: Option<RateLimit>,
}

/// Run the probe. Blocking (a few seconds): call from a background thread.
pub fn fetch(claude: &Path, now: Timestamp) -> Result<Reading, String> {
    let is_script = claude
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut cmd = if is_script {
        let mut c = Command::new("cmd.exe");
        c.arg("/c").arg(claude);
        c
    } else {
        Command::new(claude)
    };
    cmd.args(["-p", "/usage", "--no-session-persistence"])
        // Neutral folder so nothing project-specific is touched.
        .current_dir(crate::platform::home_dir().unwrap_or_else(std::env::temp_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("could not run claude: {e}"))?;
    if child.wait_timeout(Duration::from_secs(90)).map_err(|e| e.to_string())?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("claude /usage timed out".into());
    }
    let mut bytes = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        use std::io::Read;
        let _ = out.read_to_end(&mut bytes);
    }
    let reading = parse(&String::from_utf8_lossy(&bytes), now);
    if reading.five_hour.is_none() && reading.seven_day.is_none() {
        return Err("no usage figures in claude's output".into());
    }
    Ok(reading)
}

/// Parse the human-readable `/usage` text.
pub fn parse(text: &str, now: Timestamp) -> Reading {
    Reading { five_hour: line(text, r"Current session", now), seven_day: line(text, r"Current week \(all models\)", now) }
}

fn line(text: &str, label: &str, now: Timestamp) -> Option<RateLimit> {
    // "Current session: 27% used · resets Oct 5, 10:09pm (Europe/Bucharest)"
    let re = Regex::new(&format!(r"(?im)^\s*{label}\s*:\s*(\d+(?:\.\d+)?)\s*%[^\r\n]*?(?:resets\s+([^\r\n]+?))?\s*$")).ok()?;
    let caps = re.captures(text)?;
    let pct: f32 = caps.get(1)?.as_str().parse().ok()?;
    let resets_at = caps.get(2).and_then(|m| parse_reset(m.as_str(), now));
    Some(RateLimit { utilization: Some((pct / 100.0).clamp(0.0, 1.0)), resets_at, seen_at: now.as_second() })
}

fn month_number(name: &str) -> Option<i8> {
    let lower = name.to_ascii_lowercase();
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
        .iter()
        .position(|m| lower.starts_with(m))
        .map(|i| i as i8 + 1)
}

/// "Oct 5, 10:09pm (Europe/Bucharest)", "4:59pm (Europe/Bucharest)", "10pm".
fn parse_reset(s: &str, now: Timestamp) -> Option<i64> {
    let s = s.trim();
    let (when, tz) = match s.rfind('(') {
        Some(i) => (s[..i].trim(), TimeZone::get(s[i + 1..].trim_end_matches(')').trim()).unwrap_or_else(|_| TimeZone::system())),
        None => (s, TimeZone::system()),
    };
    let re = Regex::new(r"(?i)^(?:([A-Za-z]{3,9})\.?\s+(\d{1,2}),?\s+)?(\d{1,2})(?::(\d{2}))?\s*([ap])\.?m\.?$").ok()?;
    let c = re.captures(when)?;
    let mut hour: i8 = c.get(3)?.as_str().parse().ok()?;
    let minute: i8 = c.get(4).map_or(Some(0), |m| m.as_str().parse().ok())?;
    let pm = c.get(5)?.as_str().eq_ignore_ascii_case("p");
    if !(1..=12).contains(&hour) {
        return None;
    }
    hour = (hour % 12) + if pm { 12 } else { 0 };

    let local_now: Zoned = now.to_zoned(tz.clone());
    let dated = c.get(1).zip(c.get(2));
    let (year, month, day) = match dated {
        Some((m, d)) => (local_now.year(), month_number(m.as_str())?, d.as_str().parse::<i8>().ok()?),
        None => (local_now.year(), local_now.month(), local_now.day()),
    };
    let build = |y: i16, mo: i8, d: i8| civil::date(y, mo, d).at(hour, minute, 0, 0).to_zoned(tz.clone()).ok();
    let mut at = build(year, month, day)?;
    if at.timestamp() < now {
        // A reset is always in the future: roll over a year (date given) or a day (time only).
        at = if dated.is_some() { build(year + 1, month, day)? } else { at.checked_add(jiff::Span::new().days(1)).ok()? };
    }
    Some(at.timestamp().as_second())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "You are currently using your subscription to power your Claude Code usage\n\n\
Current session: 27% used · resets Oct 5, 10:09pm (Europe/Bucharest)\n\
Current week (all models): 54% used · resets Oct 7, 4:59pm (Europe/Bucharest)\n\n\
What's contributing to your limits usage?\n";

    fn now() -> Timestamp {
        // 2026-10-05T17:30:00Z == 20:30 in Bucharest (UTC+3)
        "2026-10-05T17:30:00Z".parse().unwrap()
    }

    #[test]
    fn parses_both_limits() {
        let r = parse(SAMPLE, now());
        let (five, seven) = (r.five_hour.unwrap(), r.seven_day.unwrap());
        assert_eq!(five.utilization, Some(0.27));
        assert_eq!(seven.utilization, Some(0.54));
        assert_eq!(five.resets_at, Some("2026-10-05T19:09:00Z".parse::<Timestamp>().unwrap().as_second()));
        assert_eq!(seven.resets_at, Some("2026-10-07T13:59:00Z".parse::<Timestamp>().unwrap().as_second()));
        assert_eq!(five.seen_at, now().as_second());
    }

    #[test]
    fn time_only_and_hour_only_resets() {
        let r = parse("Current session: 3% used · resets 10pm (Europe/Bucharest)\n", now()).five_hour.unwrap();
        assert_eq!(r.resets_at, Some("2026-10-05T19:00:00Z".parse::<Timestamp>().unwrap().as_second()));
        // already past today -> tomorrow
        let r = parse("Current session: 3% used · resets 8:00am (Europe/Bucharest)\n", now()).five_hour.unwrap();
        assert_eq!(r.resets_at, Some("2026-10-06T05:00:00Z".parse::<Timestamp>().unwrap().as_second()));
    }

    #[test]
    fn year_rollover() {
        let dec: Timestamp = "2026-12-31T20:00:00Z".parse().unwrap();
        let r = parse("Current week (all models): 10% used · resets Jan 2, 9:00am (UTC)\n", dec).seven_day.unwrap();
        assert_eq!(r.resets_at, Some("2027-01-02T09:00:00Z".parse::<Timestamp>().unwrap().as_second()));
    }

    #[test]
    fn missing_reset_or_unknown_text_is_tolerated() {
        let r = parse("Current session: 0% used\n", now()).five_hour.unwrap();
        assert_eq!((r.utilization, r.resets_at), (Some(0.0), None));
        assert_eq!(parse("Something else entirely", now()), Reading::default());
        assert_eq!(parse("", now()), Reading::default());
    }
}
