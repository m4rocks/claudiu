#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use claudiu_core::{AdapterCommand, Config, Core, Event};
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
    Config::new(dir.join("data"), AdapterCommand::new(env!("CARGO_BIN_EXE_fake-acp")).env("FAKE_ACP_SCRIPT", script_path))
        .with_clock(Arc::new(move || clock.load(Ordering::SeqCst)))
}

impl Harness {
    pub fn new(script: Value) -> Harness {
        let dir = TempDir::new().unwrap();
        let now = Arc::new(AtomicI64::new(T0));
        let core = Core::open(config(dir.path(), &script, &now)).unwrap();
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
