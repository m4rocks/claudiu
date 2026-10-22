//! Tab titles and model/effort switches chosen by Claude itself, through a tiny MCP server (`claudiu --mcp-title`).
//!
//! Like the status-line tee, it is wired in *per process*: Claudiu starts each Claude session with
//! `--mcp-config <file>` (and `--allowedTools` for the title tool so it never prompts). The server's
//! `instructions` ask Claude to name the tab now and then; the tool just drops the title into a file that
//! Claudiu picks up on its next tick. `set_model` is declared here only so Claude sees it: the per-process mod
//! (`mod/`, loaded with `--plugin-dir`) answers the call in-process, so the server only ever sees it when the
//! mod isn't active, and then reports that.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::store::Attached;

pub const FLAG: &str = "--mcp-title";
pub const SESSION_ENV: &str = "CLAUDIU_SESSION";
/// `code` (a Claude Code session in a terminal) or `chat` (a native chat); decides which tools the server offers.
pub const CALLER_ENV: &str = "CLAUDIU_CALLER";
/// Claudiu's own id of the session or chat. Unlike Claude's session id it survives `/clear`.
pub const RECORD_ENV: &str = "CLAUDIU_RECORD";

/// Tools the model uses to tell Claudiu something about itself; none of them reads or starts another session.
/// A chat gets `attach_github` and `link_session` too.
const CODE_TOOLS: [&str; 3] = ["set_tab_title", "attach_github", "link_session"];
const CHAT_TOOLS: [&str; 2] = ["attach_github", "link_session"];

/// Tools that reach other sessions. Each one makes Claudiu ask the user before it does anything (`bridge.rs`).
const BRIDGE_TOOLS: [&str; 7] = ["list_chats", "read_chat", "list_code_sessions", "read_code_session", "search_sessions", "start_code_session", "start_chat"];

/// `mcp__<server>__<tool>` names allowed per process so Claude Code (or the chat adapter) adds no prompt of its own:
/// renaming the tab is harmless, and the cross-session tools are gated by Claudiu's own confirmation instead.
pub fn allowed_tools(caller: &str) -> Vec<String> {
    let own: &[&str] = if caller == "code" { &CODE_TOOLS } else { &CHAT_TOOLS };
    own.iter().copied().chain(BRIDGE_TOOLS).map(|t| format!("mcp__claudiu__{t}")).collect()
}

const BRIDGE_INSTRUCTIONS: &str = " Claudiu also has tools that reach your other sessions: list_chats, read_chat, list_code_sessions, \
read_code_session, search_sessions, start_code_session (opens a new Claude Code session) and start_chat (opens a new \
chat). Each one makes Claudiu ask the user to confirm first. Use them only when the user asks you to \
look at, hand work to or continue in another session; if the user declines, accept it and don't ask again.";

/// Both callers: the sidebar shows linked sessions and chats together.
const LINK_INSTRUCTIONS: &str = " Claudiu shows related sessions and chats together. When this work continues, builds on or was \
prompted by another chat or Claude Code session (the user says so, or you recognise it), find it with search_sessions \
and call link_session with its id, once per pair. link_session is silent and changes nothing else.";

const CHAT_INSTRUCTIONS: &str = "You are a chat running inside Claudiu.";

/// Both callers: what makes the model call `attach_github`.
const GITHUB_INSTRUCTIONS: &str = " GitHub: the moment an issue or pull request of this project comes up, call attach_github (kind issue or pull, the number, and the title if you know it) before you carry on, once per item. That means: the user mentions one (#123, \"issue 52\", a github.com .../issues/N or .../pull/N link), you read or search one with gh or the GitHub tools, you open or push a pull request, or a commit or branch name refers to one. It is silent, costs nothing and shows the item in the user's sidebar, so prefer calling it to skipping it. If its schema is not loaded yet, load it first with ToolSearch (select:mcp__claudiu__attach_github).";

const INSTRUCTIONS: &str = "You are running inside Claudiu, which shows a tab title for this session. Call the \
set_tab_title tool with a short title (2-5 words, no quotes) once you understand the user's first request. After \
that, call it again only when the current task or scope changes and the title no longer fits. Also call it \
whenever the user asks you to rename or retitle this session or tab (use the name they gave). Don't call it again \
with the same or a near-identical title, and never twice in a row for the same reason (if the user ran /rename \
themselves, the title is already set; leave it). Don't mention the tool unless the user asked for a rename.\
 Never switch models on your own. Before calling set_model, ask the user with the AskUserQuestion tool which \
model and effort they want, unless they just named them explicitly. The switch applies from your next request; \
after calling set_model, end your turn and ask the user to confirm before continuing, so the session itself switches.";

