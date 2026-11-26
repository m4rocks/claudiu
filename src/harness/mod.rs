//! The coding agents m4code runs sessions in, each through its own ACP server. The core of the app treats them
//! alike; what is particular to one adapter lives inside its own submodule (`claude/`, `codex/`, `opencode`,
//! `antigravity/`). No GPUI here.

pub mod acp;
pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod compaction;
pub mod defaults;
pub mod limits;
#[cfg(feature = "adapter-opencode")]
pub mod opencode;
pub mod permission;
pub mod runner;
pub mod subagents;
pub mod text_generation;
pub mod thread;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use self::subagents::{Agent, AgentModel};
use self::permission::Permission;
use crate::mcp::session::SessionTools;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    #[default]
    Claude,
    Codex,
    OpenCode,
    Antigravity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubagentAccess {
    pub on: bool,
    pub edits: bool,
}

impl Default for SubagentAccess {
    fn default() -> Self {
        Self { on: true, edits: true }
    }
}

/// An ACP server's program and its arguments.
pub type Server = (PathBuf, Vec<String>);
pub type Environment = Vec<(String, String)>;

/// One selectable model from an enabled harness, aggregated into a single model picker across the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogModel {
    pub harness: Harness,
    /// Model id sent to the harness (`""` means let the harness pick its default).
    pub id: String,
    pub name: String,
}

impl CatalogModel {
    /// Built-in Claude Code aliases used before a Claude Code session reports its live model list.
    pub fn default_claude_models() -> Vec<Self> {
        [("opus", "Claude Opus"), ("sonnet", "Claude Sonnet"), ("haiku", "Claude Haiku")]
            .into_iter()
            .map(|(id, name)| Self { harness: Harness::Claude, id: id.into(), name: name.into() })
            .collect()
    }
}

/// How m4code drives one harness: the parts that differ between them. Each lives in its own file.
pub struct Adapter {
    pub name: &'static str,
    /// Program / bridge name shown in error messages when listing models.
    pub server_name: &'static str,
    /// The ACP server's program and arguments, or why it can't start.
    pub server: fn() -> Result<Server, String>,
    /// How a stored session opens again.
    pub reopen: Reopen,
    /// Whether the empty session opened only to read `configOptions[category == "model"]` should be deleted again.
    pub delete_model_session: bool,
    pub compact_command: Option<&'static str>,
    /// Whether the agent takes a message in the turn it is working on (`_m4code/steer`, the Claude Code bridge's own).
    pub steers: bool,
    /// Fallback model entries shown before the adapter's ACP server reports its live catalog.
    pub default_models: fn() -> Vec<CatalogModel>,
    pub prepare: fn(&SessionInput<'_>, &mut SessionPlan) -> Result<(), String>,
    pub configure_mcp: fn(&SessionTools, &mut SessionPlan) -> Result<(), String>,
    pub launch_args: fn(Option<&str>, bool) -> Vec<String>,
    pub modes: [Option<&'static str>; 3],
    pub mode_aliases: &'static [(&'static str, Permission)],
    pub full_access_relaunch: bool,
    pub native_compaction_managed: bool,
    pub fallback_question: bool,
    pub discover: fn(Option<&Path>) -> Option<PathBuf>,
    pub probe_env: fn(Option<&Path>) -> Result<Environment, String>,
    pub config_root: fn(&Path) -> PathBuf,
    pub project_skills: &'static str,
    pub hidden_commands: &'static [&'static str],
    pub worktree: WorktreeHooks,
    pub protocol: ProtocolHooks,
}

pub struct SessionInput<'a> {
    pub record: &'a str,
    pub cwd: &'a Path,
    pub session_id: Option<&'a str>,
    pub name: Option<String>,
    pub child_edits: Option<bool>,
    pub settings: &'a crate::store::Settings,
    pub executable_override: Option<&'a Path>,
    pub chrome: bool,
}

#[derive(Default)]
pub struct SessionPlan {
    pub env: Vec<(String, String)>,
    pub args: Vec<String>,
    pub meta: Value,
    pub mcp_servers: Vec<Value>,
    pub off: Vec<&'static str>,
}

