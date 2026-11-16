//! Runs one task in Codex for a custom subagent (`claude/subagents.rs`), through the Agent Client Protocol:
//! `claudiu --acp`.
//!
//! The mod starts this once per subagent turn and writes the task to stdin as JSON (`Request`). A mod can write a
//! child's stdin only once, and ACP is a conversation in both directions, so the conversation happens here. Progress
//! goes to stdout as JSON lines (`Event`), ending with the report or the reason it failed.
//!
//! Codex runs through `npx codex-acp`, which uses the user's own `codex login`. Its stdin is a pipe from this
//! process, so when the mod stops this helper (the user interrupted the subagent), the pipe closes and codex-acp
//! exits with it.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const FLAG: &str = "--acp";
/// Pinned so an upstream change reaches users only with a Claudiu release.
const CODEX_ACP: &str = "@agentclientprotocol/codex-acp@2.6.0";
/// ACP's error code for "authentication required".
const AUTH_REQUIRED: i64 = -32000;
/// Lines of codex-acp's stderr kept for a failure's reason.
const STDERR_LINES: usize = 5;

/// What the mod asks for.
#[derive(Debug, Deserialize)]
pub struct Request {
    /// Codex's model; none for Codex's own default.
    pub model: Option<String>,
    /// Whether Codex may edit files and run commands (`workspace-write`) or only read (`read-only`).
    pub edits: bool,
    /// The folder Codex works in.
    pub cwd: PathBuf,
    /// The task, as Claude wrote it.
    pub prompt: String,
}

/// One line of stdout.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A piece of a message Codex is writing.
    Text { text: String },
    /// A tool call Codex started: a command, a read, an edit.
    Step { text: String },
    /// The last message Codex wrote: the subagent's report.
    Report { text: String },
    Failed { reason: String },
}