fn titles_dir() -> PathBuf {
    crate::datadir::data_dir().join("titles")
}

/// Where Claudiu puts the Claude Code session name it wants for a session; the mod (`CLAUDIU_NAME_FILE`)
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

fn attach_dir(data: &Path) -> PathBuf {
    data.join("attach")
}

fn config_dir() -> PathBuf {
    crate::datadir::data_dir().join("mcp")
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
    let caller = std::env::var(CALLER_ENV).unwrap_or_else(|_| "code".into());
    let record = std::env::var(RECORD_ENV).unwrap_or_else(|_| session.clone());
    let from = format!("{caller}:{record}");
    serve(std::io::stdin().lock(), std::io::stdout().lock(), &caller, &session, &record, &crate::datadir::data_dir(), |tool, args| crate::bridge::ask(&from, tool, args));
    0
}

/// `ask` carries a cross-session request to Claudiu and waits for the user's answer.
fn serve(input: impl BufRead, mut out: impl Write, caller: &str, session: &str, record: &str, data: &Path, ask: impl Fn(&str, &Value) -> Result<String, String>) {
    let chat = caller == "chat";
    for line in input.lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        // Notifications (no id) and responses (no method) need no answer.
        let (Some(id), Some(method)) = (msg.get("id"), msg.get("method").and_then(Value::as_str)) else { continue };
        let reply = match method {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "claudiu", "version": env!("CARGO_PKG_VERSION") },
                "instructions": if chat { format!("{CHAT_INSTRUCTIONS}{GITHUB_INSTRUCTIONS}{LINK_INSTRUCTIONS}{BRIDGE_INSTRUCTIONS}") } else { format!("{INSTRUCTIONS}{GITHUB_INSTRUCTIONS}{LINK_INSTRUCTIONS}{BRIDGE_INSTRUCTIONS}") },
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": own_tools(chat).into_iter().chain(bridge_tools()).collect::<Vec<_>>() })),
            "tools/call" => Ok(match msg["params"]["name"].as_str() {
                Some("set_tab_title") if !chat => set_title(&msg["params"], session, &data.join("titles")),
                Some("attach_github") => attach_github(&msg["params"], record, &attach_dir(data)),
                Some("link_session") => link_session(&msg["params"], record, &attach_dir(data)),
                Some("set_model") if !chat => reply("Model switching isn't available in this session (Claudiu's mod isn't loaded).", true),
                Some(tool) if BRIDGE_TOOLS.contains(&tool) => match ask(tool, &msg["params"]["arguments"]) {
                    Ok(text) => reply(&text, false),
                    Err(text) => reply(&text, true),
                },
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

/// Tab title and model switch: only for terminal sessions (a chat names itself and has its own model picker).
fn own_tools(chat: bool) -> Vec<Value> {
    let link = json!({
        "name": "link_session",
        "description": "Link this session or chat to another one, so Claudiu shows them together in the sidebar. Use it when this work continues, builds on or was prompted by another chat or Claude Code session. Find the id with search_sessions. Silent; once per pair.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": { "type": "string", "description": "The id of the other session or chat, from search_sessions" } },
            "required": ["id"],
        },
    });
    let attach = json!({
        "name": "attach_github",
        "description": "Show a GitHub issue or pull request of this session's project in Claudiu's sidebar. Call it when the user names one or you start working on one; once per item. It changes nothing on GitHub.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["issue", "pull"], "description": "issue or pull (a pull request)" },
                "number": { "type": "integer", "description": "The issue or pull request number" },
                "title": { "type": "string", "description": "Its title, if you know it (optional)" },
            },
            "required": ["kind", "number"],
        },
    });
    if chat {
        return vec![attach, link];
    }
    vec![json!({
                "name": "set_tab_title",
                "description": "Set this session's tab title in Claudiu (2-5 words). Call after the first request, when the current task or scope changes, and whenever the user asks to rename the session or tab. Don't repeat an unchanged title.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "title": { "type": "string", "description": "Short tab title, 2-5 words" } },
                    "required": ["title"],
                },
            }), attach, json!({
                "name": "set_model",
                "description": "Switch this session's model and/or effort from your next request on, even mid-turn. Always ask the user first with the AskUserQuestion tool (which model, which effort) unless they just named them explicitly; never switch on your own. Claude Code also keeps the choice as the user's default model/effort.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "model": { "type": "string", "description": "Model alias (sonnet, opus, haiku) or full model id" },
                        "effort": { "type": "string", "description": "Effort level: low, medium, high, xhigh, max or auto" },
                    },
                },
            }), link]
}

