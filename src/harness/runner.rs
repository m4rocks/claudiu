//! Runs one task in a custom subagent's provider (`harness/subagents.rs`), through the Agent Client
//! Protocol: `m4code --acp`. The Custom subagents settings page also asks the agent for its models here (`models`).
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

use super::subagents::{Agent, AgentModel};
use crate::platform::{find_executable, reload_path};

pub const FLAG: &str = "--acp";
/// ACP's error code for "authentication required".
const AUTH_REQUIRED: i64 = -32000;
/// Lines of the agent's stderr kept for a failure's reason.
const STDERR_LINES: usize = 5;
/// How long the settings page waits for the agent's models; Codex's first run downloads codex-acp through npx.
const MODELS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const INBOX_POLL: Duration = Duration::from_millis(100);
/// How long an agent that resumes after a background command must stay quiet, with nothing running, before its turn
/// counts as over: agy marks the end of its own continuation only with a usage update, which also comes just before a
/// new tool call.
const RESUMED_QUIET: Duration = Duration::from_secs(3);
/// The longest wait for background commands, so one that never exits, such as a dev server, can't hold the task forever.
const BACKGROUND_WAIT: Duration = Duration::from_secs(30 * 60);
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
        Some(super::antigravity::read_only::HOOK_FLAG) => return super::antigravity::read_only::run_hook(),
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
    let harness = super::Harness::from_agent(request.agent);
    if !harness.is_registered() {
        return Err(format!("{} is unavailable in this build.", harness.name()));
    }
    let provider = request.agent.provider();
    let args = (provider.task_args)(request)?;
    let mut plan = super::SessionPlan::default();
    plan.env.extend((provider.environment)(request).into_iter().map(|(key, value)| (key.into(), value)));
    // This child cannot represent itself as its parent's desktop thread. Give it the shared
    // workspace tool independently of tools that require a registered m4code thread.
    let mut tools = crate::mcp::session::SessionTools::new("", &request.cwd, None, &["github", "sessions", "model", "ask_user", "visual"], &[])?;
    tools.load_workspace();
    let adapter = harness.adapter();
    (adapter.configure_mcp)(&tools, &mut plan)?;
    let env = plan.env.iter().map(|(key, value)| (key.as_str(), value.clone())).collect::<Vec<_>>();
    let connection = Connection::start(request.agent, &request.cwd, &args, &env)?;
    let id = Uuid::new_v4();
    let inbox = request.live_messages.then(|| Inbox { id, path: inbox_path(id), seen: 0 });
    let result = conversation::converse_with_tools(connection.from_agent, connection.to_agent, request, inbox, emit, plan.mcp_servers, &tools.ready);
    connection.process.finish(result)
}

pub(super) fn codex_env(request: &Request) -> Vec<(&'static str, String)> {
    let mut env = vec![("INITIAL_AGENT_MODE", (if request.edits { "workspace-write" } else { "read-only" }).to_string())];
    if let Some(config) = codex_config(request.model.as_deref(), request.effort.as_deref()) {
        env.push((super::codex::CONFIG_ENV, config.to_string()));
    }
    env
}

pub(super) fn opencode_env(request: &Request) -> Vec<(&'static str, String)> {
    let mut config = std::env::var("OPENCODE_CONFIG_CONTENT")
        .ok()
        .filter(|text| !text.trim().is_empty())
        .and_then(|text| serde_json::from_str::<serde_json::Map<String, Value>>(&text).ok())
        .unwrap_or_default();
    if let Some(overrides) = opencode_config(request.model.as_deref(), request.edits).as_object() {
        for (k, v) in overrides {
            config.insert(k.clone(), v.clone());
        }
    }
    vec![("OPENCODE_CONFIG_CONTENT", Value::Object(config).to_string())]
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
    let m4code = crate::platform::own_exe().map_err(|e| format!("m4code couldn't find itself to keep Antigravity read-only ({e})"))?;
    let gemini = super::antigravity::read_only::prepare(&crate::datadir::data_dir(), &m4code).map_err(|e| format!("m4code couldn't set up Antigravity's read-only folder ({e})"))?;
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
    // Read-only keeps the default mode, where agy asks before each edit or command and m4code refuses.
    if request.edits {
        options.push(("mode", "accept-edits".into()));
    }
    options
}

