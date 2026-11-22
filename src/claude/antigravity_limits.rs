//! Antigravity's 5-hour and weekly limits, from the CLI's own `agy -p /usage --output-format json`. Since agy 1.1.11
//! the CLI answers `/usage` itself, without a model turn, so a reading costs no tokens; it does ask Google's servers,
//! so the sidebar asks only every few minutes (`REFRESH_EVERY`). The JSON is agy's own and undocumented, so anything
//! that doesn't parse leaves the last reading in the sidebar.
//!
//! Antigravity splits its models into groups ("Gemini Models", "Claude and GPT models"), each with its own limits, so a
//! reading is one `Pool` per group.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::store::{AccountUsage, RateLimit};
use super::limits::Pool;

/// How often the sidebar asks again while Antigravity subagents are on.
pub const REFRESH_EVERY: Duration = Duration::from_secs(300);
/// `/usage` answers in well under a second; a CLI that hangs (an update, a sign-in prompt) is stopped.
const TIMEOUT: Duration = Duration::from_secs(30);
/// More than any reading needs, so a runaway child can't fill memory.
const OUTPUT_MAX: u64 = 1 << 20;

/// The first agy that answers `/usage` itself. An older one sends it to the model, which spends a turn every ask.
const FIRST_WITH_USAGE: (u32, u32, u32) = (1, 1, 11);

/// Runs `agy` for its limits, once its version shows it answers `/usage` itself. An error says why there is no
/// reading.
pub fn read(agy: &Path, cwd: &Path) -> Result<Vec<Pool>, String> {
    let version = output(agy, &["--version"], cwd)?;
    if !version_has_usage(&version) {
        return Err(format!("agy {} is older than {}.{}.{}, which first answers /usage without the model", version.trim(), FIRST_WITH_USAGE.0, FIRST_WITH_USAGE.1, FIRST_WITH_USAGE.2));
    }
    let out = output(agy, &["-p", "/usage", "--output-format", "json"], cwd)?;
    parse(&out, time::OffsetDateTime::now_utc().unix_timestamp()).ok_or_else(|| "agy's /usage gave no limits".into())
}

/// Whether `agy --version` (`1.5.12`) names a version that answers `/usage` itself. Anything unreadable doesn't.
fn version_has_usage(version: &str) -> bool {
    let mut parts = version.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch))) => (major, minor, patch) >= FIRST_WITH_USAGE,
        _ => false,
    }
}

/// What `agy args` prints, given `TIMEOUT` to finish.
fn output(agy: &Path, args: &[&str], cwd: &Path) -> Result<String, String> {
    let mut command = Command::new(agy);
    command
        .args(args)
        .current_dir(cwd)
        // An update mid-read would leave the sidebar waiting; agy updates itself on its next interactive start.
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::platform::no_window(&mut command);
    crate::platform::with_login_path(&mut command);
    let mut child = command.spawn().map_err(|e| format!("agy could not start ({e})"))?;
    let stdout = child.stdout.take().ok_or("agy could not start (no pipe)")?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.take(OUTPUT_MAX).read_to_string(&mut out);
        out
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("agy didn't answer within {} seconds", TIMEOUT.as_secs()));
            }
        }
    }
    Ok(reader.join().unwrap_or_default())
}

/// The pools in agy's `/usage` JSON, each with the windows it reported, seen at `seen_at`. None when the answer isn't
/// a `/usage` reading or holds no window: an older agy that sent `/usage` to the model answers with text instead.
pub fn parse(json: &str, seen_at: i64) -> Option<Vec<Pool>> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let command = v.get("command")?;
    if command.get("name")?.as_str()? != "usage" {
        return None;
    }
    let pools: Vec<Pool> = command["data"]["groups"]
        .as_array()?
        .iter()
        .filter_map(|group| {
            let mut usage = AccountUsage::default();
            for bucket in group["buckets"].as_array().into_iter().flatten() {
                let limit = RateLimit {
                    // An unknown fraction stays unknown rather than reading as empty or full.
                    utilization: bucket["remaining_fraction"].as_f64().filter(|f| (0.0..=1.0).contains(f)).map(|f| (1.0 - f) as f32),
                    resets_at: bucket["reset_time"].as_str().and_then(super::parse_rfc3339),
                    seen_at,
                };
                match bucket["window"].as_str() {
                    Some("5h") => usage.five_hour = Some(limit),
                    Some("weekly") => usage.seven_day = Some(limit),
                    _ => {}
                }
            }
            let name = group["name"].as_str()?.trim();
            (!name.is_empty() && (usage.five_hour.is_some() || usage.seven_day.is_some())).then(|| Pool { name: name.to_string(), usage })
        })
        .collect();
    (!pools.is_empty()).then_some(pools)
}

/// What the sidebar marks a pool's row with, by the models in it.
#[derive(Debug, PartialEq)]
pub enum PoolMark {
    /// `Gemini Models`.
    Gemini,
    /// `Claude and GPT models`.
    ClaudeAndGpt,
    /// A named Gemini model with limits of its own, by the first two letters of its name: `Gemini 4 Argon` → `Ar`.
    GeminiNamed(String),
    /// A pool m4code doesn't know, by the first two letters of its name: `Some New models` → `So`.
    Other(String),
}

