//! Claude Code adapter, through Anthropic's Claude Agent SDK. The SDK is a Node library, so m4code ships a small script
//! (`bridge/` in the repository) that drives it and speaks ACP like the other agents. The SDK runs the user's own
//! `claude` binary (`CLAUDE_CODE_EXECUTABLE`), so their install, settings, plugins and sign-in apply. m4code's mod,
//! tools and launch flags reach it through `_meta.claudeCode.options`.
//!
//! All Claude Code-specific logic (CLI discovery, embedded mod plugin, usage snapshots, settings trims, costly env
//! checks, and transcript worktree inspection) lives inside this module, keeping the rest of m4code harness-agnostic.

pub mod costly;
pub mod plugin;
pub mod trim;
pub mod usage;

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::platform::{find_executable, home_dir};
use super::{Adapter, Reopen, Server};

/// The bridge's files, embedded in the binary. The lockfile pins the SDK, so an SDK change reaches users only with a
/// m4code release.
const FILES: [(&str, &str); 4] = [
    ("claude.mjs", include_str!("../../../bridge/claude.mjs")),
    ("background-shells.mjs", include_str!("../../../bridge/background-shells.mjs")),
    ("package.json", include_str!("../../../bridge/package.json")),
    ("package-lock.json", include_str!("../../../bridge/package-lock.json")),
];

/// The SDK's own package, present once `npm ci` has finished in the bridge's folder.
const SDK: &str = "node_modules/@anthropic-ai/claude-agent-sdk/package.json";

/// Where the SDK finds the user's Claude Code. Without it, it would look for a copy of its own.
pub const EXECUTABLE_ENV: &str = "CLAUDE_CODE_EXECUTABLE";

pub const ADAPTER: Adapter = Adapter {
    name: "Claude Code",
    server_name: "claude-bridge",
    server: || server(&crate::datadir::data_dir()),
    reopen: Reopen::Load,
    delete_model_session: false,
    compact_command: Some("compact"),
    default_models: super::CatalogModel::default_claude_models,
    steers: true,
};

/// Sessions starting together must not install the SDK into the same folder at once.
static INSTALLING: Mutex<()> = Mutex::new(());

/// The bridge, written to the data dir with the SDK installed beside it: `node claude.mjs`.
fn server(data: &Path) -> Result<Server, String> {
    let node = super::find("node").ok_or("Node.js was not found on PATH. Claude Code sessions need it for the Claude Agent SDK.")?;
    let dir = plugin::install_into(data, "claude-bridge", &FILES).map_err(|e| format!("Couldn't write the Claude Code bridge: {e}"))?;
    let _one_at_a_time = INSTALLING.lock().unwrap_or_else(|e| e.into_inner());
    if !dir.join(SDK).is_file() {
        install_sdk(&dir)?;
    }
    Ok((node, vec![dir.join("claude.mjs").to_string_lossy().into_owned()]))
}

/// `npm ci` from the lockfile, without the SDK's optional packages: those are its own copies of `claude` for each
/// platform, and the user's is used instead.
fn install_sdk(dir: &Path) -> Result<(), String> {
    let npm = super::find("npm").ok_or("npm was not found on PATH. Claude Code sessions need it once, to install the Claude Agent SDK.")?;
    let mut cmd = command(&npm);
    cmd.args(["ci", "--omit=optional", "--legacy-peer-deps", "--no-audit", "--no-fund"]).current_dir(dir);
    crate::platform::with_login_path(&mut cmd);
    let out = cmd.output().map_err(|e| format!("Couldn't run npm: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output");
        return Err(format!("Installing the Claude Agent SDK failed: {last}"));
    }
    Ok(())
}

/// The bridge's process environment for a session of `claude`.
pub fn environment(claude: &Path) -> Vec<(String, String)> {
    vec![(EXECUTABLE_ENV.into(), claude.to_string_lossy().into_owned())]
}

