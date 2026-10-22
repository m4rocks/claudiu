//! The command API: start sessions, send prompts, cancel, and read the event stream and history.
//!
//! `Core` is the handle the app holds. The work lives in `Engine`, which the http endpoint can reach as well
//! (a `delegate` tool call starts a subagent session), so `Core` is only an `Engine` plus the endpoint's server.

use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::acp::{AdapterCommand, Conn, Msg};
use crate::delegate::{Meter, Run, Signal, Stagger, Tokens};
use crate::event::{AccessRequest, Decision, Event, SessionId, SessionRecord, UsageRow, kind};
use crate::guard::GuardConfig;
use crate::http::Server;
use crate::policy::{Policy, Scope};
use crate::store::Store;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// What Claudiu adds around Claude Code. All of it is on; `bench` switches parts off to measure each one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Features {
    /// The guard hooks and the canary that checks they run.
    pub hooks: bool,
    /// The context firewall (a hook on tool output).
    pub firewall: bool,
    /// The claudiu MCP server: index tools, `expand`, `request_access`.
    pub mcp: bool,
    pub delegation: bool,
}

impl Default for Features {
    fn default() -> Features {
        Features { hooks: true, firewall: true, mcp: true, delegation: true }
    }
}

impl Features {
    /// Claude Code through the adapter and nothing else.
    pub fn stock() -> Features {
        Features { hooks: false, firewall: false, mcp: false, delegation: false }
    }
}

pub struct Config {
    pub data_dir: PathBuf,
    pub adapter: AdapterCommand,
    pub clock: Clock,
    pub guard: GuardConfig,
    /// The Claude Code permission mode a task starts in.
    pub permission_mode: String,
    /// How long after the first prompt the guard hooks may stay silent before the task drops to ask mode.
    pub canary_timeout: Duration,
    /// How long a subagent's start holds back its same-profile sibling when it has not streamed yet.
    pub stagger_fallback: Duration,
    pub features: Features,
}

