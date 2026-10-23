//! One-shot text completions through a running session's own mod, for the things Claudiu writes in the background
//! (commit messages, session titles). The mod (`mod/hooks/register.js`) answers with `$.model.complete` on the
//! session's own client, so Claudiu runs no `claude -p` of its own and makes no API call.
//!
//! The helper side is files, like the usage and ctl ones: a request goes to `<data>/ask/<record id>.json`, the mod
//! answers into `<record id>.res.json`, first `working` (it picked the request up) and then `done` or `error`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// Env var holding the request file of a session, for its mod.
pub const ASK_ENV: &str = "CLAUDIU_ASK_FILE";

/// How long the mod has to pick a request up (it looks twice a second) before it counts as not there.
const PICKUP: Duration = Duration::from_secs(8);
/// How long a picked-up request may take.
const TOTAL: Duration = Duration::from_secs(100);

pub fn dir() -> PathBuf {
    crate::datadir::data_dir().join("ask")
}

/// The request file of one session, named by Claudiu's own record id (which survives `/clear`).
pub fn request_file(record_id: &str) -> PathBuf {
    dir().join(format!("{}.json", crate::datadir::safe_file_stem(record_id)))
}

/// Ask the session's mod for a Haiku completion. Blocking (seconds); call it off the UI thread.
pub fn complete(record_id: &str, system: &str, prompt: &str) -> Result<String, String> {
    complete_at(&request_file(record_id), system, prompt, PICKUP, TOTAL)
}

#[derive(Deserialize)]
struct Reply {
    id: String,
    state: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    error: String,
}

fn complete_at(file: &Path, system: &str, prompt: &str, pickup: Duration, total: Duration) -> Result<String, String> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let id = format!("{}-{}-{}", now.as_millis(), std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let reply = file.with_extension("res.json");
    let _ = std::fs::remove_file(&reply);
    let request = serde_json::json!({
        "id": id, "model": "haiku", "system": system, "prompt": prompt,
        "deadline": now.as_secs() + total.as_secs(),
    });
    crate::datadir::write_atomic(file, &request.to_string()).map_err(|e| format!("could not write the request: {e}"))?;

    let start = Instant::now();
    let mut picked_up = false;
    loop {
        let answer = std::fs::read_to_string(&reply).ok().and_then(|t| serde_json::from_str::<Reply>(&t).ok()).filter(|r| r.id == id);
        match answer.as_ref().map(|r| r.state.as_str()) {
            Some("done") => {
                let text = answer.map(|r| r.text).unwrap_or_default();
                return if text.trim().is_empty() { Err("the model returned nothing".into()) } else { Ok(text) };
            }
            Some("error") => return Err(answer.map(|r| r.error).unwrap_or_default()),
            Some("working") => picked_up = true,
            _ => {}
        }
        let waited = start.elapsed();
        if (!picked_up && waited >= pickup) || waited >= total {
            // Nobody waits for it any more: take the request back so a late mod doesn't act on it.
            let _ = std::fs::remove_file(file);
            return Err(if picked_up {
                "the session took too long to answer".into()
            } else {
                "the session didn't pick the request up (is it still starting, or was it started without Claudiu's mod?)".into()
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thread that plays the mod: reads the request, says it is working, then answers.
    fn play_mod(file: PathBuf, answer: fn(&serde_json::Value) -> serde_json::Value) -> std::thread::JoinHandle<serde_json::Value> {
        std::thread::spawn(move || {
            let reply = file.with_extension("res.json");
            for _ in 0..200 {
                if let Ok(t) = std::fs::read_to_string(&file) {
                    let req: serde_json::Value = serde_json::from_str(&t).unwrap();
                    let id = req["id"].clone();
                    std::fs::write(&reply, serde_json::json!({ "id": id, "state": "working" }).to_string()).unwrap();
                    std::thread::sleep(Duration::from_millis(120));
                    let mut out = answer(&req);
                    out["id"] = id;
                    std::fs::write(&reply, out.to_string()).unwrap();
                    return req;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("no request came")
        })
    }

    #[test]
    fn a_request_is_answered_through_the_files() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("ask/rec.json");
        let modder = play_mod(file.clone(), |req| serde_json::json!({ "state": "done", "text": format!("re: {}", req["prompt"].as_str().unwrap()) }));
        let got = complete_at(&file, "Be brief.", "hello", Duration::from_secs(5), Duration::from_secs(10)).unwrap();
        assert_eq!(got, "re: hello");
        let req = modder.join().unwrap();
        assert_eq!((req["model"].as_str(), req["system"].as_str()), (Some("haiku"), Some("Be brief.")));
        assert!(req["deadline"].as_u64().unwrap() > 0);
    }

    #[test]
    fn the_mods_error_is_passed_on() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("rec.json");
        let modder = play_mod(file.clone(), |_| serde_json::json!({ "state": "error", "error": "The model call failed (api-error 529)." }));
        let err = complete_at(&file, "", "p", Duration::from_secs(5), Duration::from_secs(10)).unwrap_err();
        assert!(err.contains("529"), "{err}");
        modder.join().unwrap();
    }

    #[test]
    fn a_session_without_the_mod_fails_fast_and_withdraws_the_request() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("rec.json");
        let err = complete_at(&file, "", "p", Duration::from_millis(200), Duration::from_secs(10)).unwrap_err();
        assert!(err.contains("pick the request up"), "{err}");
        assert!(!file.exists());
    }

    #[test]
    fn an_old_answer_is_not_taken_for_the_new_request() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("rec.json");
        std::fs::write(file.with_extension("res.json"), r#"{"id":"old","state":"done","text":"stale"}"#).unwrap();
        assert!(complete_at(&file, "", "p", Duration::from_millis(150), Duration::from_secs(10)).is_err());
    }
}
