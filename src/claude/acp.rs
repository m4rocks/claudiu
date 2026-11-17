//! Runs one task in Codex or OpenCode for a custom subagent (`claude/subagents.rs`), through the Agent Client
//! Protocol: `claudiu --acp`. The Custom subagents settings page also asks the agent for its models here (`models`).
//!
//! The mod starts this once per subagent turn and writes the task to stdin as JSON (`Request`). A mod can write a
//! child's stdin only once, and ACP is a conversation in both directions, so the conversation happens here. Progress
//! goes to stdout as JSON lines (`Event`), ending with the report or the reason it failed.
//!
//! Codex runs through `npx codex-acp`, which uses the user's own `codex login`; OpenCode runs as the user's own
//! `opencode acp`, with their providers. The agent's stdin is a pipe from this process, so when the mod stops this
//! helper (the user interrupted the subagent), the pipe closes and the agent exits with it.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::claude::subagents::{Agent, AgentModel};

pub const FLAG: &str = "--acp";
/// Pinned so an upstream change reaches users only with a Claudiu release.
const CODEX_ACP: &str = "@agentclientprotocol/codex-acp@2.6.0";
/// ACP's error code for "authentication required".
const AUTH_REQUIRED: i64 = -32000;
/// Lines of the agent's stderr kept for a failure's reason.
const STDERR_LINES: usize = 5;
/// How long the settings page waits for the agent's models; Codex's first run downloads codex-acp through npx.
const MODELS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// What the mod asks for.
#[derive(Debug, Deserialize)]
pub struct Request {
    /// The agent that runs the task. A mod from before OpenCode sends none, and means Codex.
    #[serde(default)]
    pub agent: Agent,
    /// The agent's model; none for the agent's own default.
    pub model: Option<String>,
    /// Whether the agent may edit files and run commands, or only read.
    pub edits: bool,
    /// The folder the agent works in.
    pub cwd: PathBuf,
    /// The task, as Claude wrote it.
    pub prompt: String,
}

/// One line of stdout.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A piece of a message the agent is writing.
    Text { text: String },
    /// A tool call the agent started: a command, a read, an edit.
    Step { text: String },
    /// The last message the agent wrote: the subagent's report.
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
    let finished = run_task(&request, &mut emit);
    emit(match finished {
        Ok(text) => Event::Report { text },
        Err(reason) => Event::Failed { reason },
    });
    0
}

fn run_task(request: &Request, emit: &mut impl FnMut(Event)) -> Result<String, String> {
    let env = match request.agent {
        Agent::Codex => {
            let mut env = vec![("INITIAL_AGENT_MODE", (if request.edits { "workspace-write" } else { "read-only" }).to_string())];
            if let Some(model) = &request.model {
                env.push(("CODEX_CONFIG", json!({ "model": model }).to_string()));
            }
            env
        }
        Agent::OpenCode => vec![("OPENCODE_CONFIG_CONTENT", opencode_config(request.model.as_deref(), request.edits).to_string())],
    };
    let connection = Connection::start(request.agent, &request.cwd, &env)?;
    let result = converse(connection.from_agent, connection.to_agent, request, emit);
    connection.process.finish(result)
}

/// The tools OpenCode may use read-only: reading and searching files. Everything else, the shell included, is denied,
/// since OpenCode has no sandbox that would let a command run without changing anything.
fn opencode_read_only() -> Value {
    json!({ "*": "deny", "read": "allow", "glob": "allow", "grep": "allow", "list": "allow" })
}

/// The config OpenCode gets on top of the user's own. Read-only pins the `build` agent and denies under it too,
/// because an agent's own permissions in a project's or the user's opencode.json beat the top-level ones.
fn opencode_config(model: Option<&str>, edits: bool) -> Value {
    let mut config = json!({});
    if let Some(model) = model {
        config["model"] = json!(model);
    }
    if !edits {
        config["default_agent"] = json!("build");
        config["permission"] = opencode_read_only();
        config["agent"] = json!({ "build": { "permission": opencode_read_only() } });
    }
    config
}

