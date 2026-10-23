//! Requests from a Claude Code session to Claudiu, made through the MCP helper
//! (`claudiu --mcp-title`, see `claude/mcp.rs`). The helper is a separate process, so the two talk through files:
//! it drops `bridge/req/<id>.json` and waits for `bridge/res/<id>.json`, which Claudiu writes on its next tick.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long a request stays valid: the helper gives up after this, and Claudiu ignores older files.
pub const PATIENCE: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    /// `code:<record id>`.
    pub from: String,
    pub tool: String,
    pub args: Value,
    /// Unix seconds after which nobody is waiting for the answer any more.
    pub deadline: i64,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    ok: bool,
    text: String,
}

fn dir(base: &Path, kind: &str) -> PathBuf {
    base.join("bridge").join(kind)
}

fn stem(id: &str) -> String {
    crate::datadir::safe_file_stem(id)
}

/// Helper side: file a request and block until Claudiu answers (or `PATIENCE` runs out).
pub fn ask(from: &str, tool: &str, args: &Value) -> Result<String, String> {
    ask_in(&crate::datadir::data_dir(), from, tool, args)
}

fn ask_in(base: &Path, from: &str, tool: &str, args: &Value) -> Result<String, String> {
    let id = crate::store::new_id();
    let req = Request { id: id.clone(), from: from.into(), tool: tool.into(), args: args.clone(), deadline: crate::store::now() + PATIENCE.as_secs() as i64 };
    let (req_path, res_path) = (dir(base, "req").join(format!("{}.json", stem(&id))), dir(base, "res").join(format!("{}.json", stem(&id))));
    let body = serde_json::to_string(&req).map_err(|e| e.to_string())?;
    crate::datadir::write_atomic(&req_path, &body).map_err(|e| format!("could not reach Claudiu: {e}"))?;
    let start = Instant::now();
    while start.elapsed() < PATIENCE {
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(text) = std::fs::read_to_string(&res_path) {
            let _ = std::fs::remove_file(&res_path);
            let reply: Reply = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            return if reply.ok { Ok(reply.text) } else { Err(reply.text) };
        }
    }
    let _ = std::fs::remove_file(&req_path);
    Err("Claudiu got no answer from the user in time.".into())
}

/// Claudiu side: every request filed since the last call. Files are consumed; expired or unreadable ones are dropped.
pub fn take_requests() -> Vec<Request> {
    take_requests_in(&crate::datadir::data_dir())
}

fn take_requests_in(base: &Path) -> Vec<Request> {
    prune_answers(base);
    let Ok(entries) = std::fs::read_dir(dir(base, "req")) else { return vec![] };
    let now = crate::store::now();
    let mut out: Vec<Request> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            let _ = std::fs::remove_file(e.path());
            serde_json::from_str::<Request>(&text).ok().filter(|r| r.deadline > now)
        })
        .collect();
    out.sort_by_key(|r| r.deadline);
    out
}

/// Answers nobody collected (the asker gave up or died) are deleted after a while.
fn prune_answers(base: &Path) {
    for e in std::fs::read_dir(dir(base, "res")).into_iter().flatten().flatten() {
        if e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > PATIENCE * 2) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Claudiu side: answer a request (`ok = false` makes the tool report an error to the model).
pub fn respond(id: &str, ok: bool, text: impl Into<String>) {
    respond_in(&crate::datadir::data_dir(), id, ok, text);
}

fn respond_in(base: &Path, id: &str, ok: bool, text: impl Into<String>) {
    let body = serde_json::to_string(&Reply { ok, text: text.into() }).unwrap_or_default();
    let _ = crate::datadir::write_atomic(&dir(base, "res").join(format!("{}.json", stem(id))), &body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_taken_once_answered_and_expire() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        let asker = {
            let base = base.clone();
            std::thread::spawn(move || ask_in(&base, "code:c1", "list_code_sessions", &serde_json::json!({})))
        };
        let mut taken = vec![];
        for _ in 0..50 {
            taken = take_requests_in(&base);
            if !taken.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(taken.len(), 1);
        assert_eq!((taken[0].from.as_str(), taken[0].tool.as_str()), ("code:c1", "list_code_sessions"));
        assert!(take_requests_in(&base).is_empty(), "a request is handed out once");
        respond_in(&base, &taken[0].id, true, "two sessions");
        assert_eq!(asker.join().unwrap(), Ok("two sessions".to_string()));

        let asker = {
            let base = base.clone();
            std::thread::spawn(move || ask_in(&base, "code:s1", "read_code_session", &serde_json::json!({"id": "x"})))
        };
        let req = loop {
            if let Some(r) = take_requests_in(&base).pop() {
                break r;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        respond_in(&base, &req.id, false, "The user declined.");
        assert_eq!(asker.join().unwrap(), Err("The user declined.".to_string()));

        // An answer nobody collected is cleaned up once it is old.
        respond_in(&base, "stale", true, "x");
        respond_in(&base, "fresh", true, "x");
        let old = std::time::SystemTime::now() - PATIENCE * 3;
        std::fs::File::options().write(true).open(dir(&base, "res").join("stale.json")).unwrap().set_modified(old).unwrap();
        take_requests_in(&base);
        assert!(!dir(&base, "res").join("stale.json").exists() && dir(&base, "res").join("fresh.json").exists());

        // A request nobody is waiting for any more is dropped.
        let old = Request { id: "old".into(), from: "code:c".into(), tool: "list_code_sessions".into(), args: Value::Null, deadline: 1 };
        crate::datadir::write_atomic(&dir(&base, "req").join("old.json"), &serde_json::to_string(&old).unwrap()).unwrap();
        assert!(take_requests_in(&base).is_empty());
        assert!(!dir(&base, "req").join("old.json").exists());
    }
}
