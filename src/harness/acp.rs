//! A long-lived Agent Client Protocol connection to a harness: newline-delimited JSON-RPC over the agent's stdio.
//! Requests return futures; what the agent sends unasked arrives as `Event`s. No GPUI here; `thread_view` drives it.
//! `harness::runner` is a different, one-shot client: custom subagents run through it.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use futures::channel::{mpsc as async_mpsc, oneshot};
use serde_json::{Value, json};

/// How to start a harness's ACP server.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

/// Preserve ACP error metadata for recovery instead of inferring everything from prose.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: Option<i64>,
    pub message: String,
    pub data: Value,
}

impl RpcError {
    pub fn authentication_required(&self) -> bool {
        self.code == Some(-32000) || looks_like_auth_error(&self.message)
    }
}

pub fn looks_like_auth_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["authentication required", "authentication_error", "invalid api key", "run /login", "not logged in", "not logged into", "not authenticated", "not signed in", "isn't signed in", "please log in", "please login", "please sign in", "token expired", "token has expired", "expired token", "invalid_grant", "refresh token", "unauthorized", "http 401", "status 401"].iter().any(|part| message.contains(part))
}


/// What the agent sends without being asked.
#[derive(Debug)]
pub enum Event {
    RequestFailed(RpcError),
    Authentication(Value),
    /// A `session/update` notification and the session it belongs to.
    Update { session_id: Option<String>, update: Value },
    /// `session/request_permission`: answer it with `Connection::respond` and the request's id.
    Permission { id: Value, params: Value },
    /// `elicitation/create` or `session/elicitation`: structured user-input question from the agent.
    Elicitation { id: Value, params: Value },
    /// The agent stopped; the text is its last words on stderr, if any.
    Exited(String),
    ProcessExited { code: Option<i32>, message: String },
    /// A `session/prompt`'s reply. The thread queues it itself once the reply arrives, which is after the reader
    /// queued every update the agent sent before replying, so a turn never ends ahead of its last words.
    TurnEnded(Result<Value, String>),
}

impl Event {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Update { session_id, .. } => session_id.as_deref(),
            Self::Permission { params, .. } | Self::Elicitation { params, .. } => params["sessionId"].as_str(),
            Self::Exited(_) | Self::ProcessExited { .. } | Self::TurnEnded(_) | Self::RequestFailed(_) | Self::Authentication(_) => None,
        }
    }
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

struct RequestGuard {
    id: i64,
    pending: Pending,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}

/// A running harness. Dropping it ends the process.
pub struct Connection {
    outgoing: mpsc::Sender<Option<Value>>,
    pending: Pending,
    next_id: AtomicI64,
    child: Arc<Mutex<Option<Child>>>,
    closed: Arc<AtomicBool>,
}

impl Connection {
    /// Start the server. Returns at once; send `initialize` next.
    pub fn start(launch: &Launch, events: async_mpsc::UnboundedSender<Event>) -> Result<Connection, String> {
        let mut cmd = Command::new(&launch.program);
        cmd.args(&launch.args).current_dir(&launch.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::platform::no_window(&mut cmd);
        crate::platform::with_login_path(&mut cmd);
        for (key, value) in &launch.env {
            cmd.env(key, value);
        }
        let name = launch.program.file_name().map_or_else(|| launch.program.display().to_string(), |n| n.to_string_lossy().into_owned());
        let mut child = cmd.spawn().map_err(|e| format!("{name} could not start: {e}"))?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            return Err(format!("{name} could not start (no pipes)"));
        };
        let connection = Self::connect(stdout, stdin, Some(stderr), events);
        *connection.child.lock().unwrap() = Some(child);
        Ok(connection)
    }