pub fn run() -> i32 {
    let mut out = std::io::stdout().lock();
    let mut emit = |event: Event| {
        if let Ok(line) = serde_json::to_string(&event) {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    };
    let mut input = String::new();
    let request = std::io::stdin().read_to_string(&mut input).map_err(|e| e.to_string()).and_then(|_| serde_json::from_str::<Request>(&input).map_err(|e| e.to_string()));
    let request = match request {
        Ok(r) => r,
        Err(e) => {
            emit(Event::Failed { reason: format!("the task could not be read ({e})") });
            return 1;
        }
    };
    let finished = run_codex(&request, &mut emit);
    emit(match finished {
        Ok(text) => Event::Report { text },
        Err(reason) => Event::Failed { reason },
    });
    0
}

fn run_codex(request: &Request, emit: &mut impl FnMut(Event)) -> Result<String, String> {
    let npx = which::which("npx").map_err(|_| "npx was not found on PATH. Codex subagents need Node.js.".to_string())?;
    let mut command = Command::new(npx);
    command
        .args(["-y", CODEX_ACP])
        .current_dir(&request.cwd)
        .env("INITIAL_AGENT_MODE", if request.edits { "workspace-write" } else { "read-only" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(model) = &request.model {
        command.env("CODEX_CONFIG", json!({ "model": model }).to_string());
    }
    let mut child = command.spawn().map_err(|e| format!("codex-acp could not start ({e})"))?;
    let (Some(to_agent), Some(from_agent), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        return Err("codex-acp could not start (no pipes)".into());
    };
    // The last lines of stderr explain an exit; read on a thread so a full stderr pipe can't stall codex-acp.
    let errors = std::thread::spawn(move || {
        let mut last: Vec<String> = vec![];
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !line.trim().is_empty() {
                last.push(line);
                if last.len() > STDERR_LINES {
                    last.remove(0);
                }
            }
        }
        last.join("\n")
    });

    let result = converse(BufReader::new(from_agent), to_agent, request, emit);
    // Closing its stdin (dropped with `to_agent`) ends codex-acp; kill it in case it lingers.
    let _ = child.kill();
    let _ = child.wait();
    // Only an early exit needs stderr's last lines. Waiting for stderr to close otherwise could hold up the report
    // while something codex-acp started still has it open.
    match result {
        Err(reason) if reason == EXITED => {
            let stderr = errors.join().unwrap_or_default();
            Err(if stderr.is_empty() { reason } else { format!("{EXITED}: {stderr}") })
        }
        other => other,
    }
}

const EXITED: &str = "codex-acp exited before Codex answered";

/// The ACP conversation for one task: start, open a session in the task's folder, send the task, and follow the
/// session's updates until Codex answers. Returns the last message Codex wrote.
fn converse(from_agent: impl BufRead, mut to_agent: impl Write, request: &Request, emit: &mut impl FnMut(Event)) -> Result<String, String> {
    let mut lines = from_agent.lines();
    let send = |to_agent: &mut dyn Write, message: Value| -> Result<(), String> {
        writeln!(to_agent, "{message}").and_then(|_| to_agent.flush()).map_err(|_| EXITED.to_string())
    };
    // Waits for the answer to request `id`, handling what Codex sends meanwhile.
    let mut messages = Messages::default();
    // What read-only refused last: codex-acp offers only "cancel" for some commands, which ends Codex's turn.
    let mut refused: Option<String> = None;
    let mut answer = |to_agent: &mut dyn Write, id: i64, messages: &mut Messages, emit: &mut dyn FnMut(Event)| -> Result<Value, String> {
        for line in lines.by_ref() {
            let line = line.map_err(|_| EXITED.to_string())?;
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg.get("method").is_none() && msg["id"].as_i64() == Some(id) {
                return match msg.get("error") {
                    Some(error) => Err(error_reason(error)),
                    None => Ok(msg["result"].clone()),
                };
            }
            match msg["method"].as_str() {
                Some("session/update") => messages.update(&msg["params"]["update"], emit),
                Some("session/request_permission") => {
                    let outcome = permission(&msg["params"]["options"], request.edits);
                    if !request.edits {
                        refused = Some(msg["params"]["toolCall"]["title"].as_str().unwrap_or("an action").to_string());
                    }
                    send(to_agent, json!({ "jsonrpc": "2.0", "id": msg["id"], "result": { "outcome": outcome } }))?;
                }
                // A request this client doesn't serve: it offered no file or terminal access.
                Some(_) if msg.get("id").is_some() => {
                    send(to_agent, json!({ "jsonrpc": "2.0", "id": msg["id"], "error": { "code": -32601, "message": "method not found" } }))?;
                }
                _ => {}
            }
        }
        Err(EXITED.to_string())
    };

    send(&mut to_agent, json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": 1,
        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
        "clientInfo": { "name": "claudiu", "version": env!("CARGO_PKG_VERSION") },
    }}))?;
    answer(&mut to_agent, 0, &mut messages, emit)?;

    send(&mut to_agent, json!({ "jsonrpc": "2.0", "id": 1, "method": "session/new", "params": { "cwd": request.cwd, "mcpServers": [] } }))?;
    let session = answer(&mut to_agent, 1, &mut messages, emit)?;
    let session_id = session["sessionId"].as_str().ok_or("codex-acp opened no session")?.to_string();

    send(&mut to_agent, json!({ "jsonrpc": "2.0", "id": 2, "method": "session/prompt", "params": {
        "sessionId": session_id,
        "prompt": [{ "type": "text", "text": request.prompt }],
    }}))?;
    let done = answer(&mut to_agent, 2, &mut messages, emit)?;
    let stop = done["stopReason"].as_str().unwrap_or("end_turn");
    match (stop, messages.last()) {
        ("end_turn", Some(text)) => Ok(text),
        ("end_turn", None) => Err("Codex finished without writing a report".into()),
        ("cancelled", _) => Err(match refused {
            Some(action) => format!("Codex stopped when read-only mode refused `{action}`. Turn on Allow edits in Settings → Custom subagents to let it."),
            None => "Codex's turn was cancelled".into(),
        }),
        ("refusal", _) => Err("Codex refused the task".into()),
        // Out of tokens or turns: what Codex wrote last is still the best report there is.
        (_, Some(text)) => Ok(text),
        (other, None) => Err(format!("Codex stopped ({other}) without writing a report")),
    }
}

/// The messages Codex wrote in this run, as their pieces arrive. A message ends where a tool call starts or another
/// message begins.
#[derive(Default)]
struct Messages {
    current: String,
    current_id: Option<String>,
    last: Option<String>,
}

impl Messages {
    fn update(&mut self, update: &Value, emit: &mut dyn FnMut(Event)) {
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let Some(text) = update["content"]["text"].as_str() else { return };
                let id = update["messageId"].as_str().map(String::from);
                if id.is_some() && id != self.current_id {
                    self.close();
                    self.current_id = id;
                }
                self.current.push_str(text);
                emit(Event::Text { text: text.to_string() });
            }
            Some("tool_call") => {
                self.close();
                let title = update["title"].as_str().unwrap_or("a tool");
                emit(Event::Step { text: title.to_string() });
            }
            _ => {}
        }
    }

    /// Ends the message being written; a blank one doesn't replace the last.
    fn close(&mut self) {
        let text = std::mem::take(&mut self.current);
        if !text.trim().is_empty() {
            self.last = Some(text);
        }
        self.current_id = None;
    }

    fn last(&mut self) -> Option<String> {
        self.close();
        self.last.clone()
    }
}

