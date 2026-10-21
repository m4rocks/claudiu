#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use claudiu_core::{AdapterCommand, Config, Core, Event, GuardConfig};
use serde_json::{Value, json};
use tempfile::TempDir;

/// A core wired to the scripted fake adapter, with its data dir and a clock the test moves by hand.
pub struct Harness {
    pub dir: TempDir,
    pub core: Core,
    pub now: Arc<AtomicI64>,
}

pub const T0: i64 = 1_700_000_000_000;

pub fn config(dir: &Path, script: &Value, now: &Arc<AtomicI64>) -> Config {
    let script_path = dir.join("script.json");
    std::fs::write(&script_path, script.to_string()).unwrap();
    let clock = now.clone();
    let log = dir.join("fake.log");
    Config::new(dir.join("data"), AdapterCommand::new(env!("CARGO_BIN_EXE_fake-acp")).env("FAKE_ACP_SCRIPT", script_path).env("FAKE_ACP_LOG", log))
        .with_clock(Arc::new(move || clock.load(Ordering::SeqCst)))
        .with_guard(GuardConfig { toolchain_dirs: vec![dir.join("toolchain-cache")] })
        .with_canary_timeout(Duration::from_millis(400))
}

impl Harness {
    pub fn new(script: Value) -> Harness {
        Harness::with(script, |c| c)
    }

    /// Like `new`, with the core's config adjusted first.
    pub fn with(script: Value, adjust: impl FnOnce(Config) -> Config) -> Harness {
        let dir = TempDir::new().unwrap();
        let now = Arc::new(AtomicI64::new(T0));
        let core = Core::open(adjust(config(dir.path(), &script, &now))).unwrap();
        Harness { dir, core, now }
    }

    /// The same data dir opened again, as after a restart.
    pub fn reopen(self, script: Value) -> Harness {
        let Harness { dir, core, now } = self;
        core.shutdown();
        let core = Core::open(config(dir.path(), &script, &now)).unwrap();
        Harness { dir, core, now }
    }

    pub fn project(&self) -> PathBuf {
        let p = self.dir.path().join("project");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The params of every `method` request the fake adapter received, oldest first.
    pub fn adapter_calls(&self, method: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(self.dir.path().join("fake.log")).unwrap_or_default();
        text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|m| m["method"] == method).map(|m| m["params"].clone()).collect()
    }

    pub fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }
}

pub fn text_chunk(text: &str) -> Value {
    json!({"update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}})
}

/// Events from the stream until one of `kind` arrives, as their kinds.
pub fn until(rx: &Receiver<Event>, kind: &str) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let e = rx.recv_timeout(Duration::from_secs(20)).unwrap_or_else(|_| panic!("no `{kind}` event, saw {:?}", kinds(&seen)));
        let done = e.kind == kind;
        seen.push(e);
        if done {
            return seen;
        }
    }
}

pub fn kinds(events: &[Event]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

pub fn usage(used: i64, size: i64) -> Value {
    json!({"update": {"sessionUpdate": "usage_update", "used": used, "size": size}})
}

pub fn tool_call(id: &str, output: &str) -> Value {
    json!({"update": {"sessionUpdate": "tool_call", "toolCallId": id, "title": "Read file", "status": "completed", "rawOutput": output}})
}

/// One JSON POST over plain HTTP, as Claude Code makes for an http hook: `(status, parsed body)`.
pub fn post(url: &str, body: &Value) -> (u16, Value) {
    use std::io::{Read, Write};
    let rest = url.strip_prefix("http://").expect("http url");
    let (host, path) = rest.split_once('/').expect("url path");
    let text = body.to_string();
    let mut stream = std::net::TcpStream::connect(host).unwrap();
    write!(stream, "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status = raw.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

/// A PreToolUse call for `tool` as the hook sees it.
pub fn pre(core: &Core, id: &claudiu_core::SessionId, tool: &str, input: Value) -> Value {
    post(&core.hook_url(id, "pre"), &json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": input})).1
}

pub fn decision(reply: &Value) -> Option<&str> {
    reply["hookSpecificOutput"]["permissionDecision"].as_str()
}

pub fn reason(reply: &Value) -> &str {
    reply["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap_or("")
}

/// One MCP request to the session's `claudiu` server, as Claude Code makes it; returns the JSON-RPC `result`.
pub fn mcp(core: &Core, id: &claudiu_core::SessionId, method: &str, params: Value) -> Value {
    let (status, reply) = post(&core.mcp_url(id), &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}));
    assert_eq!(status, 200, "{method}: {reply}");
    reply["result"].clone()
}

/// The text of a tool call's result.
pub fn call_tool(core: &Core, id: &claudiu_core::SessionId, tool: &str, arguments: Value) -> String {
    let result = mcp(core, id, "tools/call", json!({"name": tool, "arguments": arguments}));
    result["content"][0]["text"].as_str().unwrap_or("").to_string()
}