/// The models `agent` offers this account, as the model picker of a new session lists them. Gives up after
/// `MODELS_TIMEOUT`, so a stalled download or an agent that never answers can't leave the page loading for good.
pub fn models(agent: Agent, cwd: &Path) -> Result<Vec<AgentModel>, String> {
    let connection = Connection::start(agent, cwd, &[])?;
    let (Connection { from_agent, to_agent, process }, folder) = (connection, cwd.to_path_buf());
    let (tx, rx) = std::sync::mpsc::channel();
    // Killing the agent on a timeout closes its pipes, which ends this thread too.
    std::thread::spawn(move || {
        let _ = tx.send(list_models(agent, from_agent, to_agent, &folder));
    });
    let result = rx.recv_timeout(MODELS_TIMEOUT).unwrap_or_else(|_| Err(format!("{} didn't answer within {} seconds", server(agent), MODELS_TIMEOUT.as_secs())));
    process.finish(result)
}

/// The program Claudiu starts for `agent`, as failures name it.
fn server(agent: Agent) -> &'static str {
    match agent {
        Agent::Codex => "codex-acp",
        Agent::OpenCode => "opencode",
    }
}

fn exited(agent: Agent) -> String {
    format!("{} exited before {} answered", server(agent), agent.name())
}

/// A running agent: its pipes, and the process with its stderr.
struct Connection {
    to_agent: ChildStdin,
    from_agent: BufReader<ChildStdout>,
    process: Process,
}

struct Process {
    agent: Agent,
    child: Child,
    /// The last lines of stderr, collected on a thread so a full stderr pipe can't stall the agent.
    errors: JoinHandle<String>,
}

impl Connection {
    fn start(agent: Agent, cwd: &Path, env: &[(&str, String)]) -> Result<Connection, String> {
        let (program, args, missing): (&str, &[&str], &str) = match agent {
            Agent::Codex => ("npx", &["-y", CODEX_ACP], "npx was not found on PATH. Codex subagents need Node.js."),
            Agent::OpenCode => ("opencode", &["acp"], "opencode was not found on PATH. OpenCode subagents need OpenCode installed."),
        };
        let program = which::which(program).map_err(|_| missing.to_string())?;
        let mut command = Command::new(program);
        command.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let name = server(agent);
        let mut child = command.spawn().map_err(|e| format!("{name} could not start ({e})"))?;
        let (Some(to_agent), Some(from_agent), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            return Err(format!("{name} could not start (no pipes)"));
        };
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
        Ok(Connection { to_agent, from_agent: BufReader::new(from_agent), process: Process { agent, child, errors } })
    }
}

impl Process {
    /// Ends the agent once the conversation is over (its stdin is already closed) and returns `result`, with
    /// stderr's last lines added when the agent exited early.
    fn finish<T>(mut self, result: Result<T, String>) -> Result<T, String> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Only an early exit needs stderr's last lines. Waiting for stderr to close otherwise could hold up the answer
        // while something the agent started still has it open.
        let exited = exited(self.agent);
        match result {
            Err(reason) if reason == exited => {
                let stderr = self.errors.join().unwrap_or_default();
                Err(if stderr.is_empty() { reason } else { format!("{exited}: {stderr}") })
            }
            other => other,
        }
    }
}

fn initialize() -> Value {
    json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": 1,
        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
        "clientInfo": { "name": "claudiu", "version": env!("CARGO_PKG_VERSION") },
    }})
}

fn new_session(cwd: &Path) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "session/new", "params": { "cwd": cwd, "mcpServers": [] } })
}