/// The answer to a permission request: an allowing option when edits are on, a refusing one when they are off.
/// codex-acp offers two refusals of the same kind: `decline` lets Codex carry on without the action, `cancel` ends its
/// turn, so `decline` comes first. Without a fitting option the request is answered as cancelled, which refuses it too.
fn permission(options: &Value, edits: bool) -> Value {
    let options: Vec<&Value> = options.as_array().into_iter().flatten().collect();
    let of_kind = |kinds: &[&str]| options.iter().copied().find(|o| o["kind"].as_str().is_some_and(|k| kinds.contains(&k)));
    let chosen = if edits {
        of_kind(&["allow_once", "allow_always"])
    } else {
        options.iter().copied().find(|o| o["optionId"] == "decline").or_else(|| of_kind(&["reject_once", "reject_always"]))
    };
    match chosen.and_then(|o| o["optionId"].as_str()) {
        Some(id) => json!({ "outcome": "selected", "optionId": id }),
        None => json!({ "outcome": "cancelled" }),
    }
}

fn error_reason(error: &Value) -> String {
    if error["code"].as_i64() == Some(AUTH_REQUIRED) {
        return "Codex isn't signed in. Run `codex login`, then try again.".into();
    }
    match error["message"].as_str() {
        Some(message) => format!("codex-acp: {message}"),
        None => format!("codex-acp: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(edits: bool) -> Request {
        Request { model: Some("gpt-5.5".into()), edits, cwd: PathBuf::from("/work"), prompt: "Find the bug".into() }
    }

    fn update(update: Value) -> Value {
        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1", "update": update } })
    }

    fn chunk(id: &str, text: &str) -> Value {
        update(json!({ "sessionUpdate": "agent_message_chunk", "messageId": id, "content": { "type": "text", "text": text } }))
    }

    fn reply(id: i64, result: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "result": result })
    }

    /// Runs the conversation against what a fake agent says, in order. Returns the result, what was sent to the agent,
    /// and the events.
    fn talk(agent_says: &[Value], edits: bool) -> (Result<String, String>, Vec<Value>, Vec<Event>) {
        let script: String = agent_says.iter().map(|m| format!("{m}\n")).collect();
        let mut sent = Vec::new();
        let mut events = vec![];
        let result = converse(script.as_bytes(), &mut sent, &request(edits), &mut |e| events.push(e));
        let sent = String::from_utf8(sent).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        (result, sent, events)
    }

    fn opening() -> Vec<Value> {
        vec![reply(0, json!({ "protocolVersion": 1 })), reply(1, json!({ "sessionId": "s1" }))]
    }

    #[test]
    fn the_report_is_the_last_message_and_every_piece_streams() {
        let mut says = opening();
        says.extend([
            chunk("m1", "Looking at "),
            chunk("m1", "the parser."),
            update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Read src/parse.rs", "kind": "read", "status": "pending" })),
            chunk("m2", "The bug is on line 12."),
            reply(2, json!({ "stopReason": "end_turn" })),
        ]);
        let (result, sent, events) = talk(&says, false);
        assert_eq!(result, Ok("The bug is on line 12.".into()));
        assert_eq!(
            events,
            vec![
                Event::Text { text: "Looking at ".into() },
                Event::Text { text: "the parser.".into() },
                Event::Step { text: "Read src/parse.rs".into() },
                Event::Text { text: "The bug is on line 12.".into() },
            ]
        );
        assert_eq!(sent.iter().map(|m| m["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "session/new", "session/prompt"]);
        assert_eq!(sent[1]["params"]["cwd"], "/work");
        assert_eq!(sent[2]["params"]["sessionId"], "s1");
        assert_eq!(sent[2]["params"]["prompt"][0]["text"], "Find the bug");
        assert_eq!(sent[0]["params"]["clientCapabilities"]["terminal"], false);
    }

    #[test]
    fn messages_without_ids_split_at_tool_calls() {
        let mut says = opening();
        says.extend([
            update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "First." } })),
            update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Ran ls" })),
            update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Second." } })),
            reply(2, json!({ "stopReason": "end_turn" })),
        ]);
        assert_eq!(talk(&says, false).0, Ok("Second.".into()));
    }

    fn asks_permission() -> Vec<Value> {
        let mut says = opening();
        says.extend([
            json!({ "jsonrpc": "2.0", "id": 7, "method": "session/request_permission", "params": { "sessionId": "s1", "toolCall": { "toolCallId": "c1" }, "options": [
                { "optionId": "yes", "name": "Allow once", "kind": "allow_once" },
                { "optionId": "no", "name": "Reject", "kind": "reject_once" },
            ]}}),
            chunk("m1", "Done."),
            reply(2, json!({ "stopReason": "end_turn" })),
        ]);
        says
    }

    #[test]
    fn a_turn_ended_by_a_refusal_says_what_was_refused() {
        let mut says = opening();
        says.extend([
            json!({ "jsonrpc": "2.0", "id": 0, "method": "session/request_permission", "params": { "sessionId": "s1",
                "toolCall": { "toolCallId": "c1", "title": "printf hi > made.txt" },
                "options": [{ "optionId": "allow_once", "kind": "allow_once" }, { "optionId": "cancel", "kind": "reject_once" }] } }),
            reply(2, json!({ "stopReason": "cancelled" })),
        ]);
        let (result, sent, _) = talk(&says, false);
        assert_eq!(
            result,
            Err("Codex stopped when read-only mode refused `printf hi > made.txt`. Turn on Allow edits in Settings → Custom subagents to let it.".into())
        );
        assert_eq!(sent.last().unwrap()["result"]["outcome"]["optionId"], "cancel");
    }

    #[test]
    fn read_only_refuses_permission_requests() {
        let (_, sent, _) = talk(&asks_permission(), false);
        let answer = sent.iter().find(|m| m["id"] == 7).unwrap();
        assert_eq!(answer["result"]["outcome"], json!({ "outcome": "selected", "optionId": "no" }));
    }

    #[test]
    fn with_edits_on_permission_requests_are_allowed() {
        let (_, sent, _) = talk(&asks_permission(), true);
        let answer = sent.iter().find(|m| m["id"] == 7).unwrap();
        assert_eq!(answer["result"]["outcome"], json!({ "outcome": "selected", "optionId": "yes" }));
    }

    #[test]
    fn read_only_declines_rather_than_ending_codex_turn() {
        let options = json!([
            { "optionId": "allow_once", "kind": "allow_once" },
            { "optionId": "cancel", "kind": "reject_once" },
            { "optionId": "decline", "kind": "reject_once" },
        ]);
        assert_eq!(permission(&options, false), json!({ "outcome": "selected", "optionId": "decline" }));
    }

    #[test]
    fn a_permission_request_with_no_refusing_option_is_cancelled() {
        let options = json!([{ "optionId": "yes", "kind": "allow_once" }]);
        assert_eq!(permission(&options, false), json!({ "outcome": "cancelled" }));
    }

    #[test]
    fn file_and_terminal_requests_are_refused() {
        let mut says = opening();
        says.extend([
            json!({ "jsonrpc": "2.0", "id": 9, "method": "fs/write_text_file", "params": { "sessionId": "s1", "path": "/etc/x", "content": "" } }),
            chunk("m1", "Done."),
            reply(2, json!({ "stopReason": "end_turn" })),
        ]);
        let (result, sent, _) = talk(&says, true);
        assert_eq!(result, Ok("Done.".into()));
        assert_eq!(sent.iter().find(|m| m["id"] == 9).unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn signed_out_says_to_run_codex_login() {
        let says = vec![
            reply(0, json!({ "protocolVersion": 1 })),
            json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32000, "message": "Authentication required" } }),
        ];
        assert_eq!(talk(&says, false).0, Err("Codex isn't signed in. Run `codex login`, then try again.".into()));
    }

    #[test]
    fn an_agent_that_exits_midway_fails() {
        let mut says = opening();
        says.push(chunk("m1", "Starting"));
        assert_eq!(talk(&says, false).0, Err(EXITED.into()));
    }

    #[test]
    fn a_turn_without_a_message_fails() {
        let mut says = opening();
        says.push(reply(2, json!({ "stopReason": "end_turn" })));
        assert_eq!(talk(&says, false).0, Err("Codex finished without writing a report".into()));
    }

    #[test]
    fn running_out_of_tokens_still_reports_the_last_message() {
        let mut says = opening();
        says.extend([chunk("m1", "Partial findings."), reply(2, json!({ "stopReason": "max_tokens" }))]);
        assert_eq!(talk(&says, false).0, Ok("Partial findings.".into()));
    }

    #[test]
    fn events_are_json_lines_the_mod_reads() {
        assert_eq!(serde_json::to_value(Event::Step { text: "Ran ls".into() }).unwrap(), json!({ "kind": "step", "text": "Ran ls" }));
        assert_eq!(serde_json::to_value(Event::Failed { reason: "x".into() }).unwrap(), json!({ "kind": "failed", "reason": "x" }));
    }
}
