//! Zero-token usage data via Claude Code's own statusLine hook, injected *per process*.
//!
//! Claudiu starts every Claude session with `--settings <file>` whose only content is a `statusLine`
//! pointing back at this executable in a hidden mode (`claudiu --statusline-tee`). Claude Code runs it
//! locally after each response (the docs state the status line "does not consume API tokens") and pipes it
//! a JSON blob that includes `rate_limits.five_hour/seven_day` and the exact `context_window` figures.
//! The tee keeps a tiny snapshot of that and, if the user has their own status line configured, forwards
//! stdin to it and prints its output so their display is unchanged.
//!
//! `--settings` applies to one session and writes nothing to the user's Claude Code config files.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::RateLimit;

pub const TEE_FLAG: &str = "--statusline-tee";
/// Env var carrying the user's own statusLine command for the tee to forward to.
pub const FORWARD_ENV: &str = "CLAUDIU_STATUSLINE_FORWARD";

/// Minimal subset of the status-line JSON that Claudiu keeps (no paths, no costs, no prompts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub session_id: String,
    pub five_hour: Option<RateLimit>,
    pub seven_day: Option<RateLimit>,
    /// 0.0..=1.0, as calculated by Claude Code.
    pub context_fraction: Option<f32>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub model: Option<String>,
    pub captured_at: i64,
}

pub fn data_dir() -> PathBuf {
    crate::store::Store::default_path().parent().map(Path::to_path_buf).unwrap_or_default()
}

pub fn snapshot_dir() -> PathBuf {
    data_dir().join("usage")
}

pub fn settings_dir() -> PathBuf {
    data_dir().join("statusline")
}

/// Parse the status-line stdin JSON. Everything but `session_id` is optional (it is absent early in a session).
pub fn parse(json: &str, now: i64) -> Option<Snapshot> {
    let v: Value = serde_json::from_str(json).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    let limit = |name: &str| -> Option<RateLimit> {
        let w = v.get("rate_limits")?.get(name)?;
        let pct = w.get("used_percentage")?.as_f64()?;
        let resets_at = w.get("resets_at").and_then(|r| r.as_i64().or_else(|| r.as_f64().map(|f| f as i64)));
        Some(RateLimit { utilization: Some((pct / 100.0).clamp(0.0, 1.0) as f32), resets_at, seen_at: now })
    };
    let cw = v.get("context_window");
    let cw_f = |k: &str| cw.and_then(|c| c.get(k));
    Some(Snapshot {
        session_id,
        five_hour: limit("five_hour"),
        seven_day: limit("seven_day"),
        context_fraction: cw_f("used_percentage").and_then(Value::as_f64).map(|p| (p / 100.0).clamp(0.0, 1.0) as f32),
        context_tokens: cw_f("total_input_tokens").and_then(Value::as_u64),
        context_window: cw_f("context_window_size").and_then(Value::as_u64),
        model: v
            .get("model")
            .and_then(|m| m.get("id").or_else(|| m.get("display_name")))
            .and_then(Value::as_str)
            .map(str::to_string),
        captured_at: now,
    })
}

pub fn safe_file_stem(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Entry point of `claudiu --statusline-tee`. Must be fast and must never fail Claude Code's status line.
pub fn run_tee() -> i32 {
    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);
    let text = String::from_utf8_lossy(&input).into_owned();

    if let Some(snap) = parse(&text, crate::store::now())
        && let Ok(body) = serde_json::to_string(&snap) {
            let _ = write_atomic(&snapshot_dir().join(format!("{}.json", safe_file_stem(&snap.session_id))), &body);
        }

    // Preserve the user's own status line, if they have one.
    if let Some(cmd) = std::env::var(FORWARD_ENV).ok().filter(|c| !c.trim().is_empty()) {
        let mut child = shell_command(&cmd);
        child.stdin(Stdio::piped()).stdout(Stdio::inherit()).stderr(Stdio::null());
        if let Ok(mut child) = child.spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&input);
            }
            let _ = child.wait();
        }
    }
    0
}

