//! A long-lived Agent Client Protocol connection to a harness: newline-delimited JSON-RPC over the agent's stdio.
//! Requests return futures; what the agent sends unasked arrives as `Event`s. No GPUI here; `thread_view` drives it.
//! `claude::acp` is a different, one-shot client: Claude Code's subagents run through it.

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

/// What the agent sends without being asked.
#[derive(Debug)]
pub enum Event {
    /// A `session/update`'s `update` object.
    Update(Value),
    /// `session/request_permission`: answer it with `Connection::respond` and the request's id.
    Permission { id: Value, params: Value },
    /// The agent stopped; the text is its last words on stderr, if any.
    Exited(String),
    /// A `session/prompt`'s reply. The thread queues it itself once the reply arrives, which is after the reader
    /// queued every update the agent sent before replying, so a turn never ends ahead of its last words.
    TurnEnded(Result<Value, String>),
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

/// A running harness. Dropping it ends the process.
pub struct Connection {
    outgoing: mpsc::Sender<Value>,
    pending: Pending,
    next_id: AtomicI64,
    child: Mutex<Option<Child>>,
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
        let mut connection = Self::connect(stdout, stdin, Some(stderr), events);
        connection.child = Mutex::new(Some(child));
        Ok(connection)
    }

    /// Talk to an agent over `reader` and `writer` (its stdout and stdin).
    pub fn connect(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static, stderr: Option<impl Read + Send + 'static>, events: async_mpsc::UnboundedSender<Event>) -> Connection {
        let (outgoing, queue) = mpsc::channel::<Value>();
        std::thread::spawn(move || {
            let mut writer = writer;
            while let Ok(message) = queue.recv() {
                let Ok(mut line) = serde_json::to_vec(&message) else { continue };
                line.push(b'\n');
                if writer.write_all(&line).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        });
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
        let pending: Pending = Default::default();
        let closed = Arc::new(AtomicBool::new(false));
        {
            let (pending, outgoing) = (pending.clone(), outgoing.clone());
            let closed = closed.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(reader).lines().map_while(Result::ok) {
                    // Some servers print a banner or a log line on stdout; only JSON is the protocol.
                    if let Ok(message) = serde_json::from_str::<Value>(&line) {
                        handle(message, &pending, &outgoing, &events);
                    }
                }
                closed.store(true, Ordering::Release);
                pending.lock().unwrap().clear();
                // Give stderr a moment to deliver the reason before reporting it.
                std::thread::sleep(Duration::from_millis(200));
                let words = last_words.lock().unwrap().iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
                let _ = events.unbounded_send(Event::Exited(words));
            });
        }
        Connection { outgoing, pending, next_id: AtomicI64::new(1), child: Mutex::new(None), closed }
    }

    pub fn request(&self, method: &str, params: Value) -> impl Future<Output = Result<Value, String>> + use<> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let mut pending = self.pending.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            let _ = tx.send(Err("the agent stopped".into()));
        } else {
            pending.insert(id, tx);
            let _ = self.outgoing.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        }
        async move { rx.await.unwrap_or_else(|_| Err("the agent stopped".into())) }
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.outgoing.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Answer a request the agent made.
    pub fn respond(&self, id: Value, result: Value) {
        let _ = self.outgoing.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    pub fn terminate(&self) {
        self.closed.store(true, Ordering::Release);
        self.pending.lock().unwrap().clear();
        if let Some(mut child) = self.child.lock().unwrap().take() {
            // Off the UI thread: closing stdin is the polite stop, and one that lingers is ended.
            std::thread::spawn(move || {
                drop(child.stdin.take());
                for _ in 0..10 {
                    if child.try_wait().ok().flatten().is_some() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
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
/// agent, so the agent does those on its own.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
        "clientInfo": { "name": "m4code", "title": "m4code", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The answer to a permission request: the option picked, or none when the user declined to choose.
pub fn permission_outcome(option: Option<&str>) -> Value {
    match option {
        Some(id) => json!({ "outcome": { "outcome": "selected", "optionId": id } }),
        None => json!({ "outcome": { "outcome": "cancelled" } }),
    }
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

fn handle(message: Value, pending: &Pending, outgoing: &mpsc::Sender<Value>, events: &async_mpsc::UnboundedSender<Event>) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();
    match (method, id) {
        (None, Some(id)) => {
            let Some(tx) = id.as_i64().and_then(|id| pending.lock().unwrap().remove(&id)) else { return };
            let reply = match message.get("error") {
                Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(reply);
        }
        (Some("session/request_permission"), Some(id)) => {
            let _ = events.unbounded_send(Event::Permission { id, params: message["params"].clone() });
        }
        // Every other request gets an answer so the agent never waits for one: m4code offers no file system or
        // terminal, and extensions it doesn't know are refused.
        (Some(_), Some(id)) => {
            let _ = outgoing.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}));
        }
        (Some("session/update"), None) => {
            let _ = events.unbounded_send(Event::Update(message["params"]["update"].clone()));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_edit_permission_does_not_answer_user_questions() {
        assert!(is_tool_permission(&json!({"toolCall":{"kind":"edit"}})));
        assert!(!is_tool_permission(&json!({"toolCall":{"kind":"other"},"options":[{"kind":"allow_once"},{"kind":"allow_once"}]})));
        assert!(!is_tool_permission(&json!({"options":[]})));
    }

    #[test]
    fn terminating_cancels_pending_requests_and_rejects_new_ones() {
        let (reader, _writer) = std::io::pipe().unwrap();
        let (events, _incoming) = async_mpsc::unbounded();
        let connection = Connection::connect(reader, Vec::<u8>::new(), None::<std::io::Empty>, events);
        let pending = connection.request("never-answered", json!({}));
        connection.closed.store(true, Ordering::Release);
        connection.terminate();
        assert!(connection.pending.lock().unwrap().is_empty(), "EOF must not prevent pending-request cleanup");
        assert!(futures::executor::block_on(pending).is_err());
        assert!(futures::executor::block_on(connection.request("after-close", json!({}))).is_err());
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
            let session = agent.request("session/new", json!({"cwd": "/w", "mcpServers": []})).await.unwrap();
            assert_eq!(session["sessionId"], "s1");
            let turn = agent.request("session/prompt", json!({"sessionId": "s1", "prompt": []}));
            let Some(Event::Update(update)) = events.next().await else { panic!("no update") };
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
}
