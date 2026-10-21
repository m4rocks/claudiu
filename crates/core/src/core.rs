//! The command API: start sessions, send prompts, cancel, and read the event stream and history.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::acp::{AdapterCommand, Conn, Msg};
use crate::event::{Event, SessionId, SessionRecord, UsageRow, kind};
use crate::store::Store;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct Config {
    pub data_dir: PathBuf,
    pub adapter: AdapterCommand,
    pub clock: Clock,
}

impl Config {
    pub fn new(data_dir: impl Into<PathBuf>, adapter: AdapterCommand) -> Config {
        let clock: Clock = Arc::new(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
        });
        Config { data_dir: data_dir.into(), adapter, clock }
    }

    /// A clock the caller controls, so retention and timers can be tested without waiting.
    pub fn with_clock(mut self, clock: Clock) -> Config {
        self.clock = clock;
        self
    }
}

/// Writes events to the store and tells subscribers, in one order.
struct Recorder {
    store: Store,
    clock: Clock,
    subscribers: Mutex<Vec<Sender<Event>>>,
    order: Mutex<()>,
}

impl Recorder {
    fn record(&self, session: &SessionId, kind: &str, payload: Value) -> Result<Event> {
        let _one_at_a_time = self.order.lock().unwrap();
        let ts_ms = (self.clock)();
        let id = self.store.append_event(session, ts_ms, kind, &payload)?;
        let event = Event { id, session: session.clone(), ts_ms, kind: kind.to_string(), payload };
        self.subscribers.lock().unwrap().retain(|s| s.send(event.clone()).is_ok());
        Ok(event)
    }
}

struct Live {
    conn: Conn,
    adapter_session_id: String,
    /// A turn is in flight.
    running: Arc<AtomicBool>,
    /// We are ending the adapter on purpose, so its exit is not a crash.
    closing: Arc<AtomicBool>,
}

pub struct Core {
    rec: Arc<Recorder>,
    adapter: AdapterCommand,
    live: Mutex<HashMap<SessionId, Arc<Live>>>,
}

const CALL_TIMEOUT: Duration = Duration::from_secs(30);

impl Core {
    pub fn open(config: Config) -> Result<Core> {
        std::fs::create_dir_all(&config.data_dir)?;
        let store = Store::open(&config.data_dir.join("core.db"))?;
        let rec = Recorder { store, clock: config.clock, subscribers: Mutex::default(), order: Mutex::new(()) };
        Ok(Core { rec: Arc::new(rec), adapter: config.adapter, live: Mutex::default() })
    }

