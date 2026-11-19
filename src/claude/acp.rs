//! Runs one task in a custom subagent's provider (`claude/subagents.rs`), through the Agent Client
//! Protocol: `claudiu --acp`. The Custom subagents settings page also asks the agent for its models here (`models`).
//!
//! The mod starts this once per subagent turn and writes the task to stdin as JSON (`Request`). A mod can write a
//! child's stdin only once, and ACP is a conversation in both directions, so the conversation happens here. Progress
//! goes to stdout as JSON lines (`Event`), ending with the report or the reason it failed.
//! During a run, the mod can also deliver messages through a per-run JSON file polled here.
//!
//! Codex runs through `npx codex-acp`, which uses the user's own `codex login`; OpenCode runs as the user's own
//! `opencode acp`, with their providers. The agent's stdin is a pipe from this process, so when the mod stops this
//! helper (the user interrupted the subagent), the pipe closes and the agent exits with it.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::claude::subagents::{Agent, AgentModel};
use crate::platform::{find_executable, reload_path};

pub const FLAG: &str = "--acp";
/// ACP's error code for "authentication required".
const AUTH_REQUIRED: i64 = -32000;
/// Lines of the agent's stderr kept for a failure's reason.
const STDERR_LINES: usize = 5;
/// How long the settings page waits for the agent's models; Codex's first run downloads codex-acp through npx.
const MODELS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const INBOX_POLL: Duration = Duration::from_millis(100);
const INBOX_TEXT_MAX: usize = 20_000; // UTF-16 units, matching the mod
/// JSON can use six bytes per unit, plus the envelope.
const INBOX_BYTES: u64 = INBOX_TEXT_MAX as u64 * 6 + 1_000;

/// What the mod asks for.
#[derive(Debug, Deserialize)]
pub struct Request {
    /// The agent that runs the task. A mod from before OpenCode sends none, and means Codex.
    #[serde(default)]
    pub agent: Agent,
    /// The agent's model; none for the agent's own default.
    pub model: Option<String>,
    /// The effort Claude's Agent call asked for; none for the agent's own default. The provider decides how to apply it.
    #[serde(default)]
    pub effort: Option<String>,
    /// Whether the agent may edit files and run commands, or only read.
    pub edits: bool,
    /// The folder the agent works in.
    pub cwd: PathBuf,
    /// The task, as Claude wrote it.
    pub prompt: String,
    /// Whether this mod understands the helper's live inbox events.
    #[serde(default)]
    pub live_messages: bool,
}

/// One line of stdout.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A piece of a message the agent is writing.
    Text { text: String },
    /// A piece of the reasoning summary the agent chose to share.
    Thought { text: String },
    /// A plan, tool status or other activity notice shown while the task runs.
    Progress { text: String },
    /// New output from a tool call, separate from the agent's report.
    Output { text: String },
    /// Where the mod can write messages for this run.
    Inbox { id: String, path: PathBuf },
    /// The helper took ownership of this message, so Claude need not queue it.
    Accepted { seq: u64 },
    /// A tool call the agent started: a command, a read, an edit.
    Step { text: String },
    /// The last message the agent wrote: the subagent's report.
    Report { text: String },
    Failed { reason: String },
}