    /// Talk to an agent over `reader` and `writer` (its stdout and stdin).
    pub fn connect(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static, stderr: Option<impl Read + Send + 'static>, events: async_mpsc::UnboundedSender<Event>) -> Connection {
        let (outgoing, queue) = mpsc::channel::<Option<Value>>();
        let pending: Pending = Default::default();
        let closed = Arc::new(AtomicBool::new(false));
        let child: Arc<Mutex<Option<Child>>> = Default::default();
        {
            let (pending, closed, events) = (pending.clone(), closed.clone(), events.clone());
            std::thread::spawn(move || {
                let mut writer = writer;
                while let Ok(Some(message)) = queue.recv() {
                    let Ok(mut line) = serde_json::to_vec(&message) else { continue };
                    line.push(b'\n');
                    if let Err(error) = writer.write_all(&line).and_then(|_| writer.flush()) {
                        if !closed.swap(true, Ordering::AcqRel) {
                            let why = format!("The connection to the agent failed: {error}");
                            for (_, reply) in pending.lock().unwrap().drain() { let _ = reply.send(Err(why.clone())); }
                            let _ = events.unbounded_send(Event::Exited(why));
                        }
                        break;
                    }
                }
            });
        }
        // Kept only to explain an exit; never logged.
        let last_words = Arc::new(Mutex::new(VecDeque::<String>::new()));
        if let Some(stderr) = stderr {
            let last_words = last_words.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut lines = last_words.lock().unwrap();
                    lines.push_back(line);
                    if lines.len() > 5 {
                        lines.pop_front();
                    }
                }
            });
        }
        {
            let child = child.clone();
            let (pending, outgoing) = (pending.clone(), outgoing.clone());
            let closed = closed.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(reader).lines().map_while(Result::ok) {
                    // Some servers print a banner or a log line on stdout; only JSON is the protocol.
                    if let Ok(message) = serde_json::from_str::<Value>(&line) {
                        handle(message, &pending, &outgoing, &events);
                    }
                }
                let requested = closed.swap(true, Ordering::AcqRel);
                pending.lock().unwrap().clear();
                // Give stderr a moment to deliver the reason before reporting it.
                std::thread::sleep(Duration::from_millis(200));
                let words = last_words.lock().unwrap().iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
                if !requested {
                    let status = child.lock().unwrap().as_mut().and_then(|child| child.try_wait().ok().flatten());
                    match status {
                        Some(status) => { let _ = events.unbounded_send(Event::ProcessExited { code: status.code(), message: words }); }
                        None => {
                            let reason = if words.is_empty() { "The agent connection closed unexpectedly. Retry the session to reconnect.".into() } else { words };
                            let _ = events.unbounded_send(Event::Exited(reason));
                        }
                    }
                }
            });
        }
        Connection { outgoing, pending, next_id: AtomicI64::new(1), child, closed }
    }

    pub fn request(&self, method: &str, params: Value) -> impl Future<Output = Result<Value, String>> + use<> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let mut pending = self.pending.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            let _ = tx.send(Err("the agent stopped".into()));
        } else {
            pending.insert(id, tx);
            if self.outgoing.send(Some(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))).is_err() {
                self.closed.store(true, Ordering::Release);
                pending.clear();
            }
        }
        let guard = RequestGuard { id, pending: self.pending.clone() };
        async move {
            let _guard = guard;
            rx.await.unwrap_or_else(|_| Err("the agent stopped".into()))
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.outgoing.send(Some(json!({"jsonrpc": "2.0", "method": method, "params": params})));
    }

    /// Answer a request the agent made.
    pub fn respond(&self, id: Value, result: Value) {
        let _ = self.outgoing.send(Some(json!({"jsonrpc": "2.0", "id": id, "result": result})));
    }

    pub fn terminate(&self) {
        self.closed.store(true, Ordering::Release);
        self.pending.lock().unwrap().clear();
        // Telling the writer thread to stop drops the child's stdin pipe, which is the polite shutdown signal
        // (`bridge/claude.mjs`, `codex-acp`, and `opencode acp` all exit cleanly on stdin EOF).
        let _ = self.outgoing.send(None);
        if let Some(mut child) = self.child.lock().unwrap().take() {
            // Off the UI thread: wait briefly for stdin EOF to stop the server cleanly, then force-kill the tree.
            std::thread::spawn(move || {
                for _ in 0..30 {
                    if child.try_wait().ok().flatten().is_some() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                #[cfg(windows)]
                {
                    let mut kill_tree = Command::new("taskkill");
                    kill_tree.args(["/F", "/T", "/PID", &child.id().to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                    crate::platform::no_window(&mut kill_tree);
                    let _ = kill_tree.status();
                }
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// What m4code tells every agent about itself. It reads and writes files itself and runs no terminals for the
/// agent, so the agent does those on its own. Advertising `elicitation.form` enables native structured questions
/// (e.g. Codex's `request_user_input` through `codex-acp`).
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
            "elicitation": { "form": {} },
        },
        "clientInfo": { "name": "m4code", "title": "m4code", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The answer to an ACP/MCP elicitation request: `accept` with typed `content`, `cancel`, or `decline`.
pub fn elicitation_accept(content: serde_json::Map<String, Value>) -> Value {
    json!({ "action": "accept", "content": content })
}

pub fn elicitation_cancel() -> Value {
    json!({ "action": "cancel" })
}

pub fn elicitation_decline() -> Value {
    json!({ "action": "decline" })
}

/// How an elicitation field's selected option value is serialized into `content`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElicitationKind {
    Choice,
    MultiChoice,
    Boolean,
}

impl ElicitationKind {
    pub fn to_json_values(self, selected: &[&str]) -> Value {
        match self {
            Self::Choice => Value::String(selected.first().copied().unwrap_or_default().to_string()),
            Self::MultiChoice => json!(selected),
            Self::Boolean => Value::Bool(selected.first().copied() == Some("true")),
        }
    }
}

/// One parsed option from an ACP form elicitation request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElicitationOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// One parsed question field from an ACP form elicitation request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElicitationField {
    pub title: String,
    pub field: String,
    pub kind: ElicitationKind,
    pub options: Vec<ElicitationOption>,
}

fn parse_titled_options(entries: &[Value]) -> Vec<ElicitationOption> {
    entries
        .iter()
        .filter_map(|item| {
            let val = item["const"].as_str()?;
            let label = item["title"].as_str().filter(|s| !s.trim().is_empty()).unwrap_or(val).to_string();
            let description = item["description"].as_str().filter(|s| !s.trim().is_empty()).map(String::from);
            Some(ElicitationOption { value: val.to_string(), label, description })
        })
        .collect()
}

fn parse_enum_options(entries: &[Value], names: Option<&Vec<Value>>) -> Vec<ElicitationOption> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let val = v.as_str()?;
            let label = names.and_then(|n| n.get(i)).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).unwrap_or(val).to_string();
            Some(ElicitationOption { value: val.to_string(), label, description: None })
        })
        .collect()
}

/// Extract all selectable question fields from an ACP form elicitation request, matching T3 Code's `parseElicitationForm`.
pub fn parse_elicitation(params: &Value) -> Vec<ElicitationField> {
    if params["mode"].as_str().is_some_and(|m| m != "form") {
        return vec![];
    }
    let Some(props) = params.pointer("/requestedSchema/properties").and_then(Value::as_object) else {
        return vec![];
    };
    let top_message = params["message"].as_str().map(str::trim).filter(|s| !s.is_empty());
    let mut fields = Vec::new();
    for (field, schema) in props {
        let field_title = schema["title"]
            .as_str()
            .or_else(|| schema["description"].as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let title = match (top_message, field_title) {
            (_, Some(t)) if props.len() > 1 => t.to_string(),
            (Some(m), _) => m.to_string(),
            (None, Some(t)) => t.to_string(),
            (None, None) => "The agent has a question".to_string(),
        };
        let parsed = match schema["type"].as_str() {
            Some("string") => {
                let opts = if let Some(one_of) = schema["oneOf"].as_array() {
                    parse_titled_options(one_of)
                } else if let Some(enums) = schema["enum"].as_array() {
                    parse_enum_options(enums, schema["enumNames"].as_array())
                } else {
                    vec![]
                };
                (!opts.is_empty()).then_some((ElicitationKind::Choice, opts))
            }
            Some("array") => {
                let items = &schema["items"];
                let opts = if let Some(any_of) = items["anyOf"].as_array() {
                    parse_titled_options(any_of)
                } else if let Some(enums) = items["enum"].as_array() {
                    parse_enum_options(enums, items["enumNames"].as_array())
                } else {
                    vec![]
                };
                (!opts.is_empty()).then_some((ElicitationKind::MultiChoice, opts))
            }
            Some("boolean") => Some((
                ElicitationKind::Boolean,
                vec![
                    ElicitationOption { value: "true".into(), label: "Yes".into(), description: None },
                    ElicitationOption { value: "false".into(), label: "No".into(), description: None },
                ],
            )),
            _ => None,
        };
        if let Some((kind, options)) = parsed {
            fields.push(ElicitationField { title, field: field.clone(), kind, options });
        }
    }
    fields
}

/// The answer to a permission request: the option picked, or none when the user declined to choose.
pub fn permission_outcome(option: Option<&str>) -> Value {
    match option {
        Some(id) => json!({ "outcome": { "outcome": "selected", "optionId": id } }),
        None => json!({ "outcome": { "outcome": "cancelled" } }),
    }
}

/// A question m4code's own Claude Code bridge asks can take the user's own words instead of one of its options. ACP has
/// no such answer, so the bridge marks the question and reads the words from the answer's `_meta` (`own_answer`).
pub fn takes_own_answer(params: &Value) -> bool {
    params["_meta"]["m4code"]["freeText"].as_bool() == Some(true)
}

pub fn is_multi_select(params: &Value) -> bool {
    params["_meta"]["m4code"]["multiSelect"].as_bool() == Some(true)
}

pub fn own_answer(text: &str) -> Value {
    json!({ "outcome": { "outcome": "selected", "optionId": "other" }, "_meta": { "m4code": { "text": text } } })
}

pub fn multi_answer(option_ids: &[String], text: Option<&str>) -> Value {
    let mut meta = serde_json::Map::new();
    meta.insert("optionIds".into(), json!(option_ids));
    if let Some(t) = text.map(str::trim).filter(|s| !s.is_empty()) {
        meta.insert("text".into(), json!(t));
    }
    json!({
        "outcome": { "outcome": "selected", "optionId": option_ids.first().map(String::as_str).unwrap_or("other") },
        "_meta": { "m4code": meta },
    })
}

pub fn is_tool_permission(params: &Value) -> bool {
    matches!(params["toolCall"]["kind"].as_str(), Some("read" | "edit" | "delete" | "move" | "search" | "execute" | "think" | "fetch" | "switch_mode"))
}

pub fn reopen_method(capabilities: &Value, preferred: super::Reopen) -> Result<super::Reopen, &'static str> {
    let load = capabilities["loadSession"].as_bool() == Some(true);
    let resume = capabilities.pointer("/sessionCapabilities/resume").is_some_and(Value::is_object);
    match (preferred, load, resume) {
        (super::Reopen::Load, true, _) | (super::Reopen::Resume, true, false) => Ok(super::Reopen::Load),
        (_, _, true) => Ok(super::Reopen::Resume),
        _ => Err("the adapter advertised neither session/load nor session/resume"),
    }
}

fn handle(message: Value, pending: &Pending, outgoing: &mpsc::Sender<Option<Value>>, events: &async_mpsc::UnboundedSender<Event>) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();
    match (method, id) {
        (None, Some(id)) => {
            let Some(tx) = id.as_i64().and_then(|id| pending.lock().unwrap().remove(&id)) else { return };
            let reply = match message.get("error") {
                Some(e) => {
                    let error = RpcError { code: e["code"].as_i64(), message: e["message"].as_str().unwrap_or("error").into(), data: e["data"].clone() };
                    let message = if error.authentication_required() { format!("Authentication required: {}", error.message) } else { error.message.clone() };
                    let _ = events.unbounded_send(Event::RequestFailed(error));
                    Err(message)
                },
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(reply);
        }
        (Some("session/request_permission"), Some(id)) => {
            let _ = events.unbounded_send(Event::Permission { id, params: message["params"].clone() });
        }
        (Some("elicitation/create" | "session/elicitation"), Some(id)) => {
            let _ = events.unbounded_send(Event::Elicitation { id, params: message["params"].clone() });
        }
        // Every other request gets an answer so the agent never waits for one: m4code offers no file system or
        // terminal, and extensions it doesn't know are refused.
        (Some(_), Some(id)) => {
            let _ = outgoing.send(Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}})));
        }
        (Some("auth/status_update"), None) => { let _ = events.unbounded_send(Event::Authentication(message["params"].clone())); }
        (Some("session/update"), None) => {
            let params = &message["params"];
            let _ = events.unbounded_send(Event::Update {
                session_id: params["sessionId"].as_str().map(String::from),
                update: params["update"].clone(),
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_process_exits_keep_the_actual_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let (program, args) = if cfg!(windows) {
            (PathBuf::from("cmd.exe"), vec!["/C".into(), "exit 7".into()])
        } else { (PathBuf::from("/bin/sh"), vec!["-c".into(), "exit 7".into()]) };
        let (events, mut incoming) = async_mpsc::unbounded();
        let _connection = Connection::start(&Launch { program, args, cwd: dir.path().into(), ..Default::default() }, events).unwrap();
        assert!(matches!(futures::executor::block_on(incoming.next()), Some(Event::ProcessExited { code: Some(7), .. })));
    }

    #[test]
    fn broken_stdin_resolves_requests_even_while_stdout_is_open() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> { Err(std::io::ErrorKind::BrokenPipe.into()) }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let (reader, _stdout_stays_open) = std::io::pipe().unwrap();
        let (events, mut incoming) = async_mpsc::unbounded();
        let connection = Connection::connect(reader, Broken, None::<std::io::Empty>, events);
        let result = futures::executor::block_on(connection.request("initialize", json!({})));
        assert!(result.unwrap_err().contains("connection"));
        assert!(connection.pending.lock().unwrap().is_empty());
        assert!(matches!(futures::executor::block_on(incoming.next()), Some(Event::Exited(message)) if message.contains("connection")));
        assert!(futures::executor::block_on(connection.request("after-failure", json!({}))).is_err());
    }

    #[test]
    fn rpc_errors_keep_authentication_codes_and_data() {
        let pending: Pending = Default::default();
        let (tx, rx) = oneshot::channel();
        pending.lock().unwrap().insert(7, tx);
        let (outgoing, _queue) = mpsc::channel();
        let (events, mut incoming) = async_mpsc::unbounded();
        handle(json!({"id": 7, "error": {"code": -32000, "message": "Credentials unavailable", "data": {"provider": "codex"}}}), &pending, &outgoing, &events);
        assert!(futures::executor::block_on(rx).unwrap().unwrap_err().starts_with("Authentication required:"));
        let Some(Event::RequestFailed(error)) = futures::executor::block_on(incoming.next()) else { panic!("missing structured error") };
        assert!(error.authentication_required());
        assert_eq!(error.data["provider"], "codex");
        assert!(!looks_like_auth_error("The model exceeded its token limit"));
        assert!(!looks_like_auth_error("HTTP 403: repository access denied"));
        assert!(looks_like_auth_error("Your refresh token has expired"));
    }

    #[test]
    fn child_edit_permission_does_not_answer_user_questions() {
        assert!(is_tool_permission(&json!({"toolCall":{"kind":"edit"}})));
        assert!(!is_tool_permission(&json!({"toolCall":{"kind":"other"},"options":[{"kind":"allow_once"},{"kind":"allow_once"}]})));
        assert!(!is_tool_permission(&json!({"options":[]})));
    }

    #[test]
    fn terminating_cancels_pending_requests_and_rejects_new_ones() {
        let (reader, _stdout_writer) = std::io::pipe().unwrap();
        let (mut stdin_reader, writer) = std::io::pipe().unwrap();
        let (events, _incoming) = async_mpsc::unbounded();
        let connection = Connection::connect(reader, writer, None::<std::io::Empty>, events);
        let pending = connection.request("never-answered", json!({}));
        connection.closed.store(true, Ordering::Release);
        connection.terminate();
        assert!(connection.pending.lock().unwrap().is_empty(), "EOF must not prevent pending-request cleanup");
        assert!(futures::executor::block_on(pending).is_err());
        assert!(futures::executor::block_on(connection.request("after-close", json!({}))).is_err());
        let mut buf = String::new();
        stdin_reader.read_to_string(&mut buf).unwrap();
        assert!(buf.contains("never-answered"), "terminate must flush and close the child's stdin while stdout is still open");
    }

    #[test]
    fn reopen_uses_negotiated_capabilities_and_refuses_unsupported_operations() {
        use crate::harness::Reopen;
        assert_eq!(reopen_method(&json!({"loadSession": true}), Reopen::Resume), Ok(Reopen::Load));
        assert_eq!(reopen_method(&json!({"sessionCapabilities": {"resume": {}}}), Reopen::Load), Ok(Reopen::Resume));
        let both = json!({"loadSession":true,"sessionCapabilities":{"resume":{}}});
        assert_eq!(reopen_method(&both, Reopen::Load), Ok(Reopen::Load));
        assert_eq!(reopen_method(&both, Reopen::Resume), Ok(Reopen::Resume));
        assert!(reopen_method(&json!({}), Reopen::Load).is_err());
    }
    use futures::StreamExt;

    /// A fake agent on the other end of two pipes: it answers `initialize` and `session/new`, and for a prompt
    /// streams a chunk, asks permission, then ends the turn once it is answered.
    fn fake_agent(from_client: std::io::PipeReader, mut to_client: std::io::PipeWriter) {
        std::thread::spawn(move || {
            let mut send = |v: Value| {
                to_client.write_all(format!("{v}\n").as_bytes()).unwrap();
            };
            let mut prompt_id = None;
            for line in BufReader::new(from_client).lines().map_while(Result::ok) {
                let m: Value = serde_json::from_str(&line).unwrap();
                match m["method"].as_str() {
                    Some("initialize") => {
                        send(json!({"jsonrpc": "2.0", "method": "auth/status_update", "params": {}}));
                        send(json!({"jsonrpc": "2.0", "id": m["id"], "result": {"protocolVersion": 1}}));
                    }
                    Some("session/new") => send(json!({"jsonrpc": "2.0", "id": m["id"], "result": {"sessionId": "s1"}})),
                    Some("session/prompt") => {
                        prompt_id = Some(m["id"].clone());
                        send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}}}));
                        send(json!({"jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission", "params": {"options": [{"optionId": "allow", "kind": "allow_once"}]}}));
                        send(json!({"jsonrpc": "2.0", "id": 99, "method": "fs/read_text_file", "params": {}}));
                    }
                    None if m["id"] == "perm-1" => {
                        assert_eq!(m["result"]["outcome"]["optionId"], "allow");
                        send(json!({"jsonrpc": "2.0", "id": prompt_id.take().unwrap(), "result": {"stopReason": "end_turn"}}));
                    }
                    None if m["id"] == 99 => assert_eq!(m["error"]["code"], -32601),
                    _ => {}
                }
            }
        });
    }

    #[test]
    fn a_turn_streams_asks_permission_and_ends() {
        let (from_agent, to_client) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        fake_agent(from_client, to_client);
        let (tx, mut events) = async_mpsc::unbounded();
        let agent = Connection::connect(from_agent, to_agent, None::<std::io::PipeReader>, tx);
        futures::executor::block_on(async {
            agent.request("initialize", initialize_params()).await.unwrap();
            assert!(matches!(events.next().await, Some(Event::Authentication(_))));
            let session = agent.request("session/new", json!({"cwd": "/w", "mcpServers": []})).await.unwrap();
            assert_eq!(session["sessionId"], "s1");
            let turn = agent.request("session/prompt", json!({"sessionId": "s1", "prompt": []}));
            let Some(Event::Update { session_id, update }) = events.next().await else { panic!("no update") };
            assert_eq!(session_id.as_deref(), Some("s1"));
            assert_eq!(update["content"]["text"], "hi");
            let Some(Event::Permission { id, .. }) = events.next().await else { panic!("no permission request") };
            agent.respond(id, permission_outcome(Some("allow")));
            assert_eq!(turn.await.unwrap()["stopReason"], "end_turn");
        });
        drop(agent);
    }

    #[test]
    fn a_cancelled_permission_says_so() {
        assert_eq!(permission_outcome(None), json!({"outcome": {"outcome": "cancelled"}}));
    }

    #[test]
    fn elicitation_parses_one_of_and_enum_options() {
        let one_of = json!({
            "message": "Which database?",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "db": {
                        "type": "string",
                        "oneOf": [
                            {"const": "pg", "title": "PostgreSQL", "description": "Relational"},
                            {"const": "sqlite", "title": "SQLite"}
                        ]
                    }
                }
            }
        });
        let fields = parse_elicitation(&one_of);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].title, "Which database?");
        assert_eq!(fields[0].field, "db");
        assert_eq!(fields[0].kind, ElicitationKind::Choice);
        assert_eq!(
            fields[0].options,
            vec![
                ElicitationOption { value: "pg".into(), label: "PostgreSQL".into(), description: Some("Relational".into()) },
                ElicitationOption { value: "sqlite".into(), label: "SQLite".into(), description: None },
            ]
        );
        let mut content = serde_json::Map::new();
        content.insert("db".into(), ElicitationKind::Choice.to_json_values(&["pg"]));
        content.insert("confirm".into(), ElicitationKind::Boolean.to_json_values(&["true"]));
        content.insert("tags".into(), ElicitationKind::MultiChoice.to_json_values(&["fast", "safe"]));
        assert_eq!(elicitation_accept(content), json!({"action": "accept", "content": {"db": "pg", "confirm": true, "tags": ["fast", "safe"]}}));
        assert_eq!(elicitation_cancel(), json!({"action": "cancel"}));
        assert_eq!(elicitation_decline(), json!({"action": "decline"}));
    }

    #[test]
    fn only_a_marked_question_takes_the_users_own_words() {
        assert!(takes_own_answer(&json!({"_meta": {"m4code": {"freeText": true}}, "options": []})));
        assert!(!takes_own_answer(&json!({"options": []})), "a plain ACP permission question");
        assert!(is_multi_select(&json!({"_meta": {"m4code": {"multiSelect": true}}, "options": []})));
        assert!(!is_multi_select(&json!({"options": []})));
        assert_eq!(own_answer("blue")["_meta"]["m4code"]["text"], "blue");
        assert_eq!(own_answer("blue")["outcome"]["outcome"], "selected");
        let multi = multi_answer(&["0".into(), "2".into()], Some("extra"));
        assert_eq!(multi["outcome"]["outcome"], "selected");
        assert_eq!(multi["_meta"]["m4code"]["optionIds"], json!(["0", "2"]));
        assert_eq!(multi["_meta"]["m4code"]["text"], "extra");
    }
}
