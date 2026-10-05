//! Tab titles and model/effort switches chosen by Claude itself, through a tiny MCP server (`claudiu --mcp-title`).
//!
//! Like the status-line tee, it is wired in *per process*: Claudiu starts each Claude session with
//! `--mcp-config <file>` (and `--allowedTools` for the title tool so it never prompts). The server's
//! `instructions` ask Claude to name the tab now and then; the tool just drops the title into a file that
//! Claudiu picks up on its next tick. `set_model` works the same way: it writes a request file, and Claudiu
//! types `/model` / `/effort` into that session once it is idle. It is not pre-allowed, so Claude Code asks
//! the user first.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub const FLAG: &str = "--mcp-title";
pub const SESSION_ENV: &str = "CLAUDIU_SESSION";
/// `mcp__<server>__<tool>`: allowed per process so renaming the tab never raises a permission prompt.
pub const ALLOWED_TOOL: &str = "mcp__claudiu__set_tab_title";

const INSTRUCTIONS: &str = "You are running inside Claudiu, which shows a tab title for this session. Call the \
set_tab_title tool with a short title (2-5 words, no quotes) once you understand the user's first request. After \
that, call it again only when the current task or scope changes and the title no longer fits. Also call it \
whenever the user asks you to rename or retitle this session or tab (use the name they gave). Don't call it again \
with the same or a near-identical title, and never twice in a row for the same reason (if the user ran /rename \
themselves, the title is already set; leave it). Don't mention the tool unless the user asked for a rename. \
Never switch models on your own. Before calling set_model, ask the user with the AskUserQuestion tool which \
model and effort they want, unless they just named them explicitly. Claudiu types /model and /effort into the \
session once your reply has ended, so finish your turn right after calling it.";

fn titles_dir() -> PathBuf {
    crate::statusline::data_dir().join("titles")
}

fn requests_dir() -> PathBuf {
    crate::statusline::data_dir().join("model")
}

/// `/model` and `/effort` lines for a request. The lines are typed into a terminal, so only plain tokens pass.
fn slash_lines(req: &Value) -> Vec<String> {
    let token = |key: &str, ok: fn(char) -> bool| {
        req[key].as_str().map(str::trim).filter(|v| !v.is_empty() && v.len() <= 64 && v.chars().all(ok))
    };
    let model = token("model", |c| c.is_ascii_alphanumeric() || "-._[]".contains(c));
    let effort = token("effort", |c| c.is_ascii_alphabetic());
    model.map(|m| format!("/model {m}")).into_iter().chain(effort.map(|e| format!("/effort {e}"))).collect()
}

fn config_dir() -> PathBuf {
    crate::statusline::data_dir().join("mcp")
}

/// Collapse whitespace, drop control characters, cap the length. `None` when nothing is left.
fn clean_title(raw: &str) -> Option<String> {
    let t: String = raw.chars().filter(|c| !c.is_control() || c.is_whitespace()).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(60).collect();
    (!t.is_empty()).then_some(t)
}

/// Entry point of `claudiu --mcp-title`: newline-delimited JSON-RPC over stdio.
pub fn run() -> i32 {
    let session = std::env::var(SESSION_ENV).unwrap_or_default();
    serve(std::io::stdin().lock(), std::io::stdout().lock(), &session, &titles_dir(), &requests_dir());
    0
}