const CONFIRM: &str = " Claudiu asks the user to confirm first, so only call it when the user asked for this.";

fn bridge_tools() -> Vec<Value> {
    let tool = |name: &str, description: String, properties: Value| json!({ "name": name, "description": description, "inputSchema": { "type": "object", "properties": properties } });
    let id = |what: &str| json!({ "id": { "type": "string", "description": format!("The id from {what}") } });
    let target = json!({
        "prompt": { "type": "string", "description": "First message for the new session (optional)" },
        "folder": { "type": "string", "description": "Project name or folder path (optional)" },
    });
    vec![
        tool("list_chats", format!("List the user's chats in Claudiu (id, title, project, whether it is open, when it was created and last updated).{CONFIRM}"), json!({})),
        tool(
            "read_chat",
            format!("Read the latest messages of a chat that is open in Claudiu.{CONFIRM}"),
            json!({ "id": id("list_chats")["id"], "messages": { "type": "integer", "description": "How many recent messages (default 30)" } }),
        ),
        tool("list_code_sessions", format!("List the user's Claude Code sessions in Claudiu (id, title, folder, whether it is running, when it was created and last updated).{CONFIRM}"), json!({})),
        tool(
            "read_code_session",
            format!("Read the latest terminal output of a Claude Code session that is running in Claudiu.{CONFIRM}"),
            json!({ "id": id("list_code_sessions")["id"], "lines": { "type": "integer", "description": "How many recent lines (default 120)" } }),
        ),
        tool(
            "search_sessions",
            format!("Search the user's chats and Claude Code sessions by words in their title, project or folder. Returns kind, id, title, project and created/updated times of the best matches; pass an id to link_session.{CONFIRM}"),
            json!({ "query": { "type": "string", "description": "Words to look for (optional: without it, the most recent)" } }),
        ),
        tool("start_code_session", format!("Open a new Claude Code session, optionally with a first prompt.{CONFIRM}"), target.clone()),
        tool("start_chat", format!("Open a new chat, optionally with a first message. Without a folder it is not tied to a project.{CONFIRM}"), target),
    ]
}

fn reply(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn set_title(params: &Value, session: &str, dir: &Path) -> Value {
    let Some(title) = params["arguments"]["title"].as_str().and_then(clean_title) else {
        return reply("title must be a non-empty string", true);
    };
    if session.is_empty() {
        return reply("not running inside Claudiu", true);
    }
    match crate::datadir::write_atomic(&dir.join(format!("{}.txt", crate::datadir::safe_file_stem(session))), &title) {
        Ok(()) => reply("Tab renamed.", false),
        Err(e) => reply(&format!("could not save title: {e}"), true),
    }
}

/// Remember an issue or pull request for the session: one JSON line per call, picked up by `read_attached`.
/// Keyed by Claudiu's own record id, which (unlike Claude's session id) survives `/clear`.
fn attach_github(params: &Value, record: &str, dir: &Path) -> Value {
    let args = &params["arguments"];
    let pull = match args["kind"].as_str() {
        Some("issue") => false,
        Some("pull") => true,
        _ => return reply("kind must be \"issue\" or \"pull\"", true),
    };
    let Some(number) = args["number"].as_u64().and_then(|n| u32::try_from(n).ok()).filter(|&n| n > 0) else {
        return reply("number must be a positive integer", true);
    };
    if record.is_empty() {
        return reply("not running inside Claudiu", true);
    }
    let title = args["title"].as_str().and_then(clean_title);
    match append_line(dir, record, &json!({ "pull": pull, "number": number, "title": title })) {
        Ok(()) => reply(&format!("{} #{number} attached.", if pull { "Pull request" } else { "Issue" }), false),
        Err(e) => reply(&format!("could not attach: {e}"), true),
    }
}

/// One JSON line for Claudiu to pick up, in the file of the calling session or chat.
fn append_line(dir: &Path, record: &str, line: &Value) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.jsonl", crate::datadir::safe_file_stem(record)));
    writeln!(std::fs::OpenOptions::new().create(true).append(true).open(path)?, "{line}")
}