fn shell_command(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = match git_bash() {
            Some(bash) => {
                let mut c = Command::new(bash);
                c.arg("-c").arg(cmd);
                c
            }
            None => {
                let mut c = Command::new("cmd.exe");
                c.arg("/C").arg(cmd);
                c
            }
        };
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

#[cfg(windows)]
fn git_bash() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from).into_iter().collect();
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(pf) = std::env::var_os(var) {
            candidates.push(Path::new(&pf).join("Git/bin/bash.exe"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(Path::new(&local).join("Programs/Git/bin/bash.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// The status line the user already has, by Claude Code's precedence: local > project > user settings.
/// Read-only; the files are never modified.
pub fn user_statusline(cwd: &Path, claude_home: Option<&Path>) -> Option<Value> {
    let mut files = vec![cwd.join(".claude/settings.local.json"), cwd.join(".claude/settings.json")];
    if let Some(home) = claude_home {
        files.push(home.join("settings.json"));
    }
    files.into_iter().find_map(|f| {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?;
        let sl = v.get("statusLine")?.clone();
        sl.get("command").and_then(Value::as_str).is_some().then_some(sl)
    })
}

/// Build the per-session `--settings` content. The user's own `padding` / `refreshInterval` are kept;
/// only `command` is replaced (by the tee), and their original command is passed via env for forwarding.
pub fn build_settings(user: Option<&Value>, exe: &Path) -> (Value, Option<String>) {
    // Forward slashes survive bash, cmd and PowerShell quoting alike.
    let exe = exe.to_string_lossy().replace('\\', "/");
    let mut sl = user.cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
    let forward = sl.get("command").and_then(Value::as_str).map(str::to_string);
    sl["type"] = json!("command");
    sl["command"] = json!(format!("\"{exe}\" {TEE_FLAG}"));
    (json!({ "statusLine": sl }), forward)
}

/// Write the settings file for one session and return `(path, env)` to spawn Claude with.
pub fn prepare(session_key: &str, cwd: &Path) -> std::io::Result<(PathBuf, Vec<(String, String)>)> {
    let exe = std::env::current_exe()?;
    let claude_home = crate::claude::claude_home();
    let user = user_statusline(cwd, claude_home.as_deref());
    let (settings, forward) = build_settings(user.as_ref(), &exe);
    let path = settings_dir().join(format!("{}.json", safe_file_stem(session_key)));
    write_atomic(&path, &serde_json::to_string(&settings)?)?;
    let env = forward.into_iter().map(|c| (FORWARD_ENV.to_string(), c)).collect();
    Ok((path, env))
}

/// All snapshots currently on disk (tolerant: unreadable files are skipped).
pub fn read_snapshots(dir: &Path) -> Vec<Snapshot> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| serde_json::from_str::<Snapshot>(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect()
}

/// Drop leftovers: per-session settings files after a week, snapshots after a month.
pub fn cleanup() {
    let prune = |dir: PathBuf, max_age_secs: u64| {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() > max_age_secs);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    };
    prune(settings_dir(), 7 * 86_400);
    let [mcp_configs, titles, names] = crate::mcp::cleanup_dirs();
    prune(names, 7 * 86_400);
    prune(mcp_configs, 7 * 86_400);
    // Titles are copied into the store on the next tick, so an old file is only a duplicate.
    prune(titles, 7 * 86_400);
    prune(snapshot_dir(), 30 * 86_400);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shape taken from the status line docs.
    const SAMPLE: &str = r#"{
      "session_id": "abc-123",
      "model": {"id": "claude-opus-5-5", "display_name": "Opus"},
      "context_window": {"total_input_tokens": 15500, "total_output_tokens": 1200, "context_window_size": 200000,
                         "used_percentage": 8, "remaining_percentage": 92},
      "rate_limits": {"five_hour": {"used_percentage": 23.5, "resets_at": 1738425600},
                      "seven_day": {"used_percentage": 41.2, "resets_at": 1738857600}}
    }"#;

    #[test]
    fn parses_limits_and_context() {
        let s = parse(SAMPLE, 1000).unwrap();
        assert_eq!(s.session_id, "abc-123");
        assert_eq!(s.five_hour.as_ref().unwrap().utilization, Some(0.235));
        assert_eq!(s.five_hour.as_ref().unwrap().resets_at, Some(1738425600));
        assert_eq!(s.seven_day.as_ref().unwrap().utilization, Some(0.412));
        assert_eq!(s.context_fraction, Some(0.08));
        assert_eq!(s.context_tokens, Some(15500));
        assert_eq!(s.context_window, Some(200000));
        assert_eq!(s.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(s.captured_at, 1000);
    }

    #[test]
    fn early_session_without_rate_limits_or_context_is_fine() {
        let s = parse(r#"{"session_id":"x","context_window":{"used_percentage":null,"current_usage":null}}"#, 5).unwrap();
        assert!(s.five_hour.is_none() && s.seven_day.is_none() && s.context_fraction.is_none());
        assert!(parse("not json", 5).is_none());
        assert!(parse(r#"{"no":"session"}"#, 5).is_none());
    }

    #[test]
    fn settings_use_the_tee_and_keep_the_users_other_fields() {
        let user = json!({"type": "command", "command": "~/.claude/sl.sh", "padding": 2, "refreshInterval": 30});
        let (settings, forward) = build_settings(Some(&user), Path::new(r"C:\Program Files\Claudiu\claudiu.exe"));
        let sl = &settings["statusLine"];
        assert_eq!(sl["command"], "\"C:/Program Files/Claudiu/claudiu.exe\" --statusline-tee");
        assert_eq!(sl["padding"], 2);
        assert_eq!(sl["refreshInterval"], 30);
        assert_eq!(forward.as_deref(), Some("~/.claude/sl.sh"));

        let (settings, forward) = build_settings(None, Path::new("/usr/bin/claudiu"));
        assert_eq!(settings["statusLine"]["type"], "command");
        assert!(forward.is_none());
    }

    #[test]
    fn user_statusline_follows_claude_code_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let (cwd, home) = (tmp.path().join("proj"), tmp.path().join("home"));
        std::fs::create_dir_all(cwd.join(".claude")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("settings.json"), r#"{"statusLine":{"type":"command","command":"user"}}"#).unwrap();
        assert_eq!(user_statusline(&cwd, Some(&home)).unwrap()["command"], "user");
        std::fs::write(cwd.join(".claude/settings.json"), r#"{"statusLine":{"type":"command","command":"project"}}"#).unwrap();
        assert_eq!(user_statusline(&cwd, Some(&home)).unwrap()["command"], "project");
        std::fs::write(cwd.join(".claude/settings.local.json"), r#"{"statusLine":{"type":"command","command":"local"}}"#).unwrap();
        assert_eq!(user_statusline(&cwd, Some(&home)).unwrap()["command"], "local");
        assert!(user_statusline(&tmp.path().join("none"), None).is_none());
    }

    #[test]
    fn snapshots_roundtrip_and_session_ids_cannot_escape_the_directory() {
        assert_eq!(safe_file_stem("../../evil"), "______evil");
        let tmp = tempfile::tempdir().unwrap();
        let snap = parse(SAMPLE, 9).unwrap();
        write_atomic(&tmp.path().join("abc-123.json"), &serde_json::to_string(&snap).unwrap()).unwrap();
        std::fs::write(tmp.path().join("garbage.json"), "{{{").unwrap();
        let got = read_snapshots(tmp.path());
        assert_eq!(got, vec![snap]);
    }
}