/// Environment for probing Claude Code's available models and permission modes.
pub fn probe_env(claude: &Path) -> Vec<(String, String)> {
    let mut env = environment(claude);
    env.push(("CLAUDE_CODE_AUTO_CONNECT_IDE".into(), "false".into()));
    env.push(("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL".into(), "1".into()));
    env
}

/// MCP instruction/tool names to hide when Claude Code trims those built-in tools.
pub fn trimmed_mcp_tools() -> Vec<&'static str> {
    let mut off = Vec::new();
    if trim::FLUFF.contains(&trim::Trim::AskUserQuestion) {
        off.push("ask_user_question");
    }
    if trim::FLUFF.contains(&trim::Trim::Artifacts) {
        off.push("artifacts");
    }
    off
}

/// Where m4code puts the Claude Code session name it wants for a session; the mod (`M4CODE_NAME_FILE`)
/// applies it with `/rename`, so nothing is typed into the PTY.
pub fn name_file(claude_session_id: &str) -> PathBuf {
    crate::datadir::data_dir().join("names").join(format!("{}.txt", crate::datadir::safe_file_stem(claude_session_id)))
}

/// Ask the mod for this session name (or none, for a nameless session).
pub fn write_name(claude_session_id: &str, name: Option<&str>) {
    let path = name_file(claude_session_id);
    let _ = match name {
        Some(name) => crate::datadir::write_atomic(&path, name),
        None => std::fs::remove_file(&path),
    };
}

/// Build the process environment and ACP `_meta` for launching a Claude Code session.
#[allow(clippy::too_many_arguments)]
pub fn prepare_session(
    claude_exe: &Path,
    record_id: &str,
    session_id: Option<&str>,
    name: Option<String>,
    chrome: bool,
    tools_off: &mut Vec<&'static str>,
    settings: &crate::store::Settings,
) -> (Vec<(String, String)>, Value) {
    let mut env = probe_env(claude_exe);
    let plugin_dir = plugin::install().ok();
    if plugin_dir.is_some() {
        env.push(("M4CODE_MANAGED_COMPACTION".into(), "1".into()));
        if let Some(id) = session_id {
            env.push(("M4CODE_NAME_FILE".into(), name_file(id).to_string_lossy().into_owned()));
        }
        env.push((usage::USAGE_DIR_ENV.into(), usage::snapshot_dir().to_string_lossy().into_owned()));
        env.push((usage::CTL_ENV.into(), usage::ctl_file(record_id).to_string_lossy().into_owned()));
        env.push((crate::visual::VISUAL_ENV.into(), crate::visual::prefix(record_id).to_string_lossy().into_owned()));
        // The mod registers the agent types and answers their turns, so they go only where it loads.
        if let Some(subagents) = settings.custom_subagents.env_for(&settings.enabled_harnesses_list())
            && let Ok(exe) = crate::platform::own_exe()
        {
            env.push((super::subagents::ENV.into(), subagents));
            env.push((super::subagents::EXE_ENV.into(), exe.to_string_lossy().into_owned()));
        }
    } else if !tools_off.contains(&"visual") {
        tools_off.push("visual");
    }
    let mut extra = vec![];
    if let Some(name) = name {
        extra.push(("name", Some(name)));
    }
    if chrome {
        extra.push(("chrome", None));
    }
    let session_meta = claude_meta(plugin_dir.as_deref(), &crate::mcp::allowed_tools(tools_off), &extra, Some(trim::session_settings()));
    (env, session_meta)
}