/// Thinking levels from least to most, as ACP agents name them.
const THOUGHT_LEVELS: [&str; 4] = ["low", "medium", "high", "xhigh"];

mod config;
use config::{codex_config, effort_level, offered_value, opencode_config};

/// The models and permission modes `harness`'s ACP server offers this account, as a new session lists them. Gives up
/// after `MODELS_TIMEOUT`, so a stalled download or an agent that never answers can't leave the UI loading for good.
pub(crate) fn probe_server(
    harness: crate::harness::Harness,
    program: PathBuf,
    args: Vec<String>,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<crate::harness::HarnessCatalog, String> {
    let adapter = harness.adapter();
    let mut command = Command::new(program);
    command.args(&args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::platform::no_window(&mut command);
    crate::platform::with_login_path(&mut command);
    for (key, value) in env {
        command.env(key, value);
    }
    let name = adapter.server_name;
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
    let folder = cwd.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    // Killing the agent on a timeout closes its pipes, which ends this thread too.
    std::thread::spawn(move || {
        let _ = tx.send(list_catalog_for(harness, BufReader::new(from_agent), to_agent, &folder));
    });
    let result = rx.recv_timeout(MODELS_TIMEOUT).unwrap_or_else(|_| Err(format!("{name} didn't answer within {} seconds", MODELS_TIMEOUT.as_secs())));
    kill_process_tree(&mut child);
    let early_exit = exited_harness(harness);
    match result {
        Err(reason) if reason == early_exit => {
            let stderr = errors.join().unwrap_or_default();
            Err(if stderr.is_empty() { reason } else { format!("{early_exit}: {stderr}") })
        }
        other => other,
    }
}

/// The program m4code starts for `agent`, as failures name it.
fn server(agent: Agent) -> &'static str {
    agent.provider().server
}

fn exited_harness(harness: crate::harness::Harness) -> String {
    format!("{} exited before {} answered", harness.adapter().server_name, harness.name())
}

fn exited(agent: Agent) -> String {
    exited_harness(crate::harness::Harness::from_agent(agent))
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

/// The program on the PATH. A desktop launch has a minimal PATH, so OpenCode or Node installed since m4code started,
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
        let (program, args) = (super::Harness::from_agent(agent).adapter().server)()?;
        let mut command = Command::new(program);
        command.args(args).args(task_args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
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
        kill_process_tree(&mut self.child);
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

fn kill_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let mut kill_tree = Command::new("taskkill");
        kill_tree.args(["/F", "/T", "/PID", &child.id().to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        crate::platform::no_window(&mut kill_tree);
        let _ = kill_tree.status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn initialize() -> Value {
    json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": 1,
        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
        "clientInfo": { "name": "m4code", "version": env!("CARGO_PKG_VERSION") },
    }})
}

fn new_session(cwd: &Path) -> Value {
    session_with_servers(cwd, &[])
}

fn session_with_servers(cwd: &Path, mcp_servers: &[Value]) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": "session/new", "params": { "cwd": cwd, "mcpServers": mcp_servers } })
}

fn new_model_probe_session(harness: crate::harness::Harness, cwd: &Path) -> Value {
    if harness == crate::harness::Harness::Claude {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {
                "cwd": cwd,
                "mcpServers": [],
                "_meta": { "claudeCode": { "options": { "persistSession": false } } }
            }
        })
    } else {
        new_session(cwd)
    }
}

