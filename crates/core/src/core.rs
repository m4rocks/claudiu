//! The command API: start sessions, send prompts, cancel, and read the event stream and history.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::acp::{AdapterCommand, Conn, Msg};
use crate::event::{AccessRequest, Decision, Event, SessionId, SessionRecord, UsageRow, kind};
use crate::policy::{Policy, Scope};
use crate::guard::GuardConfig;
use crate::http::Server;
use crate::store::Store;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct Config {
    pub data_dir: PathBuf,
    pub adapter: AdapterCommand,
    pub clock: Clock,
    pub guard: GuardConfig,
    /// The Claude Code permission mode a task starts in.
    pub permission_mode: String,
    /// How long after the first prompt the guard hooks may stay silent before the task drops to ask mode.
    pub canary_timeout: Duration,
}

impl Config {
    pub fn new(data_dir: impl Into<PathBuf>, adapter: AdapterCommand) -> Config {
        let clock: Clock = Arc::new(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
        });
        Config { data_dir: data_dir.into(), adapter, clock, guard: GuardConfig::default(), permission_mode: "auto".into(), canary_timeout: Duration::from_secs(20) }
    }

    /// Where a task may write besides its project.
    pub fn with_guard(mut self, guard: GuardConfig) -> Config {
        self.guard = guard;
        self
    }

    pub fn with_canary_timeout(mut self, timeout: Duration) -> Config {
        self.canary_timeout = timeout;
        self
    }

    /// A clock the caller controls, so retention and timers can be tested without waiting.
    pub fn with_clock(mut self, clock: Clock) -> Config {
        self.clock = clock;
        self
    }
}

/// Writes events to the store and tells subscribers, in one order.
pub(crate) struct Recorder {
    pub(crate) store: Store,
    pub(crate) clock: Clock,
    subscribers: Mutex<Vec<Sender<Event>>>,
    order: Mutex<()>,
}

impl Recorder {
    pub(crate) fn record(&self, session: &SessionId, kind: &str, payload: Value) -> Result<Event> {
        let _one_at_a_time = self.order.lock().unwrap();
        let ts_ms = (self.clock)();
        let id = self.store.append_event(session, ts_ms, kind, &payload)?;
        let event = Event { id, session: session.clone(), ts_ms, kind: kind.to_string(), payload };
        self.subscribers.lock().unwrap().retain(|s| s.send(event.clone()).is_ok());
        Ok(event)
    }
}

/// What the hook endpoint needs to know about a session.
pub(crate) struct SessionInfo {
    pub cwd: PathBuf,
    guard: Mutex<GuardState>,
    canary_started: AtomicBool,
    requests: Mutex<Vec<AccessRequest>>,
    next_request: AtomicU64,
    /// One-line messages for the model, delivered with the next prompt.
    notes: Mutex<Vec<String>>,
}

/// Whether the guard hooks are known to be running for a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardState {
    /// No hook call seen yet.
    Pending,
    Active,
    /// The hooks never fired; the task was dropped to ask mode. The UI shows a badge.
    Off,
}

impl SessionInfo {
    fn new(cwd: PathBuf) -> SessionInfo {
        SessionInfo {
            cwd,
            guard: Mutex::new(GuardState::Pending),
            canary_started: AtomicBool::new(false),
            requests: Mutex::default(),
            next_request: AtomicU64::new(1),
            notes: Mutex::default(),
        }
    }

    pub fn add_request(&self, path: PathBuf, reason: String) -> u64 {
        let id = self.next_request.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(AccessRequest { id, path, reason });
        id
    }

    pub fn take_notes(&self) -> Vec<String> {
        std::mem::take(&mut *self.notes.lock().unwrap())
    }

    /// A hook call arrived, so the hooks are live. Returns false if the canary had already given up.
    pub fn hooks_fired(&self) -> bool {
        let mut state = self.guard.lock().unwrap();
        if *state == GuardState::Pending {
            *state = GuardState::Active;
        }
        *state == GuardState::Active
    }
}