/// Claude Code's part of `session/new`: the bridge passes `_meta.claudeCode.options` to the SDK. `plugin` is
/// m4code's mod; `allowed` the tools it may use unasked; `extra` the flags given as `--name value` (None for a bare
/// flag).
/// `settings` is the session's own settings layer (the Agent SDK's `settings` option), such as its trims.
pub fn claude_meta(plugin: Option<&Path>, allowed: &[String], extra: &[(&str, Option<String>)], settings: Option<Value>) -> Value {
    let mut options = json!({});
    if let Some(dir) = plugin {
        options["plugins"] = json!([{ "type": "local", "path": dir }]);
    }
    if !allowed.is_empty() {
        options["allowedTools"] = json!(allowed);
    }
    if let Some(settings) = settings {
        options["settings"] = settings;
    }
    if !extra.is_empty() {
        options["extraArgs"] = extra.iter().map(|(k, v)| (k.to_string(), v.clone().map_or(Value::Null, Value::String))).collect::<serde_json::Map<_, _>>().into();
    }
    json!({ "claudeCode": { "options": options } })
}

/// Locate the user's `claude` executable: explicit setting, PATH, then well-known install dirs.
pub fn discover(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = configured.filter(|p| p.is_file()) {
        return Some(p.to_path_buf());
    }
    if let Some(p) = find_executable("claude") {
        return Some(p);
    }
    let home = home_dir()?;
    let mut candidates = vec![
        home.join(".local/bin/claude.exe"),
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude.exe"),
        home.join(".claude/local/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ];
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(appdata).join("npm/claude.cmd"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// A command for headless use: handles `.cmd` shims and never flashes a console window.
pub fn command(exe: &Path) -> std::process::Command {
    let is_script = exe.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut cmd = if is_script {
        let mut c = std::process::Command::new("cmd.exe");
        c.arg("/c").arg(exe);
        c
    } else {
        std::process::Command::new(exe)
    };
    crate::platform::no_window(&mut cmd);
    cmd
}

/// Claude Code's data directory (`CLAUDE_CONFIG_DIR` is the documented override).
pub fn claude_home() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| home_dir().map(|h| h.join(".claude")))
}

/// One registry file as last read: its modified time and size, and what it said (session id, `updatedAt`, status).
type StatusRecord = (Option<std::time::SystemTime>, u64, Option<(String, i64, String)>);

/// The registry files `session_statuses` has read, so a file is parsed again only once it changes: the poll runs every
/// second and most files sit unchanged between two polls. A file written in the last few seconds is always read
/// again, since a timestamp may not have moved yet (coarse clocks) when a status of the same length replaced another.
#[derive(Default)]
pub struct StatusFiles(std::collections::HashMap<PathBuf, StatusRecord>);

/// Claude Code's own per-process registry (`sessions/<pid>.json`) says whether each session is `busy`, `idle`
/// or `waiting`. Read-only; returns session id -> status, newest record winning (dead processes leave stale files).
pub fn session_statuses(home: &Path, seen: &mut StatusFiles) -> std::collections::HashMap<String, String> {
    let mut best: std::collections::HashMap<String, (i64, String)> = Default::default();
    let mut present = std::collections::HashSet::new();
    for entry in fs::read_dir(home.join("sessions")).into_iter().flatten().flatten() {
        let path = entry.path();
        // Asked of the file itself: what a directory listing reports on Windows can lag behind a write.
        let Ok(meta) = fs::metadata(&path) else { continue };
        let (modified, len) = (meta.modified().ok(), meta.len());
        let settled = modified.and_then(|m| m.elapsed().ok()).is_some_and(|age| age > std::time::Duration::from_secs(3));
        let record = match seen.0.get(&path) {
            Some((m, l, said)) if settled && *m == modified && *l == len => said.clone(),
            _ => {
                let said = read_status(&path);
                seen.0.insert(path.clone(), (modified, len, said.clone()));
                said
            }
        };
        present.insert(path);
        let Some((id, at, status)) = record else { continue };
        if best.get(&id).is_none_or(|(t, _)| at >= *t) {
            best.insert(id, (at, status));
        }
    }
    seen.0.retain(|path, _| present.contains(path));
    best.into_iter().map(|(k, (_, s))| (k, s)).collect()
}

fn read_status(path: &Path) -> Option<(String, i64, String)> {
    let v = serde_json::from_str::<Value>(&fs::read_to_string(path).ok()?).ok()?;
    let id = v.get("sessionId").and_then(Value::as_str)?;
    let status = v.get("status").and_then(Value::as_str)?;
    let at = v.get("updatedAt").and_then(Value::as_i64).unwrap_or(0);
    Some((id.to_string(), at, status.to_string()))
}

/// Find the transcript for a session id without knowing Claude's path-encoding scheme.
pub fn find_transcript(home: &Path, session_id: &str) -> Option<PathBuf> {
    let name = format!("{session_id}.jsonl");
    fs::read_dir(home.join("projects")).ok()?.flatten().map(|d| d.path().join(&name)).find(|p| p.is_file())
}

/// Whether the transcript's last `worktree-state` line names a worktree. Claude Code writes the line whenever a session
/// enters or leaves one and restores it on every resume, which wins over the folder `claude` is started in.
pub fn worktree_recorded(transcript: &Path) -> std::io::Result<bool> {
    const CHUNK: u64 = 1 << 20;
    let mut file = File::open(transcript)?;
    let mut start = file.metadata()?.len();
    let mut tail: Vec<u8> = vec![];
    // The line repeats every few lines of a transcript, so the last one is nearly always in the final chunk.
    while start > 0 {
        let from = start.saturating_sub(CHUNK);
        file.seek(SeekFrom::Start(from))?;
        let mut chunk = vec![0; (start - from) as usize];
        file.read_exact(&mut chunk)?;
        chunk.extend(tail);
        (tail, start) = (chunk, from);
        let text = String::from_utf8_lossy(&tail);
        // Unless the whole file is in view, the first line may be cut in half.
        let whole = if start == 0 { &text[..] } else { text.split_once('\n').map_or("", |(_, rest)| rest) };
        if let Some(recorded) = whole.lines().rev().find_map(worktree_state) {
            return Ok(recorded);
        }
    }
    Ok(false)
}

fn worktree_state(line: &str) -> Option<bool> {
    if !line.contains("\"worktree-state\"") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    (v["type"] == "worktree-state").then(|| !v["worktreeSession"].is_null())
}

/// Append the line Claude Code's own ExitWorktree writes, so that a resume starts in the folder `claude` is launched
/// from. Only appends, and only while no process of the session runs. Blocking.
pub fn clear_recorded_worktree(transcript: &Path, session_id: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new().read(true).append(true).open(transcript)?;
    let mut line = String::new();
    if file.metadata()?.len() > 0 {
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0u8];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.push('\n');
        }
    }
    line.push_str(&serde_json::json!({"type": "worktree-state", "worktreeSession": null, "sessionId": session_id}).to_string());
    line.push('\n');
    file.write_all(line.as_bytes())
}

