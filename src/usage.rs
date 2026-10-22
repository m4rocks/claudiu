//! Zero-token usage and status data, reported by the per-process mod (`mod/hooks/register.js`).
//!
//! Every Claude session is started with `--plugin-dir` and `CLAUDIU_USAGE_DIR`; the mod runs inside Claude Code
//! and, on each `session.measure` (context fill, 5-hour / 7-day limits), `config.set` and `command.run`, writes
//! one small JSON file per Claude session into that directory. Nothing is sent anywhere and no tokens are spent.
//! Claudiu reads the files back here. Nothing is written to the user's Claude Code config.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::RateLimit;

/// Env var naming the Claudiu account a Claude session was launched with, so its limits go to that account.
pub const ACCOUNT_ENV: &str = "CLAUDIU_ACCOUNT";
/// Env var holding the directory the mod writes its snapshot into.
pub const USAGE_DIR_ENV: &str = "CLAUDIU_USAGE_DIR";

/// Env var holding the file Claudiu writes settings requests into for the mod to apply.
pub const CTL_ENV: &str = "CLAUDIU_CTL_FILE";

pub fn ctl_dir() -> PathBuf {
    crate::datadir::data_dir().join("ctl")
}

/// The request file of one session, named by Claudiu's own record id (which survives `/clear`).
pub fn ctl_file(record_id: &str) -> PathBuf {
    ctl_dir().join(format!("{}.json", crate::datadir::safe_file_stem(record_id)))
}

/// A model alias or id as the mod accepts it for `/advisor` (it checks the same rule).
pub fn valid_model_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 60 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Ask the mod of a running session to change a setting: `auto_compact` (bool) or `advisor` (a model name or `off`).
/// The file keeps the latest few commands, each with a number that only goes up, so none is lost if two come within
/// the mod's one-second look and none is applied twice.
pub fn send_ctl(file: &Path, key: &str, value: serde_json::Value) {
    let mut cmds: Vec<serde_json::Value> = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["cmds"].as_array().cloned())
        .unwrap_or_default();
    let last = cmds.iter().filter_map(|c| c["seq"].as_i64()).max().unwrap_or(0);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    cmds.push(serde_json::json!({ "seq": ms.max(last + 1), "key": key, "value": value }));
    let keep = cmds.len().saturating_sub(8);
    cmds.drain(..keep);
    let _ = crate::datadir::write_atomic(file, &serde_json::json!({ "cmds": cmds }).to_string());
}

/// What the mod reports (no paths, no costs, no prompts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub session_id: String,
    pub five_hour: Option<RateLimit>,
    pub seven_day: Option<RateLimit>,
    /// 0.0..=1.0 (tokens over the window).
    pub context_fraction: Option<f32>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub model: Option<String>,
    pub captured_at: i64,
    /// Claudiu account the session ran under (`ACCOUNT_ENV`); `None` is the main account.
    #[serde(default)]
    pub account: Option<String>,
    /// The `/config` auto-compact switch.
    #[serde(default)]
    pub auto_compact: Option<bool>,
    /// The `advisorModel` setting (`opus`, `sonnet`, ...); `None` when the advisor is off.
    #[serde(default)]
    pub advisor: Option<String>,
    /// The last slash command the user ran, name and arguments, e.g. `advisor off`.
    #[serde(default)]
    pub last_command: Option<String>,
}

pub fn snapshot_dir() -> PathBuf {
    crate::datadir::data_dir().join("usage")
}

/// All snapshots currently on disk (tolerant: unreadable or half-written files are skipped).
pub fn read_snapshots(dir: &Path) -> Vec<Snapshot> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| serde_json::from_str::<Snapshot>(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shape the mod writes (see `mod/hooks/register.js`, `report`).
    const FROM_MOD: &str = r#"{
      "session_id": "abc-123", "model": "claude-opus-5-5", "captured_at": 1000, "account": "work",
      "context_fraction": 0.028, "context_tokens": 28062, "context_window": 1000000,
      "five_hour": {"utilization": 0.67, "resets_at": 1792659600, "seen_at": 1000},
      "seven_day": {"utilization": 0.22, "resets_at": 1793167200, "seen_at": 1000},
      "auto_compact": true, "advisor": "opus", "last_command": "advisor opus"
    }"#;

    #[test]
    fn requests_keep_rising_numbers_and_only_the_latest_few() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("ctl/me.json");
        for i in 0..10 {
            send_ctl(&file, "advisor", serde_json::json!(format!("m{i}")));
        }
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let cmds = v["cmds"].as_array().unwrap();
        assert_eq!(cmds.len(), 8);
        assert!(cmds.windows(2).all(|w| w[0]["seq"].as_i64() < w[1]["seq"].as_i64()), "numbers only go up");
        assert_eq!(cmds[7]["value"], "m9");
        assert!(valid_model_name("claude-opus-5-5") && valid_model_name("off") && !valid_model_name("") && !valid_model_name("a b") && !valid_model_name("../x"));
    }

    #[test]
    fn reads_what_the_mod_writes_and_skips_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("abc-123.json"), FROM_MOD).unwrap();
        std::fs::write(tmp.path().join("garbage.json"), "{{{").unwrap();
        std::fs::write(tmp.path().join("note.txt"), FROM_MOD).unwrap();
        let got = read_snapshots(tmp.path());
        assert_eq!(got.len(), 1);
        let s = &got[0];
        assert_eq!((s.session_id.as_str(), s.account.as_deref()), ("abc-123", Some("work")));
        assert_eq!(s.five_hour.as_ref().unwrap().utilization, Some(0.67));
        assert_eq!(s.seven_day.as_ref().unwrap().resets_at, Some(1793167200));
        assert_eq!((s.context_tokens, s.context_window), (Some(28062), Some(1_000_000)));
        assert_eq!((s.auto_compact, s.advisor.as_deref(), s.last_command.as_deref()), (Some(true), Some("opus"), Some("advisor opus")));
    }

    #[test]
    fn early_session_with_only_an_id_is_fine() {
        let s: Snapshot = serde_json::from_str(r#"{"session_id":"x","five_hour":null,"seven_day":null,"context_fraction":null,"context_tokens":null,"context_window":null,"model":null,"captured_at":5}"#).unwrap();
        assert!(s.auto_compact.is_none() && s.advisor.is_none() && s.last_command.is_none());
    }
}