impl SessionPlan {
    pub fn set_env(&mut self, key: &str, value: String) {
        self.env.retain(|(name, _)| name != key);
        self.env.push((key.into(), value));
    }
}

pub struct AsyncOption {
    pub label: String,
    pub description: Option<String>,
}

pub struct AsyncQuestion {
    pub title: String,
    pub options: Vec<AsyncOption>,
    pub multi_select: bool,
}

pub struct AsyncAsk {
    pub questions: Vec<AsyncQuestion>,
    pub echoed_text: String,
}

pub struct BackgroundTask {
    pub id: String,
    pub label: String,
    pub log_path: Option<PathBuf>,
}

pub struct BackgroundHooks {
    pub from_update: fn(&thread::ToolCall, &Value) -> BackgroundUpdate,
    pub finished: fn(&str, &str) -> bool,
    /// When true, `execute` tools left `Running` at `turn_ended` stay live until their completion update arrives.
    pub live_execute_tools: bool,
}

#[derive(Default)]
pub struct BackgroundUpdate {
    pub started: Option<BackgroundTask>,
    /// Task ids explicitly retired by this tool call (e.g. Codex `write_stdin` reporting exit for `rawInput.session_id`).
    pub finished: Vec<String>,
    pub output: Vec<String>,
}

/// Collect plain-text output surfaces from a tool call (`details.output`, `stdout`, `stderr`, and text content).
pub(crate) fn tool_output_texts(call: &thread::ToolCall) -> Vec<&str> {
    let mut texts = Vec::new();
    if let Some(out) = call.details.output.as_deref() {
        texts.push(out);
    }
    if let Some(out) = call.details.stdout.as_deref() {
        texts.push(out);
    }
    if let Some(out) = call.details.stderr.as_deref() {
        texts.push(out);
    }
    for content in &call.content {
        if let thread::ToolContent::Text(text) = content {
            texts.push(text.as_str());
        }
    }
    texts
}