    /// Every event recorded from now on, in order.
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = channel();
        self.rec.subscribers.lock().unwrap().push(tx);
        rx
    }

    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        self.rec.store.sessions()
    }

    pub fn events(&self, session: &SessionId) -> Result<Vec<Event>> {
        self.rec.store.events(session)
    }

    pub fn usage(&self, session: &SessionId) -> Result<Vec<UsageRow>> {
        self.rec.store.usage(session)
    }

    /// Keeps full tool output aside (the context firewall's handles build on this); it expires after 14 days.
    pub fn put_blob(&self, session: &SessionId, data: &[u8]) -> Result<i64> {
        self.rec.store.put_blob(session, (self.rec.clock)(), data)
    }

    pub fn blob(&self, id: i64) -> Result<Option<Vec<u8>>> {
        self.rec.store.blob(id)
    }

    /// Applies the retention rules; the app calls it at startup and now and then.
    pub fn prune_expired(&self) -> Result<()> {
        self.rec.store.prune((self.rec.clock)())
    }

    pub fn start_session(&self, cwd: &Path) -> Result<SessionId> {
        let id = SessionId::new();
        let (running, closing) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let conn = self.spawn_adapter(&id, running.clone(), closing.clone())?;
        conn.call("initialize", json!({"protocolVersion": 1, "clientCapabilities": {}}), CALL_TIMEOUT)?;
        let cwd_text = cwd.to_string_lossy().into_owned();
        let made = conn.call("session/new", json!({"cwd": cwd_text, "mcpServers": []}), CALL_TIMEOUT)?;
        let adapter_session_id = made["sessionId"].as_str().ok_or_else(|| anyhow!("session/new returned no session id"))?.to_string();

        let created_ms = (self.rec.clock)();
        self.rec.store.insert_session(&SessionRecord {
            id: id.clone(),
            cwd: cwd_text.clone(),
            title: None,
            adapter_session_id: Some(adapter_session_id.clone()),
            created_ms,
        })?;
        self.rec.record(&id, kind::SESSION_STARTED, json!({"cwd": cwd_text, "adapter_session_id": adapter_session_id}))?;
        self.live.lock().unwrap().insert(id.clone(), Arc::new(Live { conn, adapter_session_id, running, closing }));
        Ok(id)
    }

    fn spawn_adapter(&self, id: &SessionId, running: Arc<AtomicBool>, closing: Arc<AtomicBool>) -> Result<Conn> {
        let rec = self.rec.clone();
        let id = id.clone();
        let conn = Conn::spawn(&self.adapter, move |msg, responder| match msg {
            Msg::Notification { method, params } if method == "session/update" => {
                let update = params["update"].clone();
                let kind = match update["sessionUpdate"].as_str() {
                    Some("tool_call" | "tool_call_update") => kind::TOOL_CALL,
                    _ => kind::AGENT_UPDATE,
                };
                if update["sessionUpdate"] == "usage_update" {
                    let _ = rec.store.record_usage(&id, (rec.clock)(), update["used"].as_i64(), update["size"].as_i64(), &update);
                }
                let _ = rec.record(&id, kind, update);
            }
            Msg::Notification { method, params } => {
                let _ = rec.record(&id, kind::AGENT_UPDATE, json!({"method": method, "params": params}));
            }
            Msg::Request { id: request_id, method, params } => {
                let _ = rec.record(&id, kind::AGENT_REQUEST, json!({"method": method, "params": params}));
                responder.respond_error(request_id, -32601, "not handled yet");
            }
            Msg::Exit(stderr) => {
                running.store(false, Ordering::SeqCst);
                if !closing.load(Ordering::SeqCst) {
                    let _ = rec.record(&id, kind::ADAPTER_EXIT, json!({"stderr": stderr}));
                }
            }
        })?;
        Ok(conn)
    }

    fn live(&self, id: &SessionId) -> Result<Arc<Live>> {
        self.live.lock().unwrap().get(id).cloned().ok_or_else(|| anyhow!("session {id} is not running"))
    }

    pub fn send_prompt(&self, id: &SessionId, text: &str) -> Result<()> {
        let live = self.live(id)?;
        if live.running.swap(true, Ordering::SeqCst) {
            bail!("a turn is already running in session {id}");
        }
        self.rec.record(id, kind::USER_PROMPT, json!({"text": text}))?;
        let (rec, session, running) = (self.rec.clone(), id.clone(), live.running.clone());
        live.conn.call_async(
            "session/prompt",
            json!({"sessionId": live.adapter_session_id, "prompt": [{"type": "text", "text": text}]}),
            move |result| {
                if !running.swap(false, Ordering::SeqCst) {
                    return;
                }
                let _ = match result {
                    Ok(v) => {
                        let stop = v["stopReason"].as_str().unwrap_or("end_turn");
                        let kind = if stop == "cancelled" { kind::INTERRUPTED } else { kind::TURN_ENDED };
                        rec.record(&session, kind, json!({"stop_reason": stop}))
                    }
                    Err(message) => rec.record(&session, kind::ERROR, json!({"message": message})),
                };
            },
        );
        Ok(())
    }

    /// Asks the adapter to stop the turn; the turn ends as `interrupted` when it answers.
    pub fn cancel(&self, id: &SessionId) -> Result<()> {
        let live = self.live(id)?;
        live.conn.notify("session/cancel", json!({"sessionId": live.adapter_session_id}));
        Ok(())
    }

    /// Quit: cancels running turns, waits briefly for them to settle (recording any that do not as interrupted),
    /// ends the adapters and flushes the store.
    pub fn shutdown(self) {
        let live: Vec<(SessionId, Arc<Live>)> = self.live.lock().unwrap().drain().collect();
        for (id, l) in &live {
            l.closing.store(true, Ordering::SeqCst);
            if l.running.load(Ordering::SeqCst) {
                l.conn.notify("session/cancel", json!({"sessionId": l.adapter_session_id}));
                for _ in 0..250 {
                    if !l.running.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                if l.running.swap(false, Ordering::SeqCst) {
                    let _ = self.rec.record(id, kind::INTERRUPTED, json!({"stop_reason": "quit"}));
                }
            }
            l.conn.close();
        }
        let _ = self.rec.store.flush();
    }
}
