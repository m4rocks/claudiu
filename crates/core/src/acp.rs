//! One adapter process and the newline-delimited JSON-RPC (ACP) spoken to it over stdio.
//!
//! A reader thread parses what the adapter prints; replies go to the callback registered with the request,
//! everything else goes to the handler given at spawn. Closing stdin makes the adapter exit.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

/// How to start an adapter: the real one is `node` running the pinned package; tests use the fake.
#[derive(Clone, Debug)]
pub struct AdapterCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl AdapterCommand {
    pub fn new(program: impl Into<PathBuf>) -> AdapterCommand {
        AdapterCommand { program: program.into(), args: Vec::new(), env: Vec::new() }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> AdapterCommand {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl AsRef<std::ffi::OsStr>) -> AdapterCommand {
        self.env.push((key.into(), value.as_ref().to_string_lossy().into_owned()));
        self
    }
}

/// What the adapter sent that is not a reply.
#[derive(Debug)]
pub enum Msg {
    Notification { method: String, params: Value },
    /// The adapter asks something and waits; answer through the [`Responder`].
    Request { id: Value, method: String, params: Value },
    /// The process ended; the text is the tail of its stderr.
    Exit(String),
}

type Reply = Box<dyn FnOnce(Result<Value, String>) + Send>;

/// Answers requests the adapter makes of us; handed to the message handler.
#[derive(Clone)]
pub struct Responder(Sender<String>);

impl Responder {
    pub fn respond_error(&self, id: Value, code: i64, message: &str) {
        let _ = self.0.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string());
    }
}

pub struct Conn {
    lines: Mutex<Option<Sender<String>>>,
    child: Mutex<Child>,
    next: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, Reply>>>,
}

impl Conn {
    pub fn spawn(cmd: &AdapterCommand, on_msg: impl FnMut(Msg, &Responder) + Send + 'static) -> Result<Conn> {
        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args).envs(cmd.env.iter().map(|(k, v)| (k, v))).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
        let mut child = command.spawn().with_context(|| format!("starting the adapter {}", cmd.program.display()))?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");

        let (lines, queue) = channel::<String>();
        let responder = Responder(lines.clone());
        std::thread::spawn(move || {
            for line in queue {
                if writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });

        let tail = Arc::new(Mutex::new(String::new()));
        let tail_writer = tail.clone();
        let stderr_thread = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            let keep = text.len().saturating_sub(2000);
            *tail_writer.lock().unwrap() = text[text.ceil_char_boundary(keep)..].to_string();
        });

        let pending: Arc<Mutex<HashMap<u64, Reply>>> = Arc::default();
        let waiting = pending.clone();
        let mut on_msg = on_msg;
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                let method = v.get("method").and_then(Value::as_str).map(String::from);
                let params = v.get("params").cloned().unwrap_or(Value::Null);
                match (v.get("id").cloned(), method) {
                    (Some(id), Some(method)) => on_msg(Msg::Request { id, method, params }, &responder),
                    (None, Some(method)) => on_msg(Msg::Notification { method, params }, &responder),
                    (Some(id), None) => {
                        let reply = id.as_u64().and_then(|n| waiting.lock().unwrap().remove(&n));
                        if let Some(reply) = reply {
                            reply(match v.get("error") {
                                Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("request failed").to_string()),
                                None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                            });
                        }
                    }
                    (None, None) => {}
                }
            }
            for (_, reply) in waiting.lock().unwrap().drain() {
                reply(Err("the adapter exited".into()));
            }
            let _ = stderr_thread.join();
            on_msg(Msg::Exit(tail.lock().unwrap().trim().to_string()), &responder);
        });

        Ok(Conn { lines: Mutex::new(Some(lines)), child: Mutex::new(child), next: AtomicU64::new(1), pending })
    }

    fn send(&self, v: Value) {
        if let Some(lines) = self.lines.lock().unwrap().as_ref() {
            let _ = lines.send(v.to_string());
        }
    }

    /// Sends a request and returns at once; `reply` runs on the reader thread when the answer (or the exit) arrives.
    pub fn call_async(&self, method: &str, params: Value, reply: impl FnOnce(Result<Value, String>) + Send + 'static) {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.pending.lock().unwrap().insert(id, Box::new(reply));
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    }

    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let (tx, rx) = channel();
        self.call_async(method, params, move |r| {
            let _ = tx.send(r);
        });
        rx.recv_timeout(timeout).map_err(|_| anyhow!("{method} timed out"))?.map_err(|e| anyhow!("{method}: {e}"))
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Ends the adapter: closes its stdin, gives it a moment to leave, then kills it.
    pub fn close(&self) {
        self.lines.lock().unwrap().take();
        let mut child = self.child.lock().unwrap();
        for _ in 0..50 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.close();
    }
}