/// Ask Claudiu to show this session or chat together with another one. Claudiu checks that the other exists.
fn link_session(params: &Value, record: &str, dir: &Path) -> Value {
    let id = params["arguments"]["id"].as_str().map(str::trim).filter(|i| !i.is_empty() && i.len() <= 64 && i.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    let Some(id) = id else { return reply("id must be a session or chat id from search_sessions", true) };
    if record.is_empty() {
        return reply("not running inside Claudiu", true);
    }
    if id == record {
        return reply("that is this session itself", true);
    }
    match append_line(dir, record, &json!({ "link": id })) {
        Ok(()) => reply("Linked.", false),
        Err(e) => reply(&format!("could not link: {e}"), true),
    }
}

/// Write the per-session MCP config and return its path (for `--mcp-config`).
pub fn prepare(claude_session_id: &str, record_id: &str) -> std::io::Result<PathBuf> {
    let config = json!({ "mcpServers": { "claudiu": {
        "command": std::env::current_exe()?.to_string_lossy(),
        "args": [FLAG],
        "env": { SESSION_ENV: claude_session_id, CALLER_ENV: "code", RECORD_ENV: record_id },
    }}});
    let path = config_dir().join(format!("{}.json", crate::datadir::safe_file_stem(claude_session_id)));
    crate::datadir::write_atomic(&path, &config.to_string())?;
    Ok(path)
}

/// The same server for a chat, in the shape ACP's `session/new` takes (`mcpServers`).
pub fn chat_servers(chat_id: &str) -> Value {
    let Ok(exe) = std::env::current_exe() else { return json!([]) };
    json!([{
        "name": "claudiu",
        "command": exe.to_string_lossy(),
        "args": [FLAG],
        "env": [{ "name": SESSION_ENV, "value": chat_id }, { "name": CALLER_ENV, "value": "chat" }, { "name": RECORD_ENV, "value": chat_id }],
    }])
}

/// The mod that answers this server's tools in-process (see `mod/`), written to the data dir and loaded per
/// process through `--plugin-dir`. Files are only rewritten when they changed: a rewrite reloads running sessions' mods.
pub fn prepare_mod() -> std::io::Result<PathBuf> {
    let dir = crate::datadir::data_dir().join("mod");
    for (rel, text) in [
        (".claude-plugin/plugin.json", include_str!("../mod/.claude-plugin/plugin.json")),
        ("hooks/hooks.json", include_str!("../mod/hooks/hooks.json")),
        ("hooks/register.js", include_str!("../mod/hooks/register.js")),
    ] {
        let path = dir.join(rel);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            crate::datadir::write_atomic(&path, text)?;
        }
    }
    Ok(dir)
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

/// What a session or chat asked Claudiu to remember about itself.
#[derive(Debug, PartialEq)]
pub enum Attachment {
    Github(Attached),
    /// The id of another session or chat.
    Link(String),
}

/// `(record id stem, item)` for everything attached since the last call. Each file is renamed before it is
/// read, so a line written meanwhile lands in a fresh file instead of being lost.
pub fn read_attached() -> Vec<(String, Attachment)> {
    let dir = attach_dir(&crate::datadir::data_dir());
    let Ok(entries) = std::fs::read_dir(&dir) else { return vec![] };
    let mut out = vec![];
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        let taken = path.with_extension("taken");
        if std::fs::rename(&path, &taken).is_err() {
            continue;
        }
        let text = std::fs::read_to_string(&taken).unwrap_or_default();
        let _ = std::fs::remove_file(&taken);
        out.extend(text.lines().filter_map(parse_attachment).map(|a| (stem.clone(), a)));
    }
    out
}

fn parse_attachment(line: &str) -> Option<Attachment> {
    let v: Value = serde_json::from_str(line).ok()?;
    match v.get("link").and_then(Value::as_str) {
        Some(id) => Some(Attachment::Link(id.to_string())),
        None => serde_json::from_value(v).ok().map(Attachment::Github),
    }
}