impl Config {
    pub fn new(data_dir: impl Into<PathBuf>, adapter: AdapterCommand) -> Config {
        let clock: Clock = Arc::new(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
        });
        Config {
            data_dir: data_dir.into(),
            adapter,
            clock,
            guard: GuardConfig::default(),
            permission_mode: "auto".into(),
            canary_timeout: Duration::from_secs(20),
            stagger_fallback: Duration::from_secs(5),
            features: Features::default(),
        }
    }

    pub fn with_features(mut self, features: Features) -> Config {
        self.features = features;
        self
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

    pub fn with_stagger_fallback(mut self, fallback: Duration) -> Config {
        self.stagger_fallback = fallback;
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

/// What the hook endpoint and the delegation runner need to know about a session.
pub(crate) struct SessionInfo {
    pub cwd: PathBuf,
    /// Whose grants apply: its own, or for a subagent its parent task's.
    pub policy_session: SessionId,
    /// For a subagent: the task it works for, which answers its access requests.
    pub parent: Option<(SessionId, Arc<SessionInfo>)>,
    guard: Mutex<GuardState>,
    canary_started: AtomicBool,
    requests: Mutex<Vec<AccessRequest>>,
    next_request: AtomicU64,
    /// One-line messages for the model, delivered with the next prompt.
    notes: Mutex<Vec<String>>,
    /// What the adapter reports the session has been billed for.
    pub meter: Meter,
    /// The agent's message text of the turn in flight.
    pub text: Mutex<String>,
    pub tool_calls: AtomicU64,
    /// Set when the first response chunk streams, which means the prompt cache holds the prefix.
    pub streamed: Arc<Signal>,
    /// The delegations this session has started.
    pub runs: Mutex<Vec<Arc<Run>>>,
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
    fn new(id: &SessionId, cwd: PathBuf, parent: Option<(SessionId, Arc<SessionInfo>)>) -> SessionInfo {
        SessionInfo {
            cwd,
            policy_session: parent.as_ref().map_or_else(|| id.clone(), |(p, _)| p.clone()),
            parent,
            guard: Mutex::new(GuardState::Pending),
            canary_started: AtomicBool::new(false),
            requests: Mutex::default(),
            next_request: AtomicU64::new(1),
            notes: Mutex::default(),
            meter: Meter::default(),
            text: Mutex::default(),
            tool_calls: AtomicU64::new(0),
            streamed: Arc::new(Signal::default()),
            runs: Mutex::default(),
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

    pub fn guards_off(&self) -> bool {
        *self.guard.lock().unwrap() == GuardState::Off
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
    pub features: Features,
    /// How long each hook call took to answer, in milliseconds.
    pub latencies: Mutex<Vec<f64>>,
    /// Random, part of every hook url, so other local processes cannot call the endpoint.
    pub token: String,
    sessions: Mutex<HashMap<SessionId, Arc<SessionInfo>>>,
    engine: OnceLock<Weak<Engine>>,
}

impl Shared {
    pub fn session(&self, id: &SessionId) -> Option<Arc<SessionInfo>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    pub fn engine(&self) -> Option<Arc<Engine>> {
        self.engine.get().and_then(Weak::upgrade)
    }
}

pub(crate) struct Live {
    pub conn: Conn,
    pub adapter_session_id: String,
    /// A turn is in flight.
    pub running: Arc<AtomicBool>,
    /// We are ending the adapter on purpose, so its exit is not a crash.
    pub closing: Arc<AtomicBool>,
}

/// What a task has cost, as the adapter reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spend {
    pub tokens: Tokens,
    pub cost_usd: f64,
}

/// How a session is started when it is a subagent.
pub(crate) struct ChildSpec {
    pub parent: SessionId,
    pub profile: &'static crate::delegate::Profile,
    pub title: String,
}

pub struct Engine {
    pub(crate) rec: Arc<Recorder>,
    pub(crate) shared: Arc<Shared>,
    adapter: AdapterCommand,
    live: Mutex<HashMap<SessionId, Arc<Live>>>,
    port: u16,
    pub(crate) stagger: Stagger,
    pub(crate) stagger_fallback: Duration,
    pub(crate) next_delegation: AtomicU64,
}

/// The handle the app holds: an [`Engine`] and the http server its sessions call back into.
pub struct Core {
    engine: Arc<Engine>,
    _server: Server,
}

impl Deref for Core {
    type Target = Engine;
    fn deref(&self) -> &Engine {
        &self.engine
    }
}

pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(30);

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
            features: config.features,
            latencies: Mutex::default(),
            token: uuid::Uuid::new_v4().simple().to_string(),
            sessions: Mutex::default(),
            engine: OnceLock::new(),
        });
        let routed = shared.clone();
        let server = Server::start(Arc::new(move |path, body| crate::hooks::handle(&routed, path, body)))?;
        let engine = Arc::new(Engine {
            rec,
            shared: shared.clone(),
            adapter: config.adapter,
            live: Mutex::default(),
            port: server.port(),
            stagger: Stagger::default(),
            stagger_fallback: config.stagger_fallback,
            next_delegation: AtomicU64::new(1),
        });
        let _ = shared.engine.set(Arc::downgrade(&engine));
        Ok(Core { engine, _server: server })
    }

    /// Quit: cancels running turns, waits briefly for them to settle (recording any that do not as interrupted),
    /// ends the adapters and flushes the store.
    pub fn shutdown(self) {
        self.engine.shutdown();
    }
}

impl Engine {
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

    /// What the task and the subagents it started have used so far.
    pub fn spend(&self, task: &SessionId) -> Spend {
        let mut spend = Spend::default();
        for (id, info) in self.shared.sessions.lock().unwrap().iter() {
            if id == task || info.parent.as_ref().is_some_and(|(p, _)| p == task) {
                spend.tokens.add(&info.meter.totals());
                spend.cost_usd += info.meter.cost_usd();
            }
        }
        spend
    }

    /// How long each hook call took the core to answer, in milliseconds, oldest first.
    pub fn hook_latencies_ms(&self) -> Vec<f64> {
        self.shared.latencies.lock().unwrap().clone()
    }

    /// Sets a session option the adapter offers, such as `model` or `effort`.
    pub fn set_config(&self, session: &SessionId, config: &str, value: &str) -> Result<()> {
        let live = self.live(session)?;
        live.conn.call("session/set_config_option", json!({"sessionId": live.adapter_session_id, "configId": config, "value": value}), CALL_TIMEOUT)?;
        Ok(())
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
        format!("http://127.0.0.1:{}/hook/{}/{session}/{event}", self.port, self.shared.token)
    }