/// State shared with the http endpoint.
pub(crate) struct Shared {
    pub rec: Arc<Recorder>,
    pub guard: GuardConfig,
    pub policy: Policy,
    pub indexes: crate::index::Indexes,
    permission_mode: String,
    canary_timeout: Duration,
    /// Random, part of every hook url, so other local processes cannot call the endpoint.
    pub token: String,
    sessions: Mutex<HashMap<SessionId, Arc<SessionInfo>>>,
}

impl Shared {
    pub fn session(&self, id: &SessionId) -> Option<Arc<SessionInfo>> {
        self.sessions.lock().unwrap().get(id).cloned()
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
    shared: Arc<Shared>,
    server: Server,
    adapter: AdapterCommand,
    live: Mutex<HashMap<SessionId, Arc<Live>>>,
}

const CALL_TIMEOUT: Duration = Duration::from_secs(30);

impl Core {
    pub fn open(config: Config) -> Result<Core> {
        std::fs::create_dir_all(&config.data_dir)?;
        let store = Store::open(&config.data_dir.join("core.db"))?;
        let rec = Arc::new(Recorder { store, clock: config.clock, subscribers: Mutex::default(), order: Mutex::new(()) });
        let shared = Arc::new(Shared {
            rec: rec.clone(),
            guard: config.guard,
            policy: Policy::new(config.data_dir.join("policy")),
            indexes: Default::default(),
            permission_mode: config.permission_mode,
            canary_timeout: config.canary_timeout,
            token: uuid::Uuid::new_v4().simple().to_string(),
            sessions: Mutex::default(),
        });
        let routed = shared.clone();
        let server = Server::start(Arc::new(move |path, body| crate::hooks::handle(&routed, path, body)))?;
        Ok(Core { rec, shared, server, adapter: config.adapter, live: Mutex::default() })
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

    /// How many files the repo index of this session has parsed so far (a change should cost only its own files).
    pub fn index_parses(&self, session: &SessionId) -> u64 {
        self.shared.session(session).map_or(0, |i| self.shared.indexes.parses(&i.cwd))
    }

    pub fn guard_state(&self, session: &SessionId) -> GuardState {
        self.shared.session(session).map_or(GuardState::Pending, |i| *i.guard.lock().unwrap())
    }

    pub fn usage(&self, session: &SessionId) -> Result<Vec<UsageRow>> {
        self.rec.store.usage(session)
    }

    /// Keeps full tool output aside (the context firewall's handles build on this); it expires after 14 days.
    pub fn put_blob(&self, session: &SessionId, data: &[u8]) -> Result<String> {
        self.rec.store.put_blob(session, (self.rec.clock)(), data)
    }

    pub fn blob(&self, handle: &str) -> Result<Option<Vec<u8>>> {
        self.rec.store.blob(handle)
    }

    /// Applies the retention rules; the app calls it at startup and now and then.
    pub fn prune_expired(&self) -> Result<()> {
        self.rec.store.prune((self.rec.clock)())
    }

    /// The url Claude Code calls for `event` (`pre`, ...) of this session.
    pub fn hook_url(&self, session: &SessionId, event: &str) -> String {
        format!("http://127.0.0.1:{}/hook/{}/{session}/{event}", self.server.port(), self.shared.token)
    }

    pub fn mcp_url(&self, session: &SessionId) -> String {
        format!("http://127.0.0.1:{}/mcp/{}/{session}", self.server.port(), self.shared.token)
    }

    /// Requests for access outside the project that wait for the user (the task shows "waiting for you").
    pub fn pending_access(&self, session: &SessionId) -> Vec<AccessRequest> {
        self.shared.session(session).map(|i| i.requests.lock().unwrap().clone()).unwrap_or_default()
    }

    /// Answers a request. An approval edits the policy file the guard reads, so it holds from the next call.
    pub fn resolve_access(&self, session: &SessionId, request: u64, decision: Decision) -> Result<()> {
        let info = self.shared.session(session).ok_or_else(|| anyhow!("unknown session {session}"))?;
        let asked = {
            let mut pending = info.requests.lock().unwrap();
            let at = pending.iter().position(|r| r.id == request).ok_or_else(|| anyhow!("no pending request {request}"))?;
            pending.remove(at)
        };
        let scope = match decision {
            Decision::Deny => None,
            Decision::Task => Some(Scope::Task),
            Decision::Project => Some(Scope::Project),
        };
        if let Some(scope) = scope {
            self.shared.policy.grant(scope, session, &info.cwd, &asked.path)?;
        }
        let shown = asked.path.display();
        let (note, outcome) = match decision {
            Decision::Deny => (format!("The user declined access to {shown}."), "denied"),
            Decision::Task => (format!("The user allowed writing to {shown} for this task."), "task"),
            Decision::Project => (format!("The user allowed writing to {shown} for this project."), "project"),
        };
        info.notes.lock().unwrap().push(note);
        self.rec.record(session, kind::ACCESS_RESOLVED, json!({"request_id": request, "path": asked.path.to_string_lossy(), "decision": outcome}))?;
        Ok(())
    }

    pub fn start_session(&self, cwd: &Path) -> Result<SessionId> {
        let id = SessionId::new();
        let (running, closing) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let conn = self.spawn_adapter(&id, running.clone(), closing.clone())?;
        conn.call("initialize", json!({"protocolVersion": 1, "clientCapabilities": {}}), CALL_TIMEOUT)?;
        let cwd_text = cwd.to_string_lossy().into_owned();
        // The session must be known to the endpoint before the adapter can call a hook for it.
        self.shared.sessions.lock().unwrap().insert(id.clone(), Arc::new(SessionInfo::new(cwd.to_path_buf())));
        let options = json!({"settings": crate::hooks::settings(self.hook_url(&id, "").trim_end_matches('/')), "enableFileCheckpointing": true, "allowedTools": ["mcp__claudiu"]});
        let servers = json!([{"type": "http", "name": "claudiu", "url": self.mcp_url(&id), "headers": []}]);
        let new = json!({"cwd": cwd_text, "mcpServers": servers, "_meta": {"claudeCode": {"options": options}}});
        let made = conn.call("session/new", new, CALL_TIMEOUT)?;
        let adapter_session_id = made["sessionId"].as_str().ok_or_else(|| anyhow!("session/new returned no session id"))?.to_string();
        let offers_mode = made["configOptions"].as_array().is_some_and(|opts| {
            opts.iter().filter(|o| o["id"] == "mode").any(|o| o["options"].as_array().is_some_and(|m| m.iter().any(|m| m["value"] == self.shared.permission_mode.as_str())))
        });
        if offers_mode {
            conn.call("session/set_mode", json!({"sessionId": adapter_session_id, "modeId": self.shared.permission_mode}), CALL_TIMEOUT)?;
        }

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
                if update["_meta"]["claudeCode"]["toolName"] == "compact" {
                    // The conversation is being summarised: the model no longer has what the ledger says it saw.
                    let _ = rec.store.clear_ledger(&id);
                }
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

    /// The first prompt starts a timer; if no hook has been called by then, the guards are not running.
    fn start_canary(&self, id: &SessionId, live: &Arc<Live>) {
        let Some(info) = self.shared.session(id) else { return };
        if info.canary_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let (rec, live, id, timeout) = (self.rec.clone(), live.clone(), id.clone(), self.shared.canary_timeout);
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            let mut state = info.guard.lock().unwrap();
            if *state != GuardState::Pending || live.closing.load(Ordering::SeqCst) {
                return;
            }
            *state = GuardState::Off;
            drop(state);
            // The badge event follows the mode change, so whoever sees one can rely on the other.
            let _ = live.conn.call("session/set_mode", json!({"sessionId": live.adapter_session_id, "modeId": "default"}), CALL_TIMEOUT);
            let _ = rec.record(&id, kind::GUARDS_OFF, json!({"reason": "no hook call arrived after the first prompt"}));
        });
    }

    pub fn send_prompt(&self, id: &SessionId, text: &str) -> Result<()> {
        let live = self.live(id)?;
        if live.running.swap(true, Ordering::SeqCst) {
            bail!("a turn is already running in session {id}");
        }
        self.rec.record(id, kind::USER_PROMPT, json!({"text": text}))?;
        self.start_canary(id, &live);
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