fn serve(input: impl BufRead, mut out: impl Write, session: &str, titles: &Path, requests: &Path) {
    for line in input.lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        // Notifications (no id) and responses (no method) need no answer.
        let (Some(id), Some(method)) = (msg.get("id"), msg.get("method").and_then(Value::as_str)) else { continue };
        let reply = match method {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "claudiu", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": [{
                "name": "set_tab_title",
                "description": "Set this session's tab title in Claudiu (2-5 words). Call after the first request, when the current task or scope changes, and whenever the user asks to rename the session or tab. Don't repeat an unchanged title.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "title": { "type": "string", "description": "Short tab title, 2-5 words" } },
                    "required": ["title"],
                },
            }, {
                "name": "set_model",
                "description": "Switch this session's model and/or effort. Claudiu types /model and /effort into the session once your reply has ended, so call it last and end your turn. Always ask the user first with the AskUserQuestion tool (which model, which effort) unless they just named them explicitly; never switch on your own. Claude Code saves the choice as the user's default.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "model": { "type": "string", "description": "Model alias (sonnet, opus, haiku) or full model id" },
                        "effort": { "type": "string", "description": "Effort level: low, medium, high, xhigh, max or auto" },
                    },
                },
            }]})),
            "tools/call" => Ok(match msg["params"]["name"].as_str() {
                Some("set_tab_title") => set_title(&msg["params"], session, titles),
                Some("set_model") => set_model(&msg["params"], session, requests),
                _ => reply("unknown tool", true),
            }),
            _ => Err(json!({ "code": -32601, "message": "method not found" })),
        };
        let body = match reply {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        let _ = writeln!(out, "{body}");
        let _ = out.flush();
    }
}

fn reply(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn set_model(params: &Value, session: &str, dir: &Path) -> Value {
    let args = &params["arguments"];
    let given = ["model", "effort"].iter().filter(|k| !args[**k].is_null()).count();
    if given == 0 || slash_lines(args).len() != given {
        return reply("give a model and/or an effort: letters, digits and - . _ [ ] only", true);
    }
    if session.is_empty() {
        return reply("not running inside Claudiu", true);
    }
    let body = json!({ "model": args["model"], "effort": args["effort"] }).to_string();
    match crate::statusline::write_atomic(&dir.join(format!("{}.json", crate::statusline::safe_file_stem(session))), &body) {
        Ok(()) => reply("Queued. It is applied once your reply ends: finish your turn now.", false),
        Err(e) => reply(&format!("could not save request: {e}"), true),
    }
}

/// Requests for these sessions (file stems) as the lines to type; the request files are consumed.
pub fn take_model_requests(stems: &[String]) -> Vec<(String, Vec<String>)> {
    take_from(&requests_dir(), stems)
}

fn take_from(dir: &Path, stems: &[String]) -> Vec<(String, Vec<String>)> {
    stems
        .iter()
        .filter_map(|stem| {
            let path = dir.join(format!("{stem}.json"));
            let text = std::fs::read_to_string(&path).ok()?;
            let _ = std::fs::remove_file(&path);
            let lines = slash_lines(&serde_json::from_str(&text).ok()?);
            (!lines.is_empty()).then(|| (stem.clone(), lines))
        })
        .collect()
}

fn set_title(params: &Value, session: &str, dir: &Path) -> Value {
    let Some(title) = params["arguments"]["title"].as_str().and_then(clean_title) else {
        return reply("title must be a non-empty string", true);
    };
    if session.is_empty() {
        return reply("not running inside Claudiu", true);
    }
    match crate::statusline::write_atomic(&dir.join(format!("{}.txt", crate::statusline::safe_file_stem(session))), &title) {
        Ok(()) => reply("Tab renamed.", false),
        Err(e) => reply(&format!("could not save title: {e}"), true),
    }
}

/// Write the per-session MCP config and return its path (for `--mcp-config`).
pub fn prepare(claude_session_id: &str) -> std::io::Result<PathBuf> {
    let config = json!({ "mcpServers": { "claudiu": {
        "command": std::env::current_exe()?.to_string_lossy(),
        "args": [FLAG],
        "env": { SESSION_ENV: claude_session_id },
    }}});
    // A request left over from an earlier run must not be applied to this one.
    let _ = std::fs::remove_file(requests_dir().join(format!("{}.json", crate::statusline::safe_file_stem(claude_session_id))));
    let path = config_dir().join(format!("{}.json", crate::statusline::safe_file_stem(claude_session_id)));
    crate::statusline::write_atomic(&path, &config.to_string())?;
    Ok(path)
}

/// `(claude session id stem, title, written at)` for every title Claude has set. The timestamp changes on every
/// call of the tool, even with an unchanged title, which is how a `/clear` (new Claude session) is noticed.
pub fn read_titles() -> Vec<(String, String, Option<std::time::SystemTime>)> {
    let Ok(entries) = std::fs::read_dir(titles_dir()) else { return vec![] };
    entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("txt"))
        .filter_map(|e| {
            let stem = e.path().file_stem()?.to_string_lossy().into_owned();
            let title = clean_title(&std::fs::read_to_string(e.path()).ok()?)?;
            Some((stem, title, e.metadata().and_then(|m| m.modified()).ok()))
        })
        .collect()
}