/// Opens a session only to read its model picker (`configOptions[category == "model"]`) and permission modes
/// (`modes.availableModes` or `configOptions[category == "mode"]`). Harnesses that keep every empty session in their
/// history (OpenCode) delete this probe session again.
fn list_catalog_for(harness: crate::harness::Harness, from_agent: impl BufRead, mut to_agent: impl Write, cwd: &Path) -> Result<crate::harness::HarnessCatalog, String> {
    let adapter = harness.adapter();
    let mut lines = from_agent.lines();
    let mut send = |message: Value| writeln!(to_agent, "{message}").and_then(|_| to_agent.flush()).map_err(|_| exited_harness(harness));
    let mut reply = |id: i64| -> Result<Value, String> {
        for line in lines.by_ref() {
            let msg: Value = serde_json::from_str(&line.map_err(|_| exited_harness(harness))?).unwrap_or_default();
            if msg.get("method").is_none() && msg["id"].as_i64() == Some(id) {
                return match msg.get("error") {
                    Some(error) => Err(harness_error_reason(harness, error)),
                    None => Ok(msg["result"].clone()),
                };
            }
        }
        Err(exited_harness(harness))
    };
    send(initialize())?;
    reply(0)?;
    send(new_model_probe_session(harness, cwd))?;
    let session = reply(1)?;
    let options = session["configOptions"].as_array().into_iter().flatten();
    let picker = options.clone().filter(|o| o["category"] == "model").find_map(|o| o["options"].as_array());
    let raw_models: Vec<AgentModel> = picker
        .into_iter()
        .flatten()
        .filter_map(|o| Some(AgentModel { id: o["value"].as_str()?.to_string(), name: o["name"].as_str().unwrap_or_default().to_string() }))
        .collect();
    let real_models: Vec<AgentModel> = raw_models.iter().filter(|m| m.id != "default").cloned().collect();
    let models = if real_models.is_empty() { raw_models } else { real_models };

    let mut modes: Vec<(String, String)> = session
        .pointer("/modes/availableModes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m["id"].as_str()?.to_string(), m["name"].as_str().unwrap_or_default().to_string())))
        .collect();
    if modes.is_empty() {
        modes = options
            .filter(|o| o["category"] == "mode")
            .find_map(|o| o["options"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|o| Some((o["value"].as_str()?.to_string(), o["name"].as_str().unwrap_or_default().to_string())))
            .collect();
    }

    if adapter.delete_model_session
        && let Some(id) = session["sessionId"].as_str()
        // The models are known already, so a failed delete only leaves an empty session behind.
        && send(json!({ "jsonrpc": "2.0", "id": 2, "method": "session/delete", "params": { "sessionId": id } })).is_ok()
    {
        let _ = reply(2);
    }
    if models.is_empty() {
        Err(format!("{} listed no models", adapter.server_name))
    } else {
        Ok(crate::harness::HarnessCatalog { models, modes })
    }
}

#[cfg(test)]
fn list_models_for(harness: crate::harness::Harness, from_agent: impl BufRead, to_agent: impl Write, cwd: &Path) -> Result<Vec<AgentModel>, String> {
    list_catalog_for(harness, from_agent, to_agent, cwd).map(|c| c.models)
}

#[cfg(test)]
fn list_models(agent: Agent, from_agent: impl BufRead, to_agent: impl Write, cwd: &Path) -> Result<Vec<AgentModel>, String> {
    list_models_for(crate::harness::Harness::from_agent(agent), from_agent, to_agent, cwd)
}

mod conversation;
#[cfg(test)]
use conversation::converse;
#[cfg(test)]
use conversation::converse_waiting;

mod messages;
use messages::Messages;
#[cfg(test)]
use messages::tool_target;

/// The answer to a permission request: an allowing option when edits are on, a refusing one when they are off.
/// codex-acp offers two refusals of the same kind: `decline` lets Codex carry on without the action, `cancel` ends its
/// turn, so `decline` comes first. Without a fitting option the request is answered as cancelled, which refuses it too.
pub(crate) fn permission(options: &Value, edits: bool) -> Value {
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

fn harness_error_reason(harness: crate::harness::Harness, error: &Value) -> String {
    if error["code"].as_i64() == Some(AUTH_REQUIRED) {
        return match harness.agent() {
            Some(agent) => agent.provider().sign_in.into(),
            None => "Sign in to Claude Code (`claude auth login`) first.".into(),
        };
    }
    let server = harness.adapter().server_name;
    match error["message"].as_str() {
        Some(message) => format!("{server}: {message}"),
        None => format!("{server}: {error}"),
    }
}

fn error_reason(agent: Agent, error: &Value) -> String {
    harness_error_reason(crate::harness::Harness::from_agent(agent), error)
}

#[cfg(test)]
mod tests;