/// Opens a session only to read its model picker: the config option whose category is `model`. OpenCode keeps every
/// session in its history, so it deletes this empty one again.
fn list_models(agent: Agent, from_agent: impl BufRead, mut to_agent: impl Write, cwd: &Path) -> Result<Vec<AgentModel>, String> {
    let mut lines = from_agent.lines();
    let mut send = |message: Value| writeln!(to_agent, "{message}").and_then(|_| to_agent.flush()).map_err(|_| exited(agent));
    let mut reply = |id: i64| -> Result<Value, String> {
        for line in lines.by_ref() {
            let msg: Value = serde_json::from_str(&line.map_err(|_| exited(agent))?).unwrap_or_default();
            if msg.get("method").is_none() && msg["id"].as_i64() == Some(id) {
                return match msg.get("error") {
                    Some(error) => Err(error_reason(agent, error)),
                    None => Ok(msg["result"].clone()),
                };
            }
        }
        Err(exited(agent))
    };
    send(initialize())?;
    reply(0)?;
    send(new_session(cwd))?;
    let session = reply(1)?;
    let options = session["configOptions"].as_array().into_iter().flatten();
    let picker = options.filter(|o| o["category"] == "model").find_map(|o| o["options"].as_array());
    let models: Vec<AgentModel> = picker
        .into_iter()
        .flatten()
        .filter_map(|o| Some(AgentModel { id: o["value"].as_str()?.to_string(), name: o["name"].as_str().unwrap_or_default().to_string() }))
        .collect();
    if agent == Agent::OpenCode
        && let Some(id) = session["sessionId"].as_str()
        // The models are known already, so a failed delete only leaves an empty session behind.
        && send(json!({ "jsonrpc": "2.0", "id": 2, "method": "session/delete", "params": { "sessionId": id } })).is_ok()
    {
        let _ = reply(2);
    }
    if models.is_empty() { Err(format!("{} listed no models", server(agent))) } else { Ok(models) }
}