/// Directories `statusline::cleanup` prunes: `[per-session MCP configs, titles, model requests]`
/// (titles also live in the store; requests are normally consumed right away).
pub fn cleanup_dirs() -> [PathBuf; 3] {
    [config_dir(), titles_dir(), requests_dir()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn talk(requests: &[Value], session: &str, dir: &Path) -> Vec<Value> {
        let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, session, dir, dir);
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn handshake_lists_the_tool_and_set_title_writes_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let replies = talk(
            &[
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"set_tab_title","arguments":{"title":"  Fix\n the   build  "}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"nope"}),
            ],
            "abc-123",
            tmp.path(),
        );
        assert_eq!(replies.len(), 4, "the notification gets no reply");
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        assert!(replies[0]["result"]["instructions"].as_str().unwrap().contains("set_tab_title"));
        assert_eq!(replies[1]["result"]["tools"][0]["name"], "set_tab_title");
        assert_eq!(replies[2]["result"]["isError"], false);
        assert_eq!(std::fs::read_to_string(tmp.path().join("abc-123.txt")).unwrap(), "Fix the build");
        assert_eq!(replies[3]["error"]["code"], -32601);
    }

    #[test]
    fn set_model_queues_typed_lines_and_rejects_anything_else() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |args: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"set_model","arguments":args}});
        let r = talk(
            &[
                call(json!({"model":"opus[1m]","effort":"high"})),
                call(json!({})),
                call(json!({"model":"sonnet\r/exit"})),
                call(json!({"effort":"high now"})),
            ],
            "abc-123",
            tmp.path(),
        );
        assert_eq!(r[0]["result"]["isError"], false);
        assert!([1, 2, 3].iter().all(|&i| r[i]["result"]["isError"] == true));
        let got = take_from(tmp.path(), &["abc-123".to_string(), "other".to_string()]);
        // The last valid request wins; the rejected ones never touched the file.
        assert_eq!(got, vec![("abc-123".to_string(), vec!["/model opus[1m]".to_string(), "/effort high".to_string()])]);
        assert!(take_from(tmp.path(), &["abc-123".to_string()]).is_empty(), "the request is consumed");
        let list = talk(&[json!({"jsonrpc":"2.0","id":9,"method":"tools/list"})], "s", tmp.path());
        assert_eq!(list[0]["result"]["tools"][1]["name"], "set_model");
    }

    #[test]
    fn bad_titles_are_rejected_and_long_ones_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |args: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"set_tab_title","arguments":args}});
        let r = talk(&[call(json!({"title":"   "})), call(json!({})), call(json!({"title":"x".repeat(200)}))], "s", tmp.path());
        assert_eq!(r[0]["result"]["isError"], true);
        assert_eq!(r[1]["result"]["isError"], true);
        assert_eq!(std::fs::read_to_string(tmp.path().join("s.txt")).unwrap().len(), 60);
        assert_eq!(talk(&[call(json!({"title":"hi"}))], "", tmp.path())[0]["result"]["isError"], true);
    }
}