    pub fn mcp_url(&self, session: &SessionId) -> String {
        format!("http://127.0.0.1:{}/mcp/{}/{session}", self.port, self.shared.token)
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
        self.start(cwd, None)
    }

    /// Starts a session: a task, or with `child` a subagent trimmed to its profile.
    pub(crate) fn start(&self, cwd: &Path, child: Option<ChildSpec>) -> Result<SessionId> {
        let id = SessionId::new();
        let (running, closing) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let parent = match &child {
            Some(c) => Some((c.parent.clone(), self.shared.session(&c.parent).ok_or_else(|| anyhow!("unknown session {}", c.parent))?)),
            None => None,
        };
        // The session must be known to the endpoint before the adapter can call a hook for it.
        let info = Arc::new(SessionInfo::new(&id, cwd.to_path_buf(), parent));
        self.shared.sessions.lock().unwrap().insert(id.clone(), info.clone());
        let command = match &child {
            Some(c) => self.adapter.clone().env("CLAUDIU_PROFILE", c.profile.name),
            None => self.adapter.clone(),
        };
        let conn = self.spawn_adapter(&command, &id, info, running.clone(), closing.clone())?;
        conn.call("initialize", json!({"protocolVersion": 1, "clientCapabilities": {}}), CALL_TIMEOUT)?;
        let cwd_text = cwd.to_string_lossy().into_owned();
        let features = self.shared.features;
        let mut options = json!({});
        if features.hooks {
            options["settings"] = crate::hooks::settings(self.hook_url(&id, "").trim_end_matches('/'), features.firewall);
            options["enableFileCheckpointing"] = json!(true);
        }
        let mut servers = json!([]);
        if features.mcp {
            options["allowedTools"] = json!(["mcp__claudiu"]);
            servers = json!([{"type": "http", "name": "claudiu", "url": self.mcp_url(&id), "headers": []}]);
        }
        let mut new = json!({"cwd": cwd_text, "mcpServers": servers});
        if let Some(c) = &child {
            c.profile.trim(&mut options, &mut new, &c.title);
        }
        new["_meta"] = json!({"claudeCode": {"options": options, "emitRawSDKMessages": true}});
        let made = conn.call("session/new", new, CALL_TIMEOUT)?;
        let adapter_session_id = made["sessionId"].as_str().ok_or_else(|| anyhow!("session/new returned no session id"))?.to_string();
        let offers = |config: &str, value: &str| {
            made["configOptions"].as_array().is_some_and(|opts| {
                opts.iter().filter(|o| o["id"] == config).any(|o| o["options"].as_array().is_some_and(|m| m.iter().any(|m| m["value"] == value)))
            })
        };
        if offers("mode", &self.shared.permission_mode) {
            conn.call("session/set_mode", json!({"sessionId": adapter_session_id, "modeId": self.shared.permission_mode}), CALL_TIMEOUT)?;
        }
        if let Some(c) = &child {
            // A fresh session starts on the default model, so a subagent's own model is always set explicitly.
            conn.call("session/set_config_option", json!({"sessionId": adapter_session_id, "configId": "model", "value": c.profile.model}), CALL_TIMEOUT)?;
            if offers("effort", c.profile.effort) {
                conn.call("session/set_config_option", json!({"sessionId": adapter_session_id, "configId": "effort", "value": c.profile.effort}), CALL_TIMEOUT)?;
            }
        }

        let created_ms = (self.rec.clock)();
        self.rec.store.insert_session(&SessionRecord {
            id: id.clone(),
            cwd: cwd_text.clone(),
            title: child.as_ref().map(|c| c.title.clone()),
            adapter_session_id: Some(adapter_session_id.clone()),
            created_ms,
            parent: child.as_ref().map(|c| c.parent.clone()),
        })?;
        self.rec.record(&id, kind::SESSION_STARTED, json!({"cwd": cwd_text, "adapter_session_id": adapter_session_id}))?;
        self.live.lock().unwrap().insert(id.clone(), Arc::new(Live { conn, adapter_session_id, running, closing }));
        Ok(id)
    }

