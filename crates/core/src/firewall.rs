//! The context firewall: keeps native tool output small before the model sees it, and keeps the full text
//! retrievable by handle.
//!
//! One rule set, one job. A huge result is cut to its head and tail with a footer naming a 12-hex handle; a file
//! the model already has, unchanged, is replaced by a pointer. The full text is stored under the handle (content
//! addressed, so the same text always gets the same handle) and `expand` returns all of it or a line range.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::event::SessionId;
use crate::store::Store;

/// Output at or under this many characters is not touched.
const MAX_CHARS: usize = 8_000;
const HEAD_LINES: usize = 60;
const TAIL_LINES: usize = 20;
/// A file read shorter than this is not worth a ledger pointer.
const LEDGER_MIN_CHARS: usize = 1_000;
const MAX_NAMES: usize = 150;
const KEPT_NAMES: usize = 100;

pub fn handle_of(text: &[u8]) -> String {
    Sha256::digest(text).iter().take(6).map(|b| format!("{b:02x}")).collect()
}

fn footer(handle: &str, elided: usize, total: usize) -> String {
    format!("[claudiu: {elided} of {total} lines kept aside, handle {handle}. expand(handle) returns them all, expand(handle, from, to) a line range.]")
}

/// Cuts `text` to its head and tail and stores the whole. Returns `None` if it is small enough to leave alone.
fn compact_text(store: &Store, session: &SessionId, ts_ms: i64, text: &str) -> Option<String> {
    if text.len() <= MAX_CHARS {
        return None;
    }
    let handle = store.put_blob(session, ts_ms, text.as_bytes()).ok()?;
    let all: Vec<&str> = text.lines().collect();
    if all.len() > HEAD_LINES + TAIL_LINES {
        let elided = all.len() - HEAD_LINES - TAIL_LINES;
        let mut out = all[..HEAD_LINES].join("\n");
        out.push('\n');
        out.push_str(&footer(&handle, elided, all.len()));
        out.push('\n');
        out.push_str(&all[all.len() - TAIL_LINES..].join("\n"));
        return Some(out);
    }
    // Few but very long lines (minified output): cut by characters instead.
    let head: String = text.chars().take(MAX_CHARS / 2).collect();
    let tail: String = text.chars().rev().take(MAX_CHARS / 8).collect::<Vec<_>>().into_iter().rev().collect();
    Some(format!("{head}\n[claudiu: the middle was kept aside, handle {handle}. expand(handle) returns it.]\n{tail}"))
}

/// What the hook should answer for a finished tool call: `Some(new tool_response)` or `None` to leave it.
pub fn compact(store: &Store, session: &SessionId, ts_ms: i64, agent: &str, tool: &str, input: &Value, response: &Value) -> Option<Value> {
    let mut out = response.clone();
    let mut changed = false;
    match tool {
        "Read" => {
            let content = response["file"]["content"].as_str()?;
            let path = input["file_path"].as_str().unwrap_or("");
            let key = format!("{path}:{}:{}", input["offset"], input["limit"]);
            let hash = handle_of(content.as_bytes());
            let seen = content.len() >= LEDGER_MIN_CHARS && store.ledger_seen(session, agent, &key, &hash, ts_ms).unwrap_or(false);
            if seen {
                let handle = store.put_blob(session, ts_ms, content.as_bytes()).ok()?;
                out["file"]["content"] = json!(format!("[claudiu: unchanged since you last read this file in this conversation ({} lines), handle {handle}. expand(handle) returns the text.]", content.lines().count()));
                changed = true;
            } else if let Some(short) = compact_text(store, session, ts_ms, content) {
                out["file"]["content"] = json!(short);
                changed = true;
            }
        }
        "Bash" => {
            for field in ["stdout", "stderr"] {
                if let Some(short) = response[field].as_str().and_then(|t| compact_text(store, session, ts_ms, t)) {
                    out[field] = json!(short);
                    changed = true;
                }
            }
        }
        "Grep" => {
            if let Some(short) = response["content"].as_str().and_then(|t| compact_text(store, session, ts_ms, t)) {
                out["content"] = json!(short);
                changed = true;
            }
            changed |= shorten_names(store, session, ts_ms, &mut out);
        }
        "Glob" => changed |= shorten_names(store, session, ts_ms, &mut out),
        _ => {}
    }
    changed.then_some(out)
}

/// A long `filenames` list keeps its first names and ends with a pointer to the rest.
fn shorten_names(store: &Store, session: &SessionId, ts_ms: i64, out: &mut Value) -> bool {
    let Some(names) = out["filenames"].as_array().filter(|n| n.len() > MAX_NAMES).cloned() else { return false };
    let joined = names.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n");
    let Ok(handle) = store.put_blob(session, ts_ms, joined.as_bytes()) else { return false };
    let mut kept: Vec<Value> = names[..KEPT_NAMES].to_vec();
    kept.push(json!(footer(&handle, names.len() - KEPT_NAMES, names.len())));
    out["filenames"] = Value::Array(kept);
    true
}

/// `expand`: all of the text under `handle`, or lines `from..=to` (1-based).
pub fn expand(store: &Store, handle: &str, from: Option<u64>, to: Option<u64>) -> String {
    let Ok(Some(data)) = store.blob(handle) else {
        return "No such handle; it may have expired after 14 days.".into();
    };
    let text = String::from_utf8_lossy(&data);
    if from.is_none() && to.is_none() {
        return text.into_owned();
    }
    let (from, to) = (from.unwrap_or(1).max(1) as usize, to.map_or(usize::MAX, |t| t as usize));
    text.lines().enumerate().filter(|(i, _)| (from..=to).contains(&(i + 1))).map(|(_, l)| l).collect::<Vec<_>>().join("\n")
}