pub fn context_limit(window: Option<u64>, compact_at: Option<u64>, auto_compact: Option<bool>) -> Option<u64> {
    compact_at.filter(|&t| t > 0 && auto_compact != Some(false)).or(window).filter(|&w| w > 0)
}

pub fn context_fraction(tokens: u64) -> f32 {
    let window: f64 = if tokens > 200_000 { 1_000_000.0 } else { 200_000.0 };
    (tokens as f64 / window).min(1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_carry_the_mod_tools_and_flags() {
        let meta = claude_meta(Some(Path::new("/mod")), &["mcp__m4code__show_visual".into()], &[("name", Some("Fix it".into())), ("chrome", None)], Some(json!({"disableWorkflows": true})));
        let options = &meta["claudeCode"]["options"];
        assert_eq!(options["plugins"], json!([{"type": "local", "path": "/mod"}]));
        assert_eq!(options["allowedTools"], json!(["mcp__m4code__show_visual"]));
        assert_eq!(options["extraArgs"], json!({"name": "Fix it", "chrome": null}));
        assert_eq!(options["settings"], json!({"disableWorkflows": true}));
        assert_eq!(claude_meta(None, &[], &[], None), json!({"claudeCode": {"options": {}}}), "nothing given adds nothing");
    }

    #[test]
    fn the_bridge_runs_the_users_claude() {
        assert_eq!(environment(Path::new("/opt/claude")), vec![(EXECUTABLE_ENV.to_string(), "/opt/claude".to_string())]);
    }

    #[test]
    fn statuses_follow_registry_files_as_they_change_and_go() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("sessions");
        fs::create_dir(&dir).unwrap();
        let write = |name: &str, status: &str| fs::write(dir.join(name), format!(r#"{{"sessionId":"s1","status":"{status}","updatedAt":1}}"#)).unwrap();
        let mut seen = StatusFiles::default();
        write("1.json", "busy");
        assert_eq!(session_statuses(home.path(), &mut seen).get("s1").map(String::as_str), Some("busy"));
        // Busy to idle: the same length, and a timestamp that hasn't moved.
        let stamp = fs::metadata(dir.join("1.json")).unwrap().modified().unwrap();
        write("1.json", "idle");
        File::options().write(true).open(dir.join("1.json")).unwrap().set_modified(stamp).unwrap();
        assert_eq!(session_statuses(home.path(), &mut seen).get("s1").map(String::as_str), Some("idle"));
        fs::remove_file(dir.join("1.json")).unwrap();
        assert!(session_statuses(home.path(), &mut seen).is_empty());
    }

    #[test]
    fn the_last_worktree_state_line_decides_and_clearing_appends_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let entered = r#"{"type":"worktree-state","worktreeSession":{"worktreeName":"t1"},"sessionId":"s"}"#;
        fs::write(&path, format!("{{\"type\":\"user\"}}\n{entered}\n{{\"type\":\"assistant\"}}")).unwrap();
        assert!(worktree_recorded(&path).unwrap());

        clear_recorded_worktree(&path, "s").unwrap();
        assert!(!worktree_recorded(&path).unwrap(), "the appended null line is the last one");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(&format!("{{\"type\":\"user\"}}\n{entered}\n{{\"type\":\"assistant\"}}\n")), "a missing final newline is added and nothing else changes");
        assert!(text.lines().last().unwrap().contains("\"worktreeSession\":null"));

        fs::write(&path, format!("{entered}\n{{\"type\":\"user\"}}\n")).unwrap();
        assert!(worktree_recorded(&path).unwrap());
        fs::write(&path, "{\"type\":\"user\"}\n").unwrap();
        assert!(!worktree_recorded(&path).unwrap(), "no line at all means no worktree");
    }

    #[test]
    fn a_worktree_state_line_far_from_the_end_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let filler = format!("{{\"type\":\"user\",\"text\":\"{}\"}}\n", "x".repeat(1000));
        let state = r#"{"type":"worktree-state","worktreeSession":{"worktreeName":"t1"},"sessionId":"s"}"#;
        fs::write(&path, format!("{state}\n{}", filler.repeat(3000))).unwrap();
        assert!(fs::metadata(&path).unwrap().len() > 2 << 20);
        assert!(worktree_recorded(&path).unwrap());
    }

    #[test]
    fn the_bar_is_full_at_the_threshold_while_auto_compact_is_on() {
        assert_eq!(context_limit(Some(1_000_000), Some(167_000), Some(true)), Some(167_000));
        assert_eq!(context_limit(Some(1_000_000), Some(167_000), None), Some(167_000));
        assert_eq!(context_limit(Some(1_000_000), Some(167_000), Some(false)), Some(1_000_000), "off: the model's window");
        assert_eq!(context_limit(Some(1_000_000), None, Some(true)), Some(1_000_000));
        assert_eq!(context_limit(None, None, None), None);
    }

    #[test]
    fn context_fraction_switches_window() {
        assert!((context_fraction(100_000) - 0.5).abs() < 1e-6);
        assert!((context_fraction(500_000) - 0.5).abs() < 1e-6);
    }
}
