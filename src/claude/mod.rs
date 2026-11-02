//! Claude Code integration. Read-only against Claude Code's data, with one exception: `clear_recorded_worktree` appends a
//! line to a transcript when a session that Claude Code put in a worktree moves to another folder. It never touches
//! Claude Code's config.
//! Launching/resuming uses documented CLI flags (`--session-id`, `--resume`); the transcript scan is a
//! best-effort, tolerant *import* aid: every field is optional and any unparseable line is skipped.
//!
//! Channels between Claudiu and the sessions it runs: `accounts` (alt config folders), `usage` (the mod's snapshots),
//! `ask` (one-shot completions through the mod), `bridge` (requests from a session), `mcp` (the tab-title server and the
//! per-session config) and `plugin` (the mod Claudiu ships).

pub mod accounts;
pub mod ask;
pub mod bridge;
pub mod config_sync;
pub mod defaults;
pub mod mcp;
pub mod plugin;
pub mod trim;
pub mod usage;

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::platform::{find_executable, home_dir};
use crate::store::ExternalSession;
use crate::terminal::SpawnSpec;

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

fn spec(exe: &Path, cwd: &Path, args: Vec<String>) -> SpawnSpec {
    SpawnSpec { program: exe.to_string_lossy().into_owned(), args, cwd: Some(cwd.to_path_buf()), ..Default::default() }
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

/// New interactive session. We choose the session id up front so Claudiu knows which transcript is ours.
pub fn new_session_spec(exe: &Path, cwd: &Path, session_id: &str) -> SpawnSpec {
    spec(exe, cwd, vec!["--session-id".into(), session_id.into()])
}

/// Resume through Claude Code's supported mechanism.
pub fn resume_spec(exe: &Path, cwd: &Path, session_id: &str) -> SpawnSpec {
    spec(exe, cwd, vec!["--resume".into(), session_id.into()])
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

pub struct ScanResult {
    pub sessions: Vec<ExternalSession>,
    /// False when the directory couldn't be read: callers must not conclude anything is gone.
    pub ok: bool,
}

/// Scan every transcript under `<home>/projects/*/*.jsonl`. Meant to run on a background thread.
pub fn scan_all(home: &Path) -> ScanResult {
    let mut out = ScanResult { sessions: vec![], ok: false };
    let Ok(dirs) = fs::read_dir(home.join("projects")) else { return out };
    out.ok = true;
    for dir in dirs.flatten() {
        let Ok(files) = fs::read_dir(dir.path()) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(scan) = read_session(&path) {
                out.sessions.push(scan.session);
            }
        }
    }
    out
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

pub struct SessionScan {
    pub session: ExternalSession,
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

fn first_prompt_text(v: &Value) -> Option<String> {
    if v.get("isMeta").and_then(Value::as_bool).unwrap_or(false) || v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let content = v.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().find_map(|i| i.get("text").and_then(Value::as_str).map(str::to_string))?,
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') || text.starts_with("Caveat:") {
        return None;
    }
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(one_line.chars().take(80).collect())
}

/// What was said in a transcript as plain text ("User: …" / "Claude: …"), the start and the newest part when long.
/// Only for naming a session; tool calls and results are left out.
pub fn conversation_text(path: &Path) -> String {
    const HEAD: usize = 1500;
    const MAX: usize = 8000;
    let Ok(text) = std::fs::read_to_string(path) else { return String::new() };
    let mut out = String::new();
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let who = match v.get("type").and_then(Value::as_str) {
            Some("user") => "User",
            Some("assistant") => "Claude",
            _ => continue,
        };
        if v.get("isMeta").and_then(Value::as_bool) == Some(true) || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let content = v.get("message").and_then(|m| m.get("content"));
        let said: Vec<&str> = match content {
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(items)) => items.iter().filter(|i| i.get("type").and_then(Value::as_str) == Some("text")).filter_map(|i| i.get("text").and_then(Value::as_str)).collect(),
            _ => vec![],
        };
        for t in said.into_iter().map(str::trim).filter(|t| !t.is_empty() && !t.starts_with('<') && !t.starts_with("Caveat:")) {
            out.push_str(&format!("{who}: {}

", t.chars().take(1500).collect::<String>()));
        }
    }
    let n = out.chars().count();
    if n <= MAX {
        return out;
    }
    let head: String = out.chars().take(HEAD).collect();
    let tail: String = out.chars().skip(n - (MAX - HEAD)).collect();
    format!("{head}
[…]
{tail}")
}

pub fn read_session(path: &Path) -> Option<SessionScan> {
    let session_id = path.file_stem()?.to_str()?.to_string();
    let (head, tail, whole, created_fs, modified_fs) = read_chunks(path).ok()?;

    let mut cwd: Option<PathBuf> = None;
    let mut created: Option<i64> = None;
    let mut first_prompt: Option<String> = None;
    let mut ai_title: Option<String> = None;
    let mut custom_title: Option<String> = None;
    let mut model: Option<String> = None;
    let mut context_tokens: Option<u64> = None;
    let mut cache_ttl: Option<u64> = None;
    let mut cache_at: Option<i64> = None;
    let mut last_ts: Option<i64> = None;

    let mut visit = |line: &str, is_tail: bool| {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let ts = v.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339);
        if ts.is_some() {
            if created.is_none() {
                created = ts;
            }
            last_ts = ts;
        }
        if cwd.is_none() {
            cwd = v.get("cwd").and_then(Value::as_str).map(PathBuf::from);
        }
        match v.get("type").and_then(Value::as_str) {
            Some("user") if first_prompt.is_none() && !is_tail => first_prompt = first_prompt_text(&v),
            Some("ai-title") => ai_title = v.get("aiTitle").and_then(Value::as_str).map(str::to_string).or(ai_title.take()),
            Some("custom-title") => custom_title = v.get("customTitle").and_then(Value::as_str).map(str::to_string).or(custom_title.take()),
            Some("assistant") if !v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) => {
                if let Some(msg) = v.get("message") {
                    if let Some(m) = msg.get("model").and_then(Value::as_str).filter(|m| !m.starts_with('<')) {
                        model = Some(m.to_string());
                    }
                    if let Some(u) = msg.get("usage") {
                        let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                        let total = get("input_tokens") + get("cache_creation_input_tokens") + get("cache_read_input_tokens");
                        if total > 0 {
                            context_tokens = Some(total);
                        }
                        // Only a response that touched the cache restarted its TTL; user messages and renames don't.
                        if get("cache_creation_input_tokens") + get("cache_read_input_tokens") > 0 && ts.is_some() {
                            cache_at = ts;
                        }
                        // The last request that wrote to the cache says which TTL tier this account gets.
                        if let Some(c) = u.get("cache_creation") {
                            let n = |k: &str| c.get(k).and_then(Value::as_u64).unwrap_or(0);
                            if n("ephemeral_1h_input_tokens") > 0 {
                                cache_ttl = Some(3600);
                            } else if n("ephemeral_5m_input_tokens") > 0 {
                                cache_ttl = Some(300);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    };
    for line in head.lines() {
        visit(line, false);
    }
    if !whole {
        for line in tail.lines() {
            visit(line, true);
        }
    }

    let title = custom_title.or(ai_title).or(first_prompt);
    Some(SessionScan {
        session: ExternalSession {
            session_id,
            cwd,
            title,
            created_at: created.unwrap_or(created_fs),
            last_active: modified_fs,
            model,
            context_tokens,
            cache_ttl,
            cache_at,
            transcript: path.to_path_buf(),
        },
    })
}

/// Approximate context-window usage in 0.0..=1.0. Claude Code doesn't expose the window for the
/// model in transcripts, so assume 200k and fall back to 1M once usage exceeds it. Shown as approximate.
/// What the context bar is full at: the auto-compact threshold while auto-compact is on (the bar fills as compaction
/// nears), otherwise the model's window.
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
    fn conversation_text_keeps_what_was_said_and_drops_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_session(
            dir.path(),
            "s",
            &[
                r#"{"type":"user","message":{"content":"fix the login bug"}}"#,
                r#"{"type":"user","isMeta":true,"message":{"content":"meta"}}"#,
                r#"{"type":"user","message":{"content":"<command-name>/x</command-name>"}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Found it."},{"type":"tool_use","name":"Read"}]}}"#,
            ],
        );
        assert_eq!(conversation_text(&p), "User: fix the login bug

Claude: Found it.

");
    }

    fn write_session(dir: &Path, id: &str, lines: &[&str]) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let p = dir.join(format!("{id}.jsonl"));
        fs::write(&p, lines.join("\n")).unwrap();
        p
    }

    #[test]
    fn reads_title_cwd_model_context() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_session(
            &tmp.path().join("projects/x"),
            "11111111-1111-1111-1111-111111111111",
            &[
                r#"{"type":"mode","mode":"auto","sessionId":"s"}"#,
                r#"{"type":"user","timestamp":"2026-10-05T14:10:53.395Z","cwd":"C:\\w\\app","message":{"role":"user","content":"fix the login bug please"}}"#,
                r#"{"type":"assistant","timestamp":"2026-10-05T14:11:00.000Z","message":{"model":"claude-sonnet-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":49898}}}"#,
                "this line is not json",
                r#"{"type":"ai-title","aiTitle":"Fix login bug","sessionId":"s"}"#,
            ],
        );
        let s = read_session(&p).unwrap().session;
        assert_eq!(s.session_id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(s.title.as_deref(), Some("Fix login bug"));
        assert_eq!(s.cwd, Some(PathBuf::from(r"C:\w\app")));
        assert_eq!(s.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(s.context_tokens, Some(50000));
        assert_eq!(s.cache_at, parse_rfc3339("2026-10-05T14:11:00.000Z"));
        assert_eq!(s.created_at, 1791209453);
    }

    #[test]
    fn falls_back_to_first_prompt_and_skips_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_session(
            &tmp.path().join("projects/x"),
            "s2",
            &[
                r#"{"type":"user","isMeta":true,"cwd":"/w","message":{"content":"meta"}}"#,
                r#"{"type":"user","cwd":"/w","message":{"content":"<command-name>/clear</command-name>"}}"#,
                r#"{"type":"user","cwd":"/w","message":{"content":[{"type":"text","text":"hello   world"}]}}"#,
            ],
        );
        assert_eq!(read_session(&p).unwrap().session.title.as_deref(), Some("hello world"));
    }

    #[test]
    fn large_file_uses_head_and_tail_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("projects/x");
        fs::create_dir_all(&dir).unwrap();
        let mut text = String::new();
        text.push_str(r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","cwd":"/w","message":{"content":"start here"}}"#);
        text.push('\n');
        let filler = format!(r#"{{"type":"attachment","pad":"{}"}}"#, "x".repeat(1000));
        for _ in 0..2000 {
            text.push_str(&filler);
            text.push('\n');
        }
        text.push_str(r#"{"type":"assistant","message":{"model":"m","usage":{"input_tokens":7}}}"#);
        text.push('\n');
        text.push_str(r#"{"type":"ai-title","aiTitle":"Tail title"}"#);
        let p = dir.join("big.jsonl");
        fs::write(&p, text).unwrap();
        assert!(fs::metadata(&p).unwrap().len() > HEAD_BYTES + TAIL_BYTES);
        let s = read_session(&p).unwrap().session;
        assert_eq!(s.title.as_deref(), Some("Tail title"));
        assert_eq!(s.context_tokens, Some(7));
        assert_eq!(s.cwd, Some(PathBuf::from("/w")));
    }

    #[test]
    fn scan_all_reports_ok_flag_and_finds_transcripts() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!scan_all(tmp.path()).ok, "no projects dir => cannot conclude anything");
        write_session(&tmp.path().join("projects/a"), "id-1", &[r#"{"type":"user","cwd":"/w","message":{"content":"hi"}}"#]);
        let r = scan_all(tmp.path());
        assert!(r.ok);
        assert_eq!(r.sessions.len(), 1);
        assert!(find_transcript(tmp.path(), "id-1").is_some());
        assert!(find_transcript(tmp.path(), "nope").is_none());
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

    #[test]
    fn launch_specs_use_documented_flags() {
        let s = new_session_spec(Path::new("claude"), Path::new("/w"), "abc");
        assert_eq!(s.args, vec!["--session-id", "abc"]);
        let r = resume_spec(Path::new("claude"), Path::new("/w"), "abc");
        assert_eq!(r.args, vec!["--resume", "abc"]);
    }
}