pub fn run() -> i32 {
    match std::env::args().nth(2).as_deref() {
        Some("--cleanup") => return cleanup_inbox(),
        Some(super::antigravity_read_only::HOOK_FLAG) => return super::antigravity_read_only::run_hook(),
        _ => {}
    }
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

fn inbox_path(id: Uuid) -> PathBuf {
    crate::datadir::data_dir().join("acp").join(format!("{id}.json"))
}

fn cleanup_inbox() -> i32 {
    let Some(id) = std::env::args().nth(3).and_then(|id| Uuid::parse_str(&id).ok()) else { return 1 };
    match std::fs::remove_file(inbox_path(id)) {
        Ok(()) => 0,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(_) => 1,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    seq: u64,
    text: String,
}

struct Inbox {
    id: Uuid,
    path: PathBuf,
    seen: u64,
}

impl Inbox {
    /// The next message the mod wrote, with its number. It is acknowledged (`Event::Accepted`) only once it has
    /// reached the agent: until then the mod still holds it, and gives it back to Claude Code's own queue if this
    /// helper ends first.
    fn take(&mut self) -> Option<(u64, String)> {
        // The mod writes the whole JSON value. A poll during that write can read a partial value and retries it.
        if !std::fs::symlink_metadata(&self.path).ok()?.file_type().is_file() {
            return None;
        }
        let mut body = Vec::new();
        std::fs::File::open(&self.path).ok()?.take(INBOX_BYTES + 1).read_to_end(&mut body).ok()?;
        if body.len() as u64 > INBOX_BYTES {
            return None;
        }
        let delivery: Delivery = serde_json::from_slice(&body).ok()?;
        if delivery.seq != self.seen + 1 || delivery.text.trim().is_empty() || delivery.text.encode_utf16().count() > INBOX_TEXT_MAX {
            return None;
        }
        self.seen = delivery.seq;
        // The mod won't write the next message until it sees the acknowledgement.
        let _ = std::fs::remove_file(&self.path);
        Some((delivery.seq, delivery.text))
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn run_task(request: &Request, emit: &mut impl FnMut(Event)) -> Result<String, String> {
    let provider = request.agent.provider();
    let args = (provider.task_args)(request)?;
    let env = (provider.environment)(request);
    let connection = Connection::start(request.agent, &request.cwd, &args, &env)?;
    let id = Uuid::new_v4();
    let inbox = request.live_messages.then(|| Inbox { id, path: inbox_path(id), seen: 0 });
    let result = converse(connection.from_agent, connection.to_agent, request, inbox, emit);
    connection.process.finish(result)
}

pub(super) fn codex_env(request: &Request) -> Vec<(&'static str, String)> {
    let mut env = vec![("INITIAL_AGENT_MODE", (if request.edits { "workspace-write" } else { "read-only" }).to_string())];
    if let Some(config) = codex_config(request.model.as_deref(), request.effort.as_deref()) {
        env.push(("CODEX_CONFIG", config.to_string()));
    }
    env
}

pub(super) fn opencode_env(request: &Request) -> Vec<(&'static str, String)> {
    vec![("OPENCODE_CONFIG_CONTENT", opencode_config(request.model.as_deref(), request.edits).to_string())]
}

pub(super) fn no_args(_: &Request) -> Result<Vec<String>, String> {
    Ok(vec![])
}

/// Read-only Antigravity runs from a Gemini folder whose hook refuses every tool but reading
/// (`claude/antigravity_read_only.rs`).
pub(super) fn antigravity_args(request: &Request) -> Result<Vec<String>, String> {
    if request.edits {
        return Ok(vec![]);
    }
    let claudiu = std::env::current_exe().map_err(|e| format!("Claudiu couldn't find itself to keep Antigravity read-only ({e})"))?;
    let gemini = super::antigravity_read_only::prepare(&crate::datadir::data_dir(), &claudiu).map_err(|e| format!("Claudiu couldn't set up Antigravity's read-only folder ({e})"))?;
    Ok(vec!["--gemini-dir".into(), gemini.to_string_lossy().into_owned()])
}

pub(super) fn no_env(_: &Request) -> Vec<(&'static str, String)> {
    vec![]
}

pub(super) fn no_options(_: &Request) -> Vec<(&'static str, String)> {
    vec![]
}

pub(super) fn antigravity_options(request: &Request) -> Vec<(&'static str, String)> {
    let mut options = vec![];
    if let Some(model) = &request.model {
        options.push(("model", model.clone()));
    }
    if let Some(effort) = effort_level(request.effort.as_deref()) {
        options.push(("thought_level", effort.into()));
    }
    // Read-only keeps the default mode, where agy asks before each edit or command and Claudiu refuses.
    if request.edits {
        options.push(("mode", "accept-edits".into()));
    }
    options
}

/// Thinking levels from least to most, as ACP agents name them.
const THOUGHT_LEVELS: [&str; 4] = ["low", "medium", "high", "xhigh"];

/// The value to set `config` to, given the options the agent `offered`. A thinking level the model doesn't offer
/// becomes the highest one it does below it, or none, which leaves the model's own default: Claude's effort is a hint,
/// not worth failing the task for. Anything else is sent as asked, so an unknown model fails with the agent's reason.
fn offered_value(offered: &Value, config: &str, wanted: &str) -> Option<String> {
    let choices: Option<Vec<&str>> = offered
        .as_array()
        .and_then(|options| options.iter().find(|o| o["id"] == config))
        .and_then(|option| option["options"].as_array())
        .map(|choices| choices.iter().filter_map(|c| c["value"].as_str()).collect());
    let Some(choices) = choices.filter(|_| config == "thought_level") else { return Some(wanted.into()) };
    let rank = THOUGHT_LEVELS.iter().position(|l| *l == wanted)?;
    THOUGHT_LEVELS[..=rank].iter().rev().find(|l| choices.contains(l)).map(|l| l.to_string())
}

fn effort_level(effort: Option<&str>) -> Option<&str> {
    match effort {
        Some("max") => Some("xhigh"),
        Some(level @ ("low" | "medium" | "high" | "xhigh")) => Some(level),
        _ => None,
    }
}

/// The config Codex gets on top of the user's own: the model and the reasoning effort, where given. Codex has no `max`,
/// so `max` becomes its highest level, `xhigh`; a level Codex doesn't know is left to the user's config.
fn codex_config(model: Option<&str>, effort: Option<&str>) -> Option<Value> {
    let mut config = serde_json::Map::new();
    if let Some(model) = model {
        config.insert("model".into(), json!(model));
    }
    let effort = effort_level(effort);
    if let Some(effort) = effort {
        config.insert("model_reasoning_effort".into(), json!(effort));
    }
    (!config.is_empty()).then_some(Value::Object(config))
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
    let connection = Connection::start(agent, cwd, &[], &[])?;
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
    agent.provider().server
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

/// The program on the PATH. A desktop launch has a minimal PATH, so OpenCode or Node installed since Claudiu started,
/// or put on the PATH only by the shell's startup files, turns up once the login shell's PATH is read again.
fn locate_reloading(program: &str) -> Option<PathBuf> {
    find_executable(program).or_else(|| {
        reload_path();
        find_executable(program)
    })
}

/// Where `agent`'s ACP server is: on the PATH, or where its Windows installer puts it, which a desktop launch's PATH
/// may not include yet.
pub fn locate(agent: Agent) -> Option<PathBuf> {
    let provider = agent.provider();
    locate_reloading(provider.program).or_else(|| {
        let installed = provider.windows_fallback.filter(|_| cfg!(windows))?;
        Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(installed)).filter(|path| path.is_file())
    })
}

impl Connection {
    fn start(agent: Agent, cwd: &Path, task_args: &[String], env: &[(&str, String)]) -> Result<Connection, String> {
        let provider = agent.provider();
        let program = locate(agent).ok_or_else(|| provider.missing.to_string())?;
        let mut command = Command::new(program);
        command.args(provider.args).args(task_args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::platform::no_window(&mut command);
        crate::platform::with_login_path(&mut command);
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
    if agent.provider().delete_model_session
        && let Some(id) = session["sessionId"].as_str()
        // The models are known already, so a failed delete only leaves an empty session behind.
        && send(json!({ "jsonrpc": "2.0", "id": 2, "method": "session/delete", "params": { "sessionId": id } })).is_ok()
    {
        let _ = reply(2);
    }
    if models.is_empty() { Err(format!("{} listed no models", server(agent))) } else { Ok(models) }
}

/// Sends a task and its live messages in one ACP session, streaming updates as they arrive.
fn converse(from_agent: impl BufRead + Send + 'static, mut to_agent: impl Write, request: &Request, mut inbox: Option<Inbox>, emit: &mut impl FnMut(Event)) -> Result<String, String> {
    let agent = request.agent;
    let name = agent.name();
    // ACP can go quiet during a long tool call. Reading stdout on its own thread lets the inbox still wake us.
    let (tx, rx) = sync_channel(32);
    std::thread::spawn(move || {
        for line in from_agent.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let send = |to_agent: &mut dyn Write, message: Value| -> Result<(), String> { writeln!(to_agent, "{message}").and_then(|_| to_agent.flush()).map_err(|_| exited(agent)) };
    let mut messages = Messages::default();
    // OpenCode also reports its own subagents' sessions. Their updates aren't this task's report.
    let read = |to_agent: &mut dyn Write, session: Option<&str>, messages: &mut Messages, emit: &mut dyn FnMut(Event), timeout: Option<Duration>| -> Result<Option<Value>, String> {
        let line = match timeout {
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => return Err(exited(agent)),
            },
            None => rx.recv().map_err(|_| exited(agent))?,
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { return Ok(None) };
        match msg["method"].as_str() {
            Some("session/update") => {
                if session.is_some_and(|s| msg["params"]["sessionId"] == s) {
                    messages.update(agent, &msg["params"]["update"], emit);
                }
            }
            Some("session/request_permission") => {
                let outcome = permission(&msg["params"]["options"], request.edits);
                if !request.edits {
                    messages.refused = Some(msg["params"]["toolCall"]["title"].as_str().unwrap_or("an action").to_string());
                }
                send(to_agent, json!({ "jsonrpc": "2.0", "id": msg["id"], "result": { "outcome": outcome } }))?;
            }
            // A request this client doesn't serve: it offered no file or terminal access.
            Some(_) if msg.get("id").is_some() => {
                send(to_agent, json!({ "jsonrpc": "2.0", "id": msg["id"], "error": { "code": -32601, "message": "method not found" } }))?;
            }
            None if msg["id"].is_i64() => return Ok(Some(msg)),
            _ => {}
        }
        Ok(None)
    };

    let result = |msg: Value| match msg.get("error") {
        Some(error) => Err(error_reason(agent, error)),
        None => Ok(msg["result"].clone()),
    };
    let answer = |to_agent: &mut dyn Write, id: i64, messages: &mut Messages, emit: &mut dyn FnMut(Event)| -> Result<Value, String> {
        loop {
            if let Some(msg) = read(to_agent, None, messages, emit, None)?
                && msg["id"] == id
            {
                return result(msg);
            }
        }
    };
    send(&mut to_agent, initialize())?;
    let initialized = answer(&mut to_agent, 0, &mut messages, emit)?;
    let supports_steering = initialized["_meta"]["steering"]["supported"] == true;
    let mut steer = supports_steering;
    if !agent.provider().inbox_without_steering && !supports_steering {
        inbox = None; // Claude Code keeps its usual queued delivery on older OpenCode versions.
    }

    send(&mut to_agent, new_session(&request.cwd))?;
    let session = answer(&mut to_agent, 1, &mut messages, emit)?;
    let session_id = session["sessionId"].as_str().ok_or(format!("{} opened no session", server(agent)))?.to_string();
    emit(Event::Progress { text: "Session ready. Sending the task.".into() });

    let prompt = |id, texts: Vec<String>| {
        let blocks: Vec<_> = texts.into_iter().map(|text| json!({ "type": "text", "text": text })).collect();
        json!({ "jsonrpc": "2.0", "id": id, "method": "session/prompt", "params": { "sessionId": session_id, "prompt": blocks } })
    };
    let mut next_id = 2;
    // Each answer lists the options as they stand after the change: a new model can offer other thinking levels.
    let mut offered = session["configOptions"].clone();
    for (config, wanted) in (agent.provider().config_options)(request) {
        let Some(value) = offered_value(&offered, config, &wanted) else { continue };
        send(&mut to_agent, json!({ "jsonrpc": "2.0", "id": next_id, "method": "session/set_config_option", "params": { "sessionId": session_id, "configId": config, "value": value } }))?;
        let changed = answer(&mut to_agent, next_id, &mut messages, emit).map_err(|reason| format!("{name} could not set {config}: {reason}"))?;
        if changed["configOptions"].is_array() {
            offered = changed["configOptions"].clone();
        }
        next_id += 1;
    }
    let mut prompt_id = next_id;
    next_id += 1;
    send(&mut to_agent, prompt(prompt_id, vec![request.prompt.clone()]))?;
    if let Some(inbox) = &inbox {
        emit(Event::Inbox { id: inbox.id.to_string(), path: inbox.path.clone() });
    }

    let mut pending = std::collections::VecDeque::new();
    let mut steering = None;
    let mut done = None;
    let mut next_poll = Instant::now();
    let done = loop {
        if let Some(inbox) = &mut inbox
            && (Instant::now() >= next_poll || done.is_some())
        {
            next_poll = Instant::now() + INBOX_POLL;
            if let Some((seq, text)) = inbox.take() {
                pending.push_back((Some(seq), text));
            }
        }

        if steering.is_none() {
            if let Some(reply) = done.take() {
                if pending.is_empty() {
                    break reply;
                }
                // The client owns the next prompt so it can stream the reply and wait for its stop reason.
                prompt_id = next_id;
                next_id += 1;
                let (seqs, texts): (Vec<Option<u64>>, Vec<String>) = pending.drain(..).unzip();
                send(&mut to_agent, prompt(prompt_id, texts))?;
                for seq in seqs.into_iter().flatten() {
                    emit(Event::Accepted { seq });
                }
                messages = Messages::default();
                steer = supports_steering;
                emit(Event::Progress { text: format!("{name} is continuing with the follow-up message.") });
            } else if steer && let Some((seq, text)) = pending.pop_front() {
                let mut params = json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": text }],
                });
                if agent.provider().steering_prompt_required {
                    params["_meta"] = json!({ "steering": { "idleBehavior": "promptRequired" } });
                }
                send(&mut to_agent, json!({ "jsonrpc": "2.0", "id": next_id, "method": "_session/steering", "params": params }))?;
                steering = Some((next_id, seq, text));
                next_id += 1;
            }
        }

        let timeout = inbox.as_ref().map(|_| next_poll.saturating_duration_since(Instant::now()));
        let Some(msg) = read(&mut to_agent, Some(&session_id), &mut messages, emit, timeout)? else { continue };
        if msg["id"] == prompt_id {
            done = Some(result(msg)?);
        } else if steering.as_ref().is_some_and(|(id, _, _)| msg["id"] == *id) {
            let (_, seq, text) = steering.take().unwrap();
            match msg["result"]["outcome"].as_str() {
                Some("injected") => {
                    if let Some(seq) = seq {
                        emit(Event::Accepted { seq });
                    }
                    emit(Event::Progress { text: format!("{name} received the message in its current turn.") });
                }
                // If a different adapter started a turn despite the opt-in, don't send the message twice.
                Some("startedNewTurn") => return Err(format!("{name} started an untracked turn while steering")),
                // The agent answered that its turn is idle, so the follow-up prompt will carry the message; Claude's
                // message can't wait on that for the rest of the turn. After an error it waits for the follow-up.
                _ => {
                    let answered = msg.get("error").is_none();
                    if answered && let Some(seq) = seq {
                        emit(Event::Accepted { seq });
                    }
                    pending.push_front((if answered { None } else { seq }, text));
                    steer = false;
                    emit(Event::Progress { text: format!("{name} will take the message after its current turn.") });
                }
            }
        }
    };
    let stop = done["stopReason"].as_str().unwrap_or("end_turn");
    match (stop, messages.last()) {
        ("end_turn", Some(text)) => Ok(text),
        ("end_turn", None) => Err(format!("{name} finished without writing a report")),
        ("cancelled", _) => Err(match messages.refused {
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
    tools: std::collections::HashMap<String, ToolProgress>,
    // Codex sometimes offers only "cancel" for a refusal, which stops the turn.
    refused: Option<String>,
}

#[derive(Default)]
struct ToolProgress {
    title: String,
    waiting_for_target: bool,
    status: String,
    output: String,
}

/// What a tool call acts on, from its input: the command, the pattern searched for, the address, or else the file.
fn tool_target(input: &Value) -> Option<&str> {
    ["command", "pattern", "query", "url", "path", "Path", "filePath"].iter().find_map(|key| input[key].as_str()).filter(|t| !t.is_empty())
}

impl Messages {
    fn update(&mut self, agent: Agent, update: &Value, emit: &mut dyn FnMut(Event)) {
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
            Some("agent_thought_chunk") => {
                if let Some(text) = update["content"]["text"].as_str() {
                    emit(Event::Thought { text: text.into() });
                }
            }
            Some("plan") => {
                let entries: Vec<_> = update["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| Some(format!("- {} ({})", entry["content"].as_str()?, entry["status"].as_str().unwrap_or("pending").replace('_', " "))))
                    .collect();
                if !entries.is_empty() {
                    emit(Event::Progress { text: format!("Plan\n{}", entries.join("\n")) });
                }
            }
            Some("tool_call") => {
                self.close();
                self.tool_progress(agent, update, true, emit);
            }
            Some("tool_call_update") => self.tool_progress(agent, update, false, emit),
            _ => {}
        }
    }

    fn tool_progress(&mut self, agent: Agent, update: &Value, started: bool, emit: &mut dyn FnMut(Event)) {
        let id = update["toolCallId"].as_str();
        let mut untracked = ToolProgress::default();
        let tool = match id {
            Some(id) => self.tools.entry(id.into()).or_default(),
            None => &mut untracked,
        };
        let target = tool_target(&update["rawInput"]);
        if started {
            let title = update["title"].as_str().filter(|title| !title.is_empty()).unwrap_or("a tool");
            tool.title = title.into();
            // OpenCode may start with only the tool name and supply its target in the first update.
            tool.waiting_for_target = agent.provider().deferred_tool_target && !title.contains(' ') && target.is_none() && id.is_some();
            if !tool.waiting_for_target {
                if let Some(target) = target.filter(|_| !title.contains(' ')) {
                    tool.title = format!("{title} {target}");
                }
                emit(Event::Step { text: tool.title.clone() });
            }
        } else if tool.waiting_for_target {
            tool.waiting_for_target = false;
            if let Some(target) = target {
                tool.title = format!("{} {target}", tool.title);
            }
            emit(Event::Step { text: tool.title.clone() });
        } else if let Some(title) = update["title"].as_str().filter(|title| !title.is_empty()) {
            tool.title = title.into();
        }
        if let Some(status) = update["status"].as_str().filter(|status| *status != tool.status) {
            tool.status = status.into();
            if !started || status == "failed" {
                let title = if tool.title.is_empty() { id.unwrap_or("Tool") } else { &tool.title };
                emit(Event::Progress { text: format!("{title} · {}", status.replace('_', " ")) });
            }
        }
        let metadata = &update["_meta"];
        for key in ["terminal_output_delta", "mcp_output_delta"] {
            if let Some(text) = metadata[key]["data"].as_str().filter(|text| !text.is_empty()) {
                emit(Event::Output { text: text.into() });
            }
        }
        let content: Vec<_> = update["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["type"] == "content" && item["content"]["type"] == "text")
            .filter_map(|item| item["content"]["text"].as_str())
            .collect();
        // On Codex starts, ACP content may describe the input. Result snapshots arrive in updates.
        let output = if started || metadata["terminal_output_delta"].is_object() || metadata["mcp_output_delta"].is_object() {
            None
        } else if content.is_empty() {
            let raw = &update["rawOutput"];
            raw.as_str().or_else(|| raw["formatted_output"].as_str()).or_else(|| raw["output"].as_str()).map(str::to_owned)
        } else {
            Some(content.join("\n"))
        };
        if let Some(output) = output {
            let added = output.strip_prefix(&tool.output).unwrap_or(&output);
            if !added.is_empty() {
                emit(Event::Output { text: added.into() });
            }
            tool.output = output;
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
        return agent.provider().sign_in.into();
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
        Request { agent, model: Some("gpt-5.5".into()), edits, cwd: PathBuf::from("/work"), prompt: "Find the bug".into(), effort: None, live_messages: false }
    }

    #[test]
    fn codex_gets_the_effort_claude_asked_for_and_max_becomes_xhigh() {
        assert_eq!(codex_config(Some("gpt-5.5"), Some("high")), Some(json!({ "model": "gpt-5.5", "model_reasoning_effort": "high" })));
        assert_eq!(codex_config(None, Some("max")), Some(json!({ "model_reasoning_effort": "xhigh" })));
        assert_eq!(codex_config(Some("gpt-5.5"), Some("turbo")), Some(json!({ "model": "gpt-5.5" })), "an unknown level is left to the user's config");
        assert_eq!(codex_config(None, None), None);
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
        talk_request(&request(agent, edits), agent_says)
    }

    fn talk_request(request: &Request, agent_says: &[Value]) -> (Result<String, String>, Vec<Value>, Vec<Event>) {
        let script: String = agent_says.iter().map(|m| format!("{m}\n")).collect();
        let mut sent = Vec::new();
        let mut events = vec![];
        let result = converse(std::io::Cursor::new(script), &mut sent, request, None, &mut |e| events.push(e));
        let sent = String::from_utf8(sent).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        (result, sent, events)
    }

    #[test]
    fn antigravity_configures_each_option_before_prompting() {
        for effort in ["low", "medium", "high", "xhigh", "max", "unknown"] {
            for edits in [false, true] {
                let mut request = request(Agent::Antigravity, edits);
                request.model = Some("gemini-4-neon".into());
                request.effort = Some(effort.into());
                let mut expected = vec![("model", "gemini-4-neon")];
                if effort != "unknown" { expected.push(("thought_level", if effort == "max" { "xhigh" } else { effort })); }
                if edits { expected.push(("mode", "accept-edits")); }
                let mut says = opening();
                for i in 0..expected.len() { says.push(reply(2 + i as i64, json!({}))); }
                says.push(chunk("m", "Done"));
                says.push(reply(2 + expected.len() as i64, json!({ "stopReason": "end_turn" })));
                let (result, sent, _) = talk_request(&request, &says);
                assert_eq!(result, Ok("Done".into()));
                for (i, (config, value)) in expected.iter().enumerate() {
                    assert_eq!(sent[i + 2], json!({ "jsonrpc": "2.0", "id": i + 2, "method": "session/set_config_option", "params": { "sessionId": "s1", "configId": config, "value": value } }));
                }
                assert_eq!(sent.last().unwrap()["method"], "session/prompt");
                assert_eq!(sent.len(), expected.len() + 3);
            }
        }
    }

    /// What agy 1.5.12 does: Gemini 3.8 Flash has no `xhigh`, and the answer to the model change says so.
    #[test]
    fn a_thinking_level_the_model_lacks_becomes_the_highest_it_offers() {
        let levels = |values: &[&str]| json!([{ "id": "thought_level", "category": "thought_level", "options": values.iter().map(|v| json!({ "value": v })).collect::<Vec<_>>() }]);
        let mut request = request(Agent::Antigravity, false);
        request.model = Some("gemini-3.8-flash".into());
        request.effort = Some("max".into());
        let says = [
            reply(0, json!({ "protocolVersion": 1 })),
            reply(1, json!({ "sessionId": "s1", "configOptions": levels(&["low", "medium", "high", "xhigh"]) })),
            reply(2, json!({ "configOptions": levels(&["low", "medium", "high"]) })),
            reply(3, json!({})),
            chunk("m", "Done"),
            reply(4, json!({ "stopReason": "end_turn" })),
        ];
        let (result, sent, _) = talk_request(&request, &says);
        assert_eq!(result, Ok("Done".into()));
        assert_eq!((&sent[3]["params"]["configId"], &sent[3]["params"]["value"]), (&json!("thought_level"), &json!("high")));
        assert_eq!(offered_value(&levels(&["medium", "high"]), "thought_level", "low"), None, "no level at or below: the model's default");
        assert_eq!(offered_value(&levels(&["low"]), "model", "x"), Some("x".into()), "other options go as asked");
    }

    #[test]
    fn antigravity_config_failure_never_prompts() {
        let mut says = opening();
        says.push(json!({ "id": 2, "error": { "code": -32602, "message": "unknown model" } }));
        let (result, sent, _) = talk_to(Agent::Antigravity, &says, false);
        assert_eq!(result, Err("Antigravity could not set model: agy: unknown model".into()));
        assert!(sent.iter().all(|m| m["method"] != "session/prompt"));
    }

    #[test]
    fn antigravity_denies_string_permission_ids_and_keeps_queued_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = request(Agent::Antigravity, false);
        request.model = None;
        let mut says = opening();
        says.push(json!({ "jsonrpc": "2.0", "id": "1", "method": "session/request_permission", "params": { "options": [{ "optionId": "allow", "kind": "allow_once" }, { "optionId": "deny", "kind": "reject_once" }] } }));
        says.push(chunk("m", "Read only"));
        says.push(reply(2, json!({ "stopReason": "end_turn" })));
        let script: String = says.iter().map(|m| format!("{m}\n")).collect();
        let mut sent = vec![];
        let mut events = vec![];
        let result = converse(std::io::Cursor::new(script), &mut sent, &request, Some(test_inbox(dir.path())), &mut |e| events.push(e));
        assert_eq!(result, Ok("Read only".into()));
        let sent: Vec<Value> = String::from_utf8(sent).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(sent[3]["id"], "1");
        assert_eq!(sent[3]["result"]["outcome"]["optionId"], "deny");
        assert!(!events.iter().any(|e| matches!(e, Event::Inbox { .. })));
    }

    #[test]
    fn antigravity_file_targets_and_sign_in_hint() {
        assert_eq!(tool_target(&json!({ "Path": "Cargo.toml" })), Some("Cargo.toml"));
        assert_eq!(error_reason(Agent::Antigravity, &json!({ "code": AUTH_REQUIRED })), "Antigravity isn't signed in. Run `agy` and sign in, then try again.");
    }

    fn talk(agent_says: &[Value], edits: bool) -> (Result<String, String>, Vec<Value>, Vec<Event>) {
        talk_to(Agent::Codex, agent_says, edits)
    }

    fn opening() -> Vec<Value> {
        vec![reply(0, json!({ "protocolVersion": 1 })), reply(1, json!({ "sessionId": "s1" }))]
    }

    fn test_inbox(dir: &Path) -> Inbox {
        Inbox { id: Uuid::new_v4(), path: dir.join("inbox.json"), seen: 0 }
    }

    #[test]
    fn the_inbox_rejects_partial_oversized_out_of_order_and_replayed_deliveries() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = test_inbox(dir.path());
        for body in [
            r#"{"seq":1,"text":"unfinished"#.to_string(),
            json!({ "seq": 1, "text": "   " }).to_string(),
            json!({ "seq": 1, "text": 15 }).to_string(),
            json!({ "seq": 2, "text": "skipped one" }).to_string(),
            json!({ "seq": 1, "text": "a".repeat(INBOX_TEXT_MAX + 1) }).to_string(),
            " ".repeat(INBOX_BYTES as usize + 1),
        ] {
            std::fs::write(&inbox.path, body).unwrap();
            assert_eq!(inbox.take(), None);
        }
        let message = json!({ "seq": 1, "text": "Check the tests too" }).to_string();
        std::fs::write(&inbox.path, &message).unwrap();
        assert_eq!(inbox.take(), Some((1, "Check the tests too".to_string())));
        assert!(!inbox.path.exists());
        std::fs::write(&inbox.path, message).unwrap();
        assert_eq!(inbox.take(), None);
        let path = inbox.path.clone();
        drop(inbox);
        assert!(!path.exists());
    }

    /// Wait for a steer without sending any ACP output after the original prompt.
    fn steer_a_quiet_turn(agent: Agent, parks_message: bool) {
        let dir = tempfile::tempdir().unwrap();
        let inbox = test_inbox(dir.path());
        let path = inbox.path.clone();
        let (from_agent, mut replies) = std::io::pipe().unwrap();
        let (commands, to_agent) = std::io::pipe().unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(commands).lines().map_while(Result::ok) {
                if sent.send(serde_json::from_str::<Value>(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        let task = std::thread::spawn(move || {
            let mut events = Vec::new();
            let report = converse(BufReader::new(from_agent), to_agent, &request(agent, false), Some(inbox), &mut |event| events.push(event));
            (report, events)
        });
        let next = || received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next()["method"], "initialize");
        writeln!(replies, "{}", reply(0, json!({ "protocolVersion": 1, "_meta": { "steering": { "supported": true } } }))).unwrap();
        assert_eq!(next()["method"], "session/new");
        writeln!(replies, "{}", opening()[1]).unwrap();
        assert_eq!(next()["method"], "session/prompt");
        std::fs::write(&path, json!({ "seq": 1, "text": "Check the tests too" }).to_string()).unwrap();
        let steer = next();
        assert_eq!(steer["method"], "_session/steering");
        assert_eq!(steer["params"]["sessionId"], "s1");
        assert_eq!(steer["params"]["prompt"], json!([{ "type": "text", "text": "Check the tests too" }]));
        let idle_behavior = &steer["params"]["_meta"]["steering"]["idleBehavior"];
        if agent == Agent::Codex {
            assert_eq!(idle_behavior, "promptRequired");
        } else {
            assert!(idle_behavior.is_null());
        }
        let steer_id = steer["id"].as_i64().unwrap();
        let finished = json!({ "stopReason": "end_turn" });
        let final_id = if parks_message {
            // The original prompt may finish before the adapter replies to the steer.
            writeln!(replies, "{}", chunk("m1", "Original answer.")).unwrap();
            writeln!(replies, "{}", reply(2, finished.clone())).unwrap();
            writeln!(replies, "{}", reply(steer_id, json!({ "outcome": "promptRequired", "reason": "noRunningTurn" }))).unwrap();
            let follow_up = next();
            assert_eq!(follow_up["method"], "session/prompt");
            assert_eq!(follow_up["params"]["sessionId"], "s1");
            assert_eq!(follow_up["params"]["prompt"], steer["params"]["prompt"]);
            follow_up["id"].as_i64().unwrap()
        } else {
            writeln!(replies, "{}", reply(steer_id, json!({ "outcome": "injected" }))).unwrap();
            2
        };
        writeln!(replies, "{}", chunk("m2", "Checked the tests too.")).unwrap();
        writeln!(replies, "{}", reply(final_id, finished)).unwrap();
        drop(replies);
        let (report, events) = task.join().unwrap();
        assert_eq!(report.as_deref(), Ok("Checked the tests too."));
        assert!(events.contains(&Event::Accepted { seq: 1 }));
        assert!(events.contains(&Event::Text { text: "Checked the tests too.".into() }));
        assert!(!path.exists());
    }

    /// The mod gives a message back to Claude Code's queue unless it is acknowledged, so one that never reached the
    /// agent must not be: the agent exits before answering the steer, or refuses it with an error and then exits.
    fn exit_before_taking_a_message(refuses_first: bool) {
        let dir = tempfile::tempdir().unwrap();
        let inbox = test_inbox(dir.path());
        let path = inbox.path.clone();
        let (from_agent, mut replies) = std::io::pipe().unwrap();
        let (commands, to_agent) = std::io::pipe().unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(commands).lines().map_while(Result::ok) {
                if sent.send(serde_json::from_str::<Value>(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        let task = std::thread::spawn(move || {
            let mut events = Vec::new();
            let report = converse(BufReader::new(from_agent), to_agent, &request(Agent::Codex, false), Some(inbox), &mut |event| events.push(event));
            (report, events)
        });
        let next = || received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next()["method"], "initialize");
        writeln!(replies, "{}", reply(0, json!({ "protocolVersion": 1, "_meta": { "steering": { "supported": true } } }))).unwrap();
        assert_eq!(next()["method"], "session/new");
        writeln!(replies, "{}", opening()[1]).unwrap();
        assert_eq!(next()["method"], "session/prompt");
        std::fs::write(&path, json!({ "seq": 1, "text": "Check the tests too" }).to_string()).unwrap();
        let steer = next();
        assert_eq!(steer["method"], "_session/steering");
        if refuses_first {
            writeln!(replies, "{}", json!({ "jsonrpc": "2.0", "id": steer["id"], "error": { "code": -32603, "message": "busy" } })).unwrap();
        }
        drop(replies);
        let (report, events) = task.join().unwrap();
        assert!(report.is_err());
        assert!(!events.iter().any(|e| matches!(e, Event::Accepted { .. })), "{events:?}");
    }

    #[test]
    fn a_message_is_not_acknowledged_when_the_agent_exits_before_taking_it() {
        exit_before_taking_a_message(false);
    }

    #[test]
    fn a_message_is_not_acknowledged_when_the_agent_refuses_the_steer_and_exits() {
        exit_before_taking_a_message(true);
    }

    #[test]
    fn codex_can_be_steered_while_acp_is_quiet() {
        steer_a_quiet_turn(Agent::Codex, false);
    }

    #[test]
    fn codex_follows_up_in_the_same_session_when_steering_finds_an_idle_turn() {
        steer_a_quiet_turn(Agent::Codex, true);
    }

    #[test]
    fn opencode_can_be_steered_when_it_advertises_support() {
        steer_a_quiet_turn(Agent::OpenCode, false);
    }

    #[test]
    fn opencode_without_steering_leaves_delivery_to_claude() {
        let dir = tempfile::tempdir().unwrap();
        let mut says = opening();
        says.extend([chunk("m1", "Original answer."), reply(2, json!({ "stopReason": "end_turn" }))]);
        let script: String = says.iter().map(|m| format!("{m}\n")).collect();
        let mut events = Vec::new();
        let report = converse(std::io::Cursor::new(script), std::io::sink(), &request(Agent::OpenCode, false), Some(test_inbox(dir.path())), &mut |event| events.push(event));
        assert_eq!(report.as_deref(), Ok("Original answer."));
        assert!(!events.iter().any(|event| matches!(event, Event::Inbox { .. })));
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
                Event::Progress { text: "Session ready. Sending the task.".into() },
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
    fn thoughts_plans_and_tool_updates_stream_without_replacing_the_report() {
        let mut says = opening();
        says.extend([
            update(json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "Check the parser." } })),
            update(json!({ "sessionUpdate": "plan", "entries": [{ "content": "Run tests", "status": "in_progress" }] })),
            update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Run tests", "status": "in_progress" })),
            update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "_meta": { "terminal_output_delta": { "data": "running\n" } } })),
            update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "rawOutput": { "formatted_output": "started\n" } })),
            update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "failed", "content": [
                { "type": "content", "content": { "type": "text", "text": "started\nfailed\n" } },
            ] })),
            update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "failed", "rawOutput": "started\nfailed\n" })),
            chunk("m1", "The parser test failed."),
            update(json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "Thought after the report." } })),
            reply(2, json!({ "stopReason": "end_turn" })),
        ]);
        let (report, _, events) = talk(&says, false);
        assert_eq!(report, Ok("The parser test failed.".into()));
        assert_eq!(events, [
            Event::Progress { text: "Session ready. Sending the task.".into() },
            Event::Thought { text: "Check the parser.".into() },
            Event::Progress { text: "Plan\n- Run tests (in progress)".into() },
            Event::Step { text: "Run tests".into() },
            Event::Output { text: "running\n".into() },
            Event::Output { text: "started\n".into() },
            Event::Progress { text: "Run tests · failed".into() },
            Event::Output { text: "failed\n".into() },
            Event::Text { text: "The parser test failed.".into() },
            Event::Thought { text: "Thought after the report.".into() },
        ]);
    }

    /// OpenCode names only the tool when the call starts and sends what it acts on in the call's first update.
    #[test]
    fn a_step_named_only_by_its_tool_waits_for_what_it_acts_on() {
        let call = |id: &str, title: &str| update(json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": title, "status": "pending", "rawInput": {} }));
        let running = |id: &str, title: &str, input: Value| update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "title": title, "status": "in_progress", "rawInput": input }));
        let done = |id: &str| update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "title": null, "status": "completed", "rawInput": null }));
        let mut says = opening();
        says.extend([
            call("c1", "read"),
            call("c2", "shell"),
            call("c3", "todowrite"),
            call("c4", "grep"),
            running("c4", "grep", json!({ "pattern": "fn main", "path": "/work/src" })),
            running("c1", "read", json!({ "path": "note.json" })),
            running("c2", "echo hi", json!({ "command": "echo hi", "cwd": "/work" })),
            done("c1"),
            done("c3"),
            done("c2"),
            chunk("m1", "Done."),
            reply(2, json!({ "stopReason": "end_turn" })),
        ]);
        let steps: Vec<String> = talk_to(Agent::OpenCode, &says, false).2.into_iter().filter_map(|e| if let Event::Step { text } = e { Some(text) } else { None }).collect();
        assert_eq!(steps, ["grep fn main", "read note.json", "shell echo hi", "todowrite"]);
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
        assert_eq!(events, vec![Event::Progress { text: "Session ready. Sending the task.".into() }, Event::Text { text: "Asked a helper.".into() }]);
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
    #[cfg(unix)]
    fn a_program_only_the_login_shell_put_on_the_path_is_found() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("claudiu-acp-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("claudiu-acp-path-probe");
        std::fs::write(&tool, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _only_test_using_login_path = crate::platform::LOGIN_PATH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = crate::platform::LOGIN_PATH.lock().unwrap().clone();
        *crate::platform::LOGIN_PATH.lock().unwrap() = Some(dir.as_os_str().to_os_string());
        let found = locate_reloading("claudiu-acp-path-probe");
        *crate::platform::LOGIN_PATH.lock().unwrap() = previous;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, Some(tool));
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
    fn antigravity_lists_its_model_config_option_without_prompting() {
        let says = [reply(0, json!({ "protocolVersion": 1, "agentInfo": { "name": "Antigravity ACP Server" } })), reply(1, json!({
            "sessionId": "s1",
            "configOptions": [
                { "id": "thought_level", "category": "thought_level", "options": [{ "value": "high", "name": "High" }] },
                { "id": "model", "category": "model", "options": [{ "value": "gemini-4-neon", "name": "Gemini 4 Neon" }] },
                { "id": "mode", "category": "mode", "options": [{ "value": "default", "name": "Default" }] }
            ],
            "_meta": { "antigravity": { "tier": { "name": "Google AI Pro" } } }
        }))];
        let (models, sent) = list(Agent::Antigravity, &says);
        assert_eq!(models.unwrap(), vec![AgentModel { id: "gemini-4-neon".into(), name: "Gemini 4 Neon".into() }]);
        assert_eq!(sent.iter().map(|m| m["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "session/new"]);
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