pub fn mark(pool: &str) -> PoolMark {
    let lower: Vec<String> = pool.split_whitespace().filter(|w| !w.eq_ignore_ascii_case("models")).map(str::to_lowercase).collect();
    match lower.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["gemini"] => PoolMark::Gemini,
        ["claude", "and", "gpt"] => PoolMark::ClaudeAndGpt,
        ["gemini", version, name] if version.parse::<u32>().is_ok() => PoolMark::GeminiNamed(symbol(name)),
        [first, ..] => PoolMark::Other(symbol(first)),
        [] => PoolMark::Other(symbol(&pool.to_lowercase())),
    }
}

/// A name's first two letters, written like an element's symbol: `argon` → `Ar`.
fn symbol(name: &str) -> String {
    let mut letters = name.chars();
    letters.next().map(|first| first.to_uppercase().chain(letters.take(1)).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // What agy 1.5.12 answered, with the descriptions cut short.
    const READING: &str = r#"{"conversation_id":"","status":"SUCCESS","response":"Gemini Models\tWeekly Limit Remaining\t100%\n","num_turns":0,"usage":{"input_tokens":0,"output_tokens":0},"command":{"name":"usage","data":{"description":"Within each group, models share a weekly limit and a 5-hour limit.","groups":[{"name":"Gemini Models","description":"Models within this group: Gemini 4 Neon","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.75,"reset_time":"2026-11-26T06:49:30Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":0.5,"reset_time":"2026-11-19T11:49:30Z"}]},{"name":"Claude and GPT models","buckets":[{"id":"3p-weekly","window":"weekly","remaining_fraction":1,"reset_time":"2026-11-26T06:55:31Z"},{"id":"3p-5h","window":"5h","remaining_fraction":1,"reset_time":"2026-11-19T11:55:31Z"}]},{"name":"Gemini 4 Argon","buckets":[{"id":"gemini-argon-wtu-weekly","window":"weekly","remaining_fraction":1,"reset_time":"2026-11-26T06:55:31Z"}]}]}}}"#;

    #[test]
    fn each_group_is_a_pool_with_its_windows_as_used_fractions() {
        let pools = parse(READING, 100).unwrap();
        assert_eq!(pools.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Gemini Models", "Claude and GPT models", "Gemini 4 Argon"]);
        let gemini = &pools[0].usage;
        assert_eq!(gemini.five_hour, Some(RateLimit { utilization: Some(0.5), resets_at: super::super::parse_rfc3339("2026-11-19T11:49:30Z"), seen_at: 100 }));
        assert_eq!(gemini.seven_day.as_ref().unwrap().utilization, Some(0.25));
        assert_eq!(pools[1].usage.five_hour.as_ref().unwrap().utilization, Some(0.0));
        assert_eq!((pools[2].usage.five_hour.is_none(), pools[2].usage.seven_day.is_some()), (true, true), "a pool with only a weekly limit");
    }

    #[test]
    fn a_missing_or_odd_fraction_stays_unknown() {
        let missing = READING.replace(r#""remaining_fraction":0.5,"#, "").replace(r#""remaining_fraction":0.75"#, r#""remaining_fraction":7"#);
        let gemini = &parse(&missing, 0).unwrap()[0].usage;
        assert_eq!(gemini.five_hour.as_ref().unwrap().utilization, None);
        assert_eq!(gemini.seven_day.as_ref().unwrap().utilization, None);
    }

    #[test]
    fn an_answer_that_is_not_a_usage_reading_gives_none() {
        // An agy older than 1.1.11 sends `/usage` to the model, which answers in prose.
        assert_eq!(parse(r#"{"status":"SUCCESS","response":"I don't have access to your quota.","num_turns":1}"#, 0), None);
        assert_eq!(parse(&READING.replace(r#""name":"usage""#, r#""name":"quota""#), 0), None);
        assert_eq!(parse("not json", 0), None);
        assert_eq!(parse(r#"{"command":{"name":"usage","data":{"groups":[{"name":"X","buckets":[{"window":"daily"}]}]}}}"#, 0), None, "no window m4code shows");
    }

    #[test]
    fn only_an_agy_that_answers_usage_itself_is_asked() {
        assert!(version_has_usage("1.5.12
"));
        assert!(version_has_usage("1.1.11"));
        assert!(version_has_usage("2.0.0"));
        assert!(!version_has_usage("1.1.10"));
        assert!(!version_has_usage("1.0.99"));
        assert!(!version_has_usage("agy version unknown"));
        assert!(!version_has_usage(""));
    }

    #[test]
    fn each_pool_gets_the_mark_of_its_models() {
        assert_eq!(mark("Gemini Models"), PoolMark::Gemini);
        assert_eq!(mark("Claude and GPT models"), PoolMark::ClaudeAndGpt);
        assert_eq!(mark("Gemini 4 Argon"), PoolMark::GeminiNamed("Ar".into()));
        assert_eq!(mark("Gemini 5 XENON"), PoolMark::GeminiNamed("Xe".into()));
        assert_eq!(mark("Gemini custom pool"), PoolMark::Other("Ge".into()));
        assert_eq!(mark("Other 4 Argon"), PoolMark::Other("Ot".into()));
        assert_eq!(mark("Mistral and Llama models"), PoolMark::Other("Mi".into()));
    }
}