/// Human-readable single-line label for a background task started by `call`.
pub(crate) fn background_task_label(call: &thread::ToolCall) -> String {
    call.details
        .command
        .as_deref()
        .or(Some(call.title.as_str()))
        .and_then(|s| s.lines().find(|l| !l.trim().is_empty()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Background task".into())
}

/// Output text snapshots from `call` and `update["rawOutput"]`, used to detect background task completion notices.
pub(crate) fn background_output_texts(call: &thread::ToolCall, update: &Value) -> Vec<String> {
    let mut output = tool_output_texts(call).into_iter().map(str::to_string).collect::<Vec<_>>();
    if let Some(raw) = update.get("rawOutput") {
        if let Some(text) = raw.as_str() {
            output.push(text.to_string());
        } else {
            for key in ["formatted_output", "output", "stdout", "stderr"] {
                if let Some(text) = raw.get(key).and_then(Value::as_str) {
                    output.push(text.to_string());
                }
            }
        }
    }
    output
}

pub struct ProtocolHooks {
    pub async_ask: Option<fn(&Value) -> Option<AsyncAsk>>,
    pub background: Option<BackgroundHooks>,
    pub usage_limits: fn(&Value, i64) -> Option<crate::store::UsageLimits>,
    pub ignore_update: fn(&Value) -> bool,
}

impl ProtocolHooks {
    const DEFAULT: Self = Self {
        async_ask: None,
        background: None,
        usage_limits: |_, _| None,
        ignore_update: |_| false,
    };
}

pub struct WorktreeHooks {
    pub inspect: fn(&str) -> (Option<bool>, Option<PathBuf>),
    pub can_resume: fn(&Path) -> Result<bool, String>,
    pub has_history: fn(Option<&str>, bool) -> bool,
}

impl WorktreeHooks {
    const DEFAULT: Self = Self {
        inspect: |_| (None, None),
        can_resume: |_| Ok(true),
        has_history: |id, _| id.is_some(),
    };
}

fn stdio_mcp(tools: &SessionTools, plan: &mut SessionPlan) -> Result<(), String> {
    plan.mcp_servers.push(tools.stdio());
    Ok(())
}

fn no_probe_env(_: Option<&Path>) -> Result<Vec<(String, String)>, String> {
    Ok(Vec::new())
}

fn no_launch_args(_: Option<&str>, _: bool) -> Vec<String> {
    Vec::new()
}

fn child_request(agent: Agent, input: &SessionInput<'_>, edits: bool) -> runner::Request {
    runner::Request {
        agent, model: None, effort: None, edits, cwd: input.cwd.to_path_buf(),
        prompt: String::new(), live_messages: false,
    }
}

static UNAVAILABLE_ADAPTER: Adapter = Adapter {
    name: "Unavailable adapter",
    server_name: "unavailable",
    server: || Err("This adapter is unavailable in this build.".into()),
    reopen: Reopen::Load,
    delete_model_session: false,
    compact_command: None,
    steers: false,
    default_models: Vec::new,
    prepare: |_, _| Err("This adapter is unavailable in this build.".into()),
    configure_mcp: stdio_mcp,
    launch_args: no_launch_args,
    modes: [None; 3],
    mode_aliases: &[],
    full_access_relaunch: false,
    native_compaction_managed: false,
    fallback_question: false,
    discover: |_| None,
    probe_env: no_probe_env,
    config_root: Path::to_path_buf,
    project_skills: "",
    hidden_commands: &[],
    worktree: WorktreeHooks::DEFAULT,
    protocol: ProtocolHooks::DEFAULT,
};

/// Default single placeholder model for an adapter before its ACP server reports a live catalog.
pub(crate) fn default_model_entry(harness: Harness) -> Vec<CatalogModel> {
    vec![CatalogModel { harness, id: String::new(), name: format!("{} (default)", harness.name()) }]
}

/// One value per harness, indexed exhaustively without hashing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessMap<T> {
    claude: T,
    codex: T,
    opencode: T,
    antigravity: T,
}

impl<T> HarnessMap<T> {
    pub fn get(&self, harness: Harness) -> &T {
        match harness {
            Harness::Claude => &self.claude,
            Harness::Codex => &self.codex,
            Harness::OpenCode => &self.opencode,
            Harness::Antigravity => &self.antigravity,
        }
    }

    pub fn get_mut(&mut self, harness: Harness) -> &mut T {
        match harness {
            Harness::Claude => &mut self.claude,
            Harness::Codex => &mut self.codex,
            Harness::OpenCode => &mut self.opencode,
            Harness::Antigravity => &mut self.antigravity,
        }
    }
}

/// Models and permission modes reported by a harness's ACP adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessCatalog {
    pub models: Vec<AgentModel>,
    pub modes: Vec<(String, String)>,
}

/// Ask `harness`'s ACP adapter for the models and permission modes it offers, using the same
/// `initialize` -> `session/new` handshake for every harness (Claude Code, Codex, OpenCode, and Antigravity).
pub fn probe(harness: Harness, cwd: &Path, env: &[(String, String)]) -> Result<HarnessCatalog, String> {
    let (program, args) = (harness.adapter().server)()?;
    runner::probe_server(harness, program, args, cwd, env)
}

/// ACP has two ways to open a stored session: `session/load` replays the conversation into the client, and
/// `session/resume` picks it up without the replay. A harness without load is resumed, so its old messages aren't
/// shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reopen {
    Load,
    Resume,
}

impl Harness {
    pub const ALL: [Harness; 4] = [Self::Claude, Self::Codex, Self::OpenCode, Self::Antigravity];

