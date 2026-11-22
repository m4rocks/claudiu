//! Claude Code integration. Read-only against Claude Code's data, with one exception: `clear_recorded_worktree` appends a
//! line to a transcript when a session that Claude Code put in a worktree moves to another folder. It never touches
//! Claude Code's config.
//! Launching/resuming uses documented CLI flags (`--session-id`, `--resume`); the transcript scan is a
//! best-effort, tolerant *import* aid: every field is optional and any unparseable line is skipped.
//!
//! Channels between m4code and the sessions it runs: `accounts` (alt config folders), `usage` (the mod's snapshots),
//! `ask` (one-shot completions through the mod), `bridge` (requests from a session), `mcp` (the tab-title server and the
//! per-session config) and `plugin` (the mod m4code ships). `codex_limits` reads Codex's own usage limits from its
//! session logs, and `antigravity_limits` asks the Antigravity CLI for its own.

pub mod accounts;
pub mod acp;
pub mod antigravity_limits;
pub mod antigravity_read_only;
pub mod bridge;
pub mod codex_limits;
pub mod config_sync;
pub mod costly;
pub mod defaults;
pub mod limits;
pub mod mcp;
pub mod plugin;
pub mod subagents;
pub mod trim;
pub mod usage;

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::platform::{find_executable, home_dir};

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

/// A `claude` command for headless use (`-p`): handles `.cmd` shims and never flashes a console window.
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
/// or `waiting`. Read-only; returns session id → status, newest record winning (dead processes leave stale files).
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

const HEAD_BYTES: u64 = 128 * 1024;
const TAIL_BYTES: u64 = 768 * 1024;

fn read_chunks(path: &Path) -> std::io::Result<(String, String, bool, i64, i64)> {
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
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-10-05T14:10:53.395Z"), Some(1791209453));
        assert_eq!(parse_rfc3339("nope"), None);
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
        fs::write(&path, format!("{{\"type\":\"user\"}}
{entered}
{{\"type\":\"assistant\"}}")).unwrap();
        assert!(worktree_recorded(&path).unwrap());

        clear_recorded_worktree(&path, "s").unwrap();
        assert!(!worktree_recorded(&path).unwrap(), "the appended null line is the last one");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(&format!("{{\"type\":\"user\"}}
{entered}
{{\"type\":\"assistant\"}}
")), "a missing final newline is added and nothing else changes");
        assert!(text.lines().last().unwrap().contains("\"worktreeSession\":null"));

        fs::write(&path, format!("{entered}
{{\"type\":\"user\"}}
")).unwrap();
        assert!(worktree_recorded(&path).unwrap());
        fs::write(&path, "{\"type\":\"user\"}
").unwrap();
        assert!(!worktree_recorded(&path).unwrap(), "no line at all means no worktree");
    }

    #[test]
    fn a_worktree_state_line_far_from_the_end_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let filler = format!("{{\"type\":\"user\",\"text\":\"{}\"}}
", "x".repeat(1000));
        let state = r#"{"type":"worktree-state","worktreeSession":{"worktreeName":"t1"},"sessionId":"s"}"#;
        fs::write(&path, format!("{state}
{}", filler.repeat(3000))).unwrap();
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
