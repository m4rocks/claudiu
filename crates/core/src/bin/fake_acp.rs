//! A scripted stand-in for the ACP adapter, used by the core's tests.
//!
//! It speaks newline-delimited JSON-RPC on stdio and plays the script named by `FAKE_ACP_SCRIPT`:
//! `{"turns": [{"steps": [...], "stop": "end_turn"}]}`, one entry per `session/prompt`, in order.
//! A step is one of:
//! - `{"update": {...}}`: a `session/update` notification carrying that update;
//! - `{"notify": {"method": "...", "params": {...}}}`: any notification;
//! - `{"request": {"method": "...", "params": {...}}}`: a request to the client, waits for its reply;
//! - `{"sleep_ms": 50}`;
//! - `{"wait_cancel": true}`: blocks until `session/cancel` arrives; the turn then ends as `cancelled`.
//!
//! A subagent adapter (`CLAUDIU_PROFILE` set by the core) plays `{"profiles": {"<profile>": {"turns": [...]}}}` instead.
//! Turns beyond the script end at once with `end_turn`.
//! If `FAKE_ACP_LOG` names a file, every message received is appended to it as one JSON line.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

struct Fake {
    input: std::io::Lines<std::io::StdinLock<'static>>,
    out: std::io::Stdout,
    session: String,
    cancelled: bool,
    next_request: u64,
    log: Option<String>,
}

impl Fake {
    fn send(&mut self, v: Value) {
        let mut out = self.out.lock();
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }

    fn read(&mut self) -> Option<Value> {
        loop {
            let line = self.input.next()?.ok()?;
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if let Some(path) = &self.log {
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                        // One write, so lines from two adapters never interleave.
                        let _ = f.write_all(format!("{v}
").as_bytes());
                    }
                }
                return Some(v);
            }
        }
    }

    fn reply(&mut self, id: Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn notice(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Notes a `session/cancel` that arrives while a turn is running.
    fn seen(&mut self, msg: &Value) {
        if msg["method"] == "session/cancel" {
            self.cancelled = true;
        }
    }

    fn step(&mut self, step: &Value) {
        if let Some(update) = step.get("update") {
            let session = self.session.clone();
            self.notice("session/update", json!({"sessionId": session, "update": update}));
        } else if let Some(n) = step.get("notify") {
            self.notice(n["method"].as_str().unwrap_or(""), n["params"].clone());
        } else if let Some(r) = step.get("request") {
            self.next_request += 1;
            let id = json!(format!("fake-{}", self.next_request));
            self.send(json!({"jsonrpc": "2.0", "id": id, "method": r["method"], "params": r["params"]}));
            while let Some(msg) = self.read() {
                self.seen(&msg);
                if msg["id"] == id && msg.get("method").is_none() {
                    break;
                }
            }
        } else if let Some(ms) = step.get("sleep_ms").and_then(Value::as_u64) {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        } else if step.get("wait_cancel").is_some() {
            while !self.cancelled {
                let Some(msg) = self.read() else { return };
                self.seen(&msg);
            }
        }
    }
}

fn main() {
    let path = std::env::var("FAKE_ACP_SCRIPT").expect("FAKE_ACP_SCRIPT names the script");
    let script: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("script is readable")).expect("script is JSON");
    let profile = std::env::var("CLAUDIU_PROFILE").ok();
    let own = profile.as_deref().and_then(|p| script["profiles"].get(p)).unwrap_or(&script);
    let mut turns = own["turns"].as_array().cloned().unwrap_or_default().into_iter();
    let mut fake = Fake {
        input: std::io::stdin().lock().lines(),
        out: std::io::stdout(),
        session: String::new(),
        cancelled: false,
        next_request: 0,
        log: std::env::var("FAKE_ACP_LOG").ok(),
    };
    let mut created = 0;
    while let Some(msg) = fake.read() {
        let id = msg["id"].clone();
        match msg["method"].as_str() {
            Some("initialize") => fake.reply(id, json!({"protocolVersion": 1, "agentCapabilities": {}})),
            Some("session/new") => {
                created += 1;
                fake.session = format!("fake-session-{created}");
                let session = fake.session.clone();
                let modes = ["default", "acceptEdits", "plan", "auto", "bypassPermissions"].map(|m| json!({"value": m, "name": m}));
                let models = ["haiku", "sonnet", "opus"].map(|m| json!({"value": m, "name": m}));
                let efforts = ["low", "medium", "high"].map(|m| json!({"value": m, "name": m}));
                fake.reply(
                    id,
                    json!({"sessionId": session, "configOptions": [
                        {"id": "mode", "category": "mode", "currentValue": "default", "options": modes},
                        {"id": "model", "category": "model", "currentValue": "sonnet", "options": models},
                        {"id": "effort", "category": "thought_level", "currentValue": "medium", "options": efforts},
                    ]}),
                );
            }
            Some("session/load") => {
                fake.session = msg["params"]["sessionId"].as_str().unwrap_or("").to_string();
                fake.reply(id, json!({}));
            }
            Some("session/prompt") => {
                fake.cancelled = false;
                let turn = turns.next().unwrap_or(json!({}));
                for step in turn["steps"].as_array().cloned().unwrap_or_default() {
                    fake.step(&step);
                }
                let stop = if fake.cancelled { "cancelled" } else { turn["stop"].as_str().unwrap_or("end_turn") };
                fake.reply(id, json!({"stopReason": stop}));
            }
            Some("session/cancel") => fake.cancelled = true,
            Some("session/set_mode" | "session/set_config_option") => fake.reply(id, json!({})),
            Some(_) if !id.is_null() => fake.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}})),
            _ => {}
        }
    }
}
