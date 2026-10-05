//! Claude Code integration. Strictly read-only against Claude Code's data and never touches its config.
//! Launching/resuming uses documented CLI flags (`--session-id`, `--resume`); the transcript scan is a
//! best-effort, tolerant *import* aid: every field is optional and any unparseable line is skipped.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::platform::{find_executable, home_dir};
use crate::store::{ExternalSession, RateLimit};
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

pub struct ScanResult {
    pub sessions: Vec<ExternalSession>,
    /// False when the directory couldn't be read: callers must not conclude anything is gone.
    pub ok: bool,
    pub five_hour: Option<RateLimit>,
    pub seven_day: Option<RateLimit>,
}

/// Scan every transcript under `<home>/projects/*/*.jsonl`. Meant to run on a background thread.
pub fn scan_all(home: &Path) -> ScanResult {
    let mut out = ScanResult { sessions: vec![], ok: false, five_hour: None, seven_day: None };
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
                for obs in scan.limits {
                    let slot = if obs.kind == "five_hour" { &mut out.five_hour } else { &mut out.seven_day };
                    if slot.as_ref().is_none_or(|cur| obs.limit.seen_at >= cur.seen_at) {
                        *slot = Some(obs.limit);
                    }
                }
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

pub struct LimitObs {
    pub kind: String,
    pub limit: RateLimit,
}

pub struct SessionScan {
    pub session: ExternalSession,
    pub limits: Vec<LimitObs>,
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

fn find_rate_limit(v: &Value) -> Option<&Value> {
    match v {
        Value::Object(map) => {
            if map.contains_key("rateLimitType") {
                return Some(v);
            }
            map.values().find_map(find_rate_limit)
        }
        Value::Array(a) => a.iter().find_map(find_rate_limit),
        _ => None,
    }
}

fn limit_from(obj: &Value, at: i64) -> Option<LimitObs> {
    let kind = obj.get("rateLimitType")?.as_str()?.to_string();
    if kind != "five_hour" && kind != "seven_day" {
        return None;
    }
    let status = obj.get("status").and_then(Value::as_str).unwrap_or("");
    let utilization = obj
        .get("utilization")
        .and_then(Value::as_f64)
        .map(|u| if u > 1.5 { u / 100.0 } else { u } as f32)
        .or(if status == "rejected" { Some(1.0) } else { None });
    let resets_at = obj.get("resetsAt").and_then(Value::as_i64);
    Some(LimitObs { kind, limit: RateLimit { utilization, resets_at, seen_at: at } })
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
    let mut limits: Vec<LimitObs> = Vec::new();
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
                    }
                }
            }
            _ => {}
        }
        if line.contains("rateLimitType")
            && let Some(obj) = find_rate_limit(&v)
                && let Some(obs) = limit_from(obj, ts.unwrap_or(modified_fs)) {
                    limits.push(obs);
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
            transcript: path.to_path_buf(),
        },
        limits,
    })
}

/// Approximate context-window usage in 0.0..=1.0. Claude Code doesn't expose the window for the
/// model in transcripts, so assume 200k and fall back to 1M once usage exceeds it. Shown as approximate.
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
    fn rate_limit_observations() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_session(
            &tmp.path().join("projects/x"),
            "s3",
            &[
                r#"{"type":"system","timestamp":"2026-10-05T12:00:00Z","cwd":"/w","rate_limit_info":{"status":"rejected","resetsAt":1791209400,"rateLimitType":"five_hour"}}"#,
                r#"{"type":"system","timestamp":"2026-10-05T13:00:00Z","cwd":"/w","x":{"status":"allowed_warning","utilization":0.8,"resetsAt":1791500000,"rateLimitType":"seven_day"}}"#,
            ],
        );
        let scan = read_session(&p).unwrap();
        assert_eq!(scan.limits.len(), 2);
        assert_eq!(scan.limits[0].limit.utilization, Some(1.0));
        assert_eq!(scan.limits[1].limit.utilization, Some(0.8));
        assert_eq!(scan.limits[1].kind, "seven_day");
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