/// The ACP conversation for one task: start, open a session in the task's folder, send the task, and follow the
/// session's updates until the agent answers. Returns the last message the agent wrote.
fn converse(from_agent: impl BufRead, mut to_agent: impl Write, request: &Request, emit: &mut impl FnMut(Event)) -> Result<String, String> {
    let agent = request.agent;
    let name = agent.name();
    let mut lines = from_agent.lines();
    let send = |to_agent: &mut dyn Write, message: Value| -> Result<(), String> { writeln!(to_agent, "{message}").and_then(|_| to_agent.flush()).map_err(|_| exited(agent)) };
    let mut messages = Messages::default();
    // What read-only refused last: codex-acp offers only "cancel" for some commands, which ends Codex's turn.
    let mut refused: Option<String> = None;
    // Waits for the answer to request `id`, handling what the agent sends meanwhile. Only updates of `session` count:
    // OpenCode also reports the sessions of its own subagents, whose messages are not this task's report.
    let mut answer = |to_agent: &mut dyn Write, id: i64, session: Option<&str>, messages: &mut Messages, emit: &mut dyn FnMut(Event)| -> Result<Value, String> {
        for line in lines.by_ref() {
            let line = line.map_err(|_| exited(agent))?;
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg.get("method").is_none() && msg["id"].as_i64() == Some(id) {
                return match msg.get("error") {
                    Some(error) => Err(error_reason(agent, error)),
                    None => Ok(msg["result"].clone()),
                };
            }
            match msg["method"].as_str() {
                Some("session/update") => {
                    if session.is_some_and(|s| msg["params"]["sessionId"] == s) {
                        messages.update(&msg["params"]["update"], emit);
                    }
                }
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
        Err(exited(agent))
    };

    send(&mut to_agent, initialize())?;
    answer(&mut to_agent, 0, None, &mut messages, emit)?;

    send(&mut to_agent, new_session(&request.cwd))?;
    let session = answer(&mut to_agent, 1, None, &mut messages, emit)?;
    let session_id = session["sessionId"].as_str().ok_or(format!("{} opened no session", server(agent)))?.to_string();

    send(&mut to_agent, json!({ "jsonrpc": "2.0", "id": 2, "method": "session/prompt", "params": {
        "sessionId": session_id,
        "prompt": [{ "type": "text", "text": request.prompt }],
    }}))?;
    let done = answer(&mut to_agent, 2, Some(&session_id), &mut messages, emit)?;
    let stop = done["stopReason"].as_str().unwrap_or("end_turn");
    match (stop, messages.last()) {
        ("end_turn", Some(text)) => Ok(text),
        ("end_turn", None) => Err(format!("{name} finished without writing a report")),
        ("cancelled", _) => Err(match refused {
            Some(action) => format!("{name} stopped when read-only mode refused `{action}`. Turn on Allow edits in Settings → Custom subagents to let it."),
            None => format!("{name}'s turn was cancelled"),
        }),
        ("refusal", _) => Err(format!("{name} refused the task")),
        // Out of tokens or turns: what the agent wrote last is still the best report there is.
        (_, Some(text)) => Ok(text),
        (other, None) => Err(format!("{name} stopped ({other}) without writing a report")),
    }
}

/// The messages the agent wrote in this run, as their pieces arrive. A message ends where a tool call starts or
/// another message begins.
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

fn error_reason(agent: Agent, error: &Value) -> String {
    if error["code"].as_i64() == Some(AUTH_REQUIRED) {
        return match agent {
            Agent::Codex => "Codex isn't signed in. Run `codex login`, then try again.".into(),
            Agent::OpenCode => "OpenCode isn't signed in. Run `opencode auth login`, then try again.".into(),
        };
    }
    let server = server(agent);
    match error["message"].as_str() {
        Some(message) => format!("{server}: {message}"),
        None => format!("{server}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(agent: Agent, edits: bool) -> Request {
        Request { agent, model: Some("gpt-5.5".into()), edits, cwd: PathBuf::from("/work"), prompt: "Find the bug".into() }
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
    fn talk_to(agent: Agent, agent_says: &[Value], edits: bool) -> (Result<String, String>, Vec<Value>, Vec<Event>) {
        let script: String = agent_says.iter().map(|m| format!("{m}\n")).collect();
        let mut sent = Vec::new();
        let mut events = vec![];
        let result = converse(script.as_bytes(), &mut sent, &request(agent, edits), &mut |e| events.push(e));
        let sent = String::from_utf8(sent).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        (result, sent, events)
    }

    fn talk(agent_says: &[Value], edits: bool) -> (Result<String, String>, Vec<Value>, Vec<Event>) {
        talk_to(Agent::Codex, agent_says, edits)
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

    #[test]
    fn a_child_session_message_is_not_the_report() {
        let mut says = opening();
        says.extend([
            chunk("m1", "Asked a helper."),
            json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "child", "update": {
                "sessionUpdate": "agent_message_chunk", "messageId": "c1", "content": { "type": "text", "text": "Helper notes." } } } }),
            reply(2, json!({ "stopReason": "end_turn" })),
        ]);
        let (result, _, events) = talk_to(Agent::OpenCode, &says, true);
        assert_eq!(result, Ok("Asked a helper.".into()));
        assert_eq!(events, vec![Event::Text { text: "Asked a helper.".into() }]);
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
    fn opencode_permission_options_are_answered_by_kind() {
        // What opencode 2.3 offers.
        let options = json!([
            { "optionId": "once", "kind": "allow_once", "name": "Allow once" },
            { "optionId": "always", "kind": "allow_always", "name": "Always allow" },
            { "optionId": "reject", "kind": "reject_once", "name": "Reject" },
        ]);
        assert_eq!(permission(&options, false), json!({ "outcome": "selected", "optionId": "reject" }));
        assert_eq!(permission(&options, true), json!({ "outcome": "selected", "optionId": "once" }));
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
    fn signed_out_says_how_to_sign_in() {
        let says = vec![
            reply(0, json!({ "protocolVersion": 1 })),
            json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32000, "message": "Authentication required" } }),
        ];
        assert_eq!(talk(&says, false).0, Err("Codex isn't signed in. Run `codex login`, then try again.".into()));
        assert_eq!(talk_to(Agent::OpenCode, &says, false).0, Err("OpenCode isn't signed in. Run `opencode auth login`, then try again.".into()));
    }

    #[test]
    fn an_agent_that_exits_midway_fails() {
        let mut says = opening();
        says.push(chunk("m1", "Starting"));
        assert_eq!(talk(&says, false).0, Err("codex-acp exited before Codex answered".into()));
        assert_eq!(talk_to(Agent::OpenCode, &says, false).0, Err("opencode exited before OpenCode answered".into()));
    }

    #[test]
    fn a_turn_without_a_message_fails() {
        let mut says = opening();
        says.push(reply(2, json!({ "stopReason": "end_turn" })));
        assert_eq!(talk(&says, false).0, Err("Codex finished without writing a report".into()));
        assert_eq!(talk_to(Agent::OpenCode, &says, false).0, Err("OpenCode finished without writing a report".into()));
    }

    #[test]
    fn running_out_of_tokens_still_reports_the_last_message() {
        let mut says = opening();
        says.extend([chunk("m1", "Partial findings."), reply(2, json!({ "stopReason": "max_tokens" }))]);
        assert_eq!(talk(&says, false).0, Ok("Partial findings.".into()));
    }

    #[test]
    fn read_only_opencode_denies_all_but_reading_even_in_its_build_agent() {
        let read_only = json!({ "*": "deny", "read": "allow", "glob": "allow", "grep": "allow", "list": "allow" });
        assert_eq!(
            opencode_config(Some("openai/gpt-6.1-sol"), false),
            json!({ "model": "openai/gpt-6.1-sol", "default_agent": "build", "permission": read_only, "agent": { "build": { "permission": read_only } } })
        );
        assert_eq!(opencode_config(Some("openai/gpt-6.1-sol"), true), json!({ "model": "openai/gpt-6.1-sol" }));
        assert_eq!(opencode_config(None, true), json!({}));
    }

    #[test]
    fn a_request_without_an_agent_is_for_codex() {
        let r: Request = serde_json::from_value(json!({ "model": "gpt-5.5", "edits": false, "cwd": "/w", "prompt": "x" })).unwrap();
        assert_eq!(r.agent, Agent::Codex);
        let r: Request = serde_json::from_value(json!({ "agent": "opencode", "model": "openai/gpt-5.5", "edits": false, "cwd": "/w", "prompt": "x" })).unwrap();
        assert_eq!(r.agent, Agent::OpenCode);
    }

    fn picker_session() -> Value {
        reply(1, json!({ "sessionId": "s1", "configOptions": [
            { "id": "mode", "category": "mode", "options": [{ "value": "read-only", "name": "Read-only" }] },
            { "id": "model", "category": "model", "options": [
                { "value": "gpt-6.1-sol", "name": "6.1 Sol" },
                { "value": "gpt-5.6-luna", "name": "5.6 Luna" },
            ]},
        ]}))
    }

    fn list(agent: Agent, says: &[Value]) -> (Result<Vec<AgentModel>, String>, Vec<Value>) {
        let script: String = says.iter().map(|m| format!("{m}\n")).collect();
        let mut sent = Vec::new();
        let models = list_models(agent, script.as_bytes(), &mut sent, Path::new("/work"));
        (models, String::from_utf8(sent).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect())
    }

    #[test]
    fn the_models_come_from_the_session_model_picker() {
        let says = [reply(0, json!({ "protocolVersion": 1 })), update(json!({ "sessionUpdate": "available_commands_update", "availableCommands": [] })), picker_session()];
        let (models, sent) = list(Agent::Codex, &says);
        assert_eq!(
            models.unwrap(),
            vec![AgentModel { id: "gpt-6.1-sol".into(), name: "6.1 Sol".into() }, AgentModel { id: "gpt-5.6-luna".into(), name: "5.6 Luna".into() }]
        );
        assert_eq!(sent.iter().map(|m| m["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "session/new"], "no prompt, and Codex's session is left alone");
    }

    #[test]
    fn opencode_deletes_the_session_it_read_the_models_from() {
        let says = [reply(0, json!({ "protocolVersion": 1 })), picker_session(), reply(2, json!({}))];
        let (models, sent) = list(Agent::OpenCode, &says);
        assert_eq!(models.unwrap().len(), 2);
        assert_eq!(sent.iter().map(|m| m["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "session/new", "session/delete"]);
        assert_eq!(sent[2]["params"]["sessionId"], "s1");
        // A delete opencode refuses, or never answers before exiting, still gives the models.
        let says = [reply(0, json!({ "protocolVersion": 1 })), picker_session()];
        assert_eq!(list(Agent::OpenCode, &says).0.unwrap().len(), 2);
    }

    #[test]
    fn no_model_picker_is_an_error() {
        let says = [reply(0, json!({})), reply(1, json!({ "sessionId": "s1" }))];
        assert_eq!(list(Agent::Codex, &says).0, Err("codex-acp listed no models".into()));
        assert_eq!(list(Agent::OpenCode, &says).0, Err("opencode listed no models".into()));
    }

    #[test]
    fn events_are_json_lines_the_mod_reads() {
        assert_eq!(serde_json::to_value(Event::Step { text: "Ran ls".into() }).unwrap(), json!({ "kind": "step", "text": "Ran ls" }));
        assert_eq!(serde_json::to_value(Event::Failed { reason: "x".into() }).unwrap(), json!({ "kind": "failed", "reason": "x" }));
    }
}