    const REGISTRY: &'static [(Self, &'static Adapter)] = &[
        #[cfg(feature = "adapter-claude")]
        (Self::Claude, &claude::ADAPTER),
        #[cfg(feature = "adapter-codex")]
        (Self::Codex, &codex::ADAPTER),
        #[cfg(feature = "adapter-opencode")]
        (Self::OpenCode, &opencode::ADAPTER),
        #[cfg(feature = "adapter-antigravity")]
        (Self::Antigravity, &antigravity::ADAPTER),
    ];

    pub fn is_registered(self) -> bool {
        Self::registered().any(|h| h == self)
    }

    pub fn registered() -> impl Iterator<Item = Self> {
        Self::REGISTRY.iter().map(|(id, _)| *id)
    }

    pub fn adapter(self) -> &'static Adapter {
        Self::REGISTRY.iter().find(|(id, _)| *id == self)
            .map_or(&UNAVAILABLE_ADAPTER, |(_, adapter)| *adapter)
    }

    pub fn name(self) -> &'static str {
        if self.is_registered() {
            self.adapter().name
        } else {
            self.key()
        }
    }

    /// The harness's short lowercase key, as saved settings and the `set_model` tool spell it.
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Antigravity => "antigravity",
        }
    }

    /// m4code owns session naming and navigation, model controls, permissions, and status across harnesses.
    /// Provider-specific entries below are terminal controls or settings already managed by the app.
    pub fn hides_command(self, name: &str) -> bool {
        matches!(name, "rename" | "model" | "effort" | "permissions" | "resume" | "status" | "quit" | "exit")
            || self.adapter().hidden_commands.contains(&name)
    }

    pub fn from_agent(agent: Agent) -> Self {
        match agent {
            Agent::Codex => Self::Codex,
            Agent::OpenCode => Self::OpenCode,
            Agent::Antigravity => Self::Antigravity,
        }
    }

    pub fn agent(self) -> Option<Agent> {
        match self {
            Self::Claude => None,
            Self::Codex => Some(Agent::Codex),
            Self::OpenCode => Some(Agent::OpenCode),
            Self::Antigravity => Some(Agent::Antigravity),
        }
    }
}

/// The server of a harness that shares its ACP server with a subagent provider.
fn subagent_server(agent: Agent) -> Result<Server, String> {
    let provider = agent.provider();
    let program = runner::locate(agent).ok_or(provider.missing)?;
    Ok((program, provider.args.iter().map(|s| s.to_string()).collect()))
}

/// `name` on the PATH; a desktop launch's PATH may lack what only the login shell's startup files add.
fn find(name: &str) -> Option<PathBuf> {
    crate::platform::find_executable(name).or_else(|| {
        crate::platform::reload_path();
        crate::platform::find_executable(name)
    })
}

/// One MCP server for `session/new` and `session/load`, in ACP's stdio form.
pub fn stdio_server(name: &str, command: &Path, args: &[&str], env: &[(&str, String)]) -> Value {
    json!({
        "name": name,
        "command": command,
        "args": args,
        "env": env.iter().map(|(k, v)| json!({"name": k, "value": v})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn absent_subagent_permissions_default_on_and_explicit_choices_survive() {
        let defaults: SubagentAccess = serde_json::from_value(json!({})).unwrap();
        assert!(defaults.on && defaults.edits);
        let explicit: SubagentAccess = serde_json::from_value(json!({"on":false,"edits":false})).unwrap();
        assert!(!explicit.on && !explicit.edits);
        assert_eq!(serde_json::from_value::<SubagentAccess>(serde_json::to_value(explicit).unwrap()).unwrap(), explicit);
    }

    #[test]
    fn the_menu_leaves_out_commands_m4code_does_itself() {
        assert!(super::Harness::Claude.hides_command("rename"));
        assert!(super::Harness::Claude.hides_command("autocompact"));
        assert!(!super::Harness::Claude.hides_command("review"));
        assert!(!super::Harness::Claude.hides_command("mcp"));
        assert!(super::Harness::Codex.hides_command("approvals"));
        assert!(!super::Harness::Claude.hides_command("compact"), "compacting stays");
    }

    use super::*;

    #[test]
    fn an_mcp_server_lists_its_environment_by_name() {
        let server = stdio_server("m4code", Path::new("/bin/m4code"), &["--mcp-title"], &[("M4CODE_RECORD", "r1".into())]);
        assert_eq!(server, json!({"name": "m4code", "command": "/bin/m4code", "args": ["--mcp-title"], "env": [{"name": "M4CODE_RECORD", "value": "r1"}]}));
    }

    #[test]
    fn the_store_names_harnesses_in_lowercase() {
        assert_eq!(serde_json::to_value(Harness::OpenCode).unwrap(), json!("opencode"));
        assert_eq!(serde_json::from_value::<Harness>(json!("claude")).unwrap(), Harness::Claude);
    }
}