    fn spawn_adapter(&self, command: &AdapterCommand, id: &SessionId, info: Arc<SessionInfo>, running: Arc<AtomicBool>, closing: Arc<AtomicBool>) -> Result<Conn> {
        let rec = self.rec.clone();
        let id = id.clone();
        let conn = Conn::spawn(command, move |msg, responder| match msg {
            Msg::Notification { method, params } if method == "_claude/sdkMessage" => info.meter.add(&params["message"]),
            Msg::Notification { method, params } if method == "session/update" => {
                let update = params["update"].clone();
                let kind = match update["sessionUpdate"].as_str() {
                    Some("tool_call" | "tool_call_update") => kind::TOOL_CALL,
                    _ => kind::AGENT_UPDATE,
                };
                match update["sessionUpdate"].as_str() {
                    Some("agent_message_chunk") => {
                        if let Some(text) = update["content"]["text"].as_str() {
                            info.text.lock().unwrap().push_str(text);
                        }
                        info.streamed.set();
                    }
                    Some("agent_thought_chunk") => info.streamed.set(),
                    Some("tool_call") => {
                        info.tool_calls.fetch_add(1, Ordering::SeqCst);
                        info.streamed.set();
                    }
                    _ => {}
                }
                if update["_meta"]["claudeCode"]["toolName"] == "compact" {
                    // The conversation is being summarised: the model no longer has what the ledger says it saw.
                    let _ = rec.store.clear_ledger(&id);
                }
                if let Some(usd) = update["cost"]["amount"].as_f64() {
                    info.meter.set_cost(usd);
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

    pub(crate) fn live(&self, id: &SessionId) -> Result<Arc<Live>> {
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
            let fallback = live.conn.call("session/set_mode", json!({"sessionId": live.adapter_session_id, "modeId": "default"}), CALL_TIMEOUT).is_ok();
            let _ = rec.record(&id, kind::GUARDS_OFF, json!({"reason": "no hook call arrived after the first prompt", "fallback": fallback}));
        });
    }

    pub fn send_prompt(&self, id: &SessionId, text: &str) -> Result<()> {
        let live = self.live(id)?;
        self.start_turn(id, &live, text, |_| {})?;
        if self.shared.features.hooks {
            self.start_canary(id, &live);
        }
        Ok(())
    }

    /// Sends one prompt; `then` gets the adapter's answer once the turn is over.
    pub(crate) fn start_turn(&self, id: &SessionId, live: &Arc<Live>, text: &str, then: impl FnOnce(Result<Value, String>) + Send + 'static) -> Result<()> {
        if live.running.swap(true, Ordering::SeqCst) {
            bail!("a turn is already running in session {id}");
        }
        if let Some(info) = self.shared.session(id) {
            info.text.lock().unwrap().clear();
        }
        self.rec.record(id, kind::USER_PROMPT, json!({"text": text}))?;
        let (rec, session, running) = (self.rec.clone(), id.clone(), live.running.clone());
        live.conn.call_async(
            "session/prompt",
            json!({"sessionId": live.adapter_session_id, "prompt": [{"type": "text", "text": text}]}),
            move |result| {
                if running.swap(false, Ordering::SeqCst) {
                    let _ = match &result {
                        Ok(v) => {
                            let stop = v["stopReason"].as_str().unwrap_or("end_turn");
                            let kind = if stop == "cancelled" { kind::INTERRUPTED } else { kind::TURN_ENDED };
                            rec.record(&session, kind, json!({"stop_reason": stop}))
                        }
                        Err(message) => rec.record(&session, kind::ERROR, json!({"message": message})),
                    };
                }
                then(result);
            },
        );
        Ok(())
    }

    /// Ends a finished subagent's adapter. Its record and events stay.
    pub(crate) fn end_session(&self, id: &SessionId) {
        if let Some(l) = self.live.lock().unwrap().remove(id) {
            l.closing.store(true, Ordering::SeqCst);
            l.conn.close();
        }
    }

    /// Asks the adapter to stop the turn; the turn ends as `interrupted` when it answers. Subagents the task
    /// started are stopped with it.
    pub fn cancel(&self, id: &SessionId) -> Result<()> {
        let live = self.live(id)?;
        live.conn.notify("session/cancel", json!({"sessionId": live.adapter_session_id}));
        if let Some(info) = self.shared.session(id) {
            for run in info.runs.lock().unwrap().iter() {
                run.cancel();
                if let Some(child) = run.child() {
                    let _ = self.cancel(child);
                }
            }
        }
        Ok(())
    }

    fn shutdown(&self) {
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