/// Directories `datadir::cleanup` prunes: `[per-session MCP configs, titles, names, attached items]` (titles also live in the store).
pub fn cleanup_dirs() -> [PathBuf; 4] {
    let data = crate::datadir::data_dir();
    [config_dir(), titles_dir(), data.join("names"), attach_dir(&data)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn talk_as(caller: &str, requests: &[Value], session: &str, dir: &Path, ask: impl Fn(&str, &Value) -> Result<String, String>) -> Vec<Value> {
        let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, caller, session, session, dir, ask);
        String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn talk(requests: &[Value], session: &str, dir: &Path) -> Vec<Value> {
        talk_as("code", requests, session, dir, |_, _| Err("no".into()))
    }

    #[test]
    fn chats_only_see_the_cross_session_tools_and_every_call_is_forwarded_to_the_user() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |name: &str| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{"id":"x"}}});
        let r = talk_as(
            "chat",
            &[json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}), call("read_code_session"), call("set_tab_title"), call("start_chat")],
            "c1",
            tmp.path(),
            |tool, args| if tool == "start_chat" { Err("The user declined.".into()) } else { Ok(format!("{tool} {}", args["id"])) },
        );
        let names: Vec<_> = r[0]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["attach_github", "link_session", "list_chats", "read_chat", "list_code_sessions", "read_code_session", "search_sessions", "start_code_session", "start_chat"]);
        assert_eq!(r[1]["result"]["content"][0]["text"], "read_code_session \"x\"");
        assert_eq!(r[1]["result"]["isError"], false);
        assert_eq!(r[2]["result"]["isError"], true, "a chat cannot set a terminal tab title");
        assert_eq!(r[3]["result"]["content"][0]["text"], "The user declined.");
        assert_eq!(r[3]["result"]["isError"], true);
        assert!(allowed_tools("chat").iter().all(|t| !t.ends_with("set_tab_title")));
        assert_eq!(allowed_tools("code").len(), BRIDGE_TOOLS.len() + CODE_TOOLS.len());
        assert!(allowed_tools("chat").contains(&"mcp__claudiu__link_session".to_string()));
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
        assert_eq!(std::fs::read_to_string(tmp.path().join("titles/abc-123.txt")).unwrap(), "Fix the build");
        assert_eq!(replies[3]["error"]["code"], -32601);
    }

    #[test]
    fn set_model_is_listed_but_errors_when_the_mod_did_not_answer() {
        let tmp = tempfile::tempdir().unwrap();
        let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"set_model","arguments":{"model":"opus"}}});
        let r = talk(&[json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}), call], "abc-123", tmp.path());
        assert_eq!(r[0]["result"]["tools"][2]["name"], "set_model");
        assert_eq!(r[1]["result"]["isError"], true);
    }

    #[test]
    fn attach_github_appends_validated_lines_for_claudiu_to_pick_up() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |args: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"attach_github","arguments":args}});
        let r = talk(
            &[
                call(json!({"kind":"issue","number":52,"title":"  Right\n sidebar "})),
                call(json!({"kind":"pull","number":7})),
                call(json!({"kind":"commit","number":1})),
                call(json!({"kind":"issue","number":0})),
                call(json!({"kind":"issue","number":-3})),
            ],
            "s1",
            tmp.path(),
        );
        let errors: Vec<_> = r.iter().map(|x| x["result"]["isError"].as_bool().unwrap()).collect();
        assert_eq!(errors, [false, false, true, true, true]);
        let lines: Vec<Attached> = std::fs::read_to_string(tmp.path().join("attach/s1.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines, [Attached { pull: false, number: 52, title: Some("Right sidebar".into()) }, Attached { pull: true, number: 7, title: None }]);
        assert_eq!(talk(&[call(json!({"kind":"issue","number":1}))], "", tmp.path())[0]["result"]["isError"], true);
        assert!(allowed_tools("code").contains(&"mcp__claudiu__attach_github".to_string()));
        assert!(allowed_tools("chat").contains(&"mcp__claudiu__attach_github".to_string()));
    }

    #[test]
    fn link_session_appends_a_validated_id_and_claudiu_reads_it_back() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |id: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"link_session","arguments":{"id":id}}});
        let r = talk(&[call(json!("abc-123")), call(json!("")), call(json!("../x")), call(json!("s1")), call(json!(5))], "s1", tmp.path());
        let errors: Vec<_> = r.iter().map(|x| x["result"]["isError"].as_bool().unwrap()).collect();
        assert_eq!(errors, [false, true, true, true, true], "bad ids and a link to itself are refused");
        let line = std::fs::read_to_string(tmp.path().join("attach/s1.jsonl")).unwrap();
        assert_eq!(line.lines().count(), 1);
        assert_eq!(parse_attachment(line.lines().next().unwrap()), Some(Attachment::Link("abc-123".into())));
        assert_eq!(parse_attachment(r#"{"pull":true,"number":7,"title":null}"#), Some(Attachment::Github(Attached { pull: true, number: 7, title: None })));
        assert_eq!(parse_attachment("nonsense"), None);
    }

    #[test]
    fn bad_titles_are_rejected_and_long_ones_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let call = |args: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"set_tab_title","arguments":args}});
        let r = talk(&[call(json!({"title":"   "})), call(json!({})), call(json!({"title":"x".repeat(200)}))], "s", tmp.path());
        assert_eq!(r[0]["result"]["isError"], true);
        assert_eq!(r[1]["result"]["isError"], true);
        assert_eq!(std::fs::read_to_string(tmp.path().join("titles/s.txt")).unwrap().len(), 60);
        assert_eq!(talk(&[call(json!({"title":"hi"}))], "", tmp.path())[0]["result"]["isError"], true);
    }
}
