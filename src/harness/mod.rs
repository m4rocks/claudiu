//! The coding agents m4code runs sessions in, each through its own ACP server. The core of the app treats them
//! alike; what is particular to one (Claude Code's plugin, accounts and flags) is in its `Profile` here and in the
//! code that builds its launch. No GPUI here.

pub mod acp;
pub mod compaction;
mod antigravity;
pub mod claude;
pub(crate) mod codex;
pub(crate) mod opencode;
pub mod text_generation;
pub mod thread;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};


use crate::claude::subagents::Agent;

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
    /// Fallback permission modes shown before the adapter's ACP server reports its live list.
    pub default_modes: fn() -> Vec<(String, String)>,
    /// Fallback model entries shown before the adapter's ACP server reports its live catalog.
    pub default_models: fn() -> Vec<CatalogModel>,
}

/// Default permission modes for ACP adapters that haven't reported a live mode list yet.
pub(crate) fn standard_default_modes() -> Vec<(String, String)> {
    vec![("default".into(), "Ask Before Editing".into()), ("fullAccess".into(), "Full Access".into())]
}

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
    pub models: Vec<crate::claude::subagents::AgentModel>,
    pub modes: Vec<(String, String)>,
}

/// Ask `harness`'s ACP adapter for the models and permission modes it offers this account, using the same
/// `initialize` -> `session/new` handshake for every harness (Claude Code, Codex, OpenCode, and Antigravity).
pub fn probe(harness: Harness, cwd: &Path, env: &[(String, String)]) -> Result<HarnessCatalog, String> {
    let (program, args) = (harness.adapter().server)()?;
    crate::claude::acp::probe_server(harness, program, args, cwd, env)
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

    pub fn adapter(self) -> &'static Adapter {
        match self {
            Self::Claude => &claude::ADAPTER,
            Self::Codex => &codex::ADAPTER,
            Self::OpenCode => &opencode::ADAPTER,
            Self::Antigravity => &antigravity::ADAPTER,
        }
    }

    pub fn name(self) -> &'static str {
        self.adapter().name
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

    /// Slash commands the menu leaves out because m4code's own UI does their job: the model and permission pickers,
    /// New session, and Settings. Claude Code's /autocompact is the `/config` switch m4code manages.
    pub fn hidden_commands(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["autocompact", "model"],
            Self::Codex => &["model", "approvals", "permissions", "new", "status", "logout", "quit", "exit"],
            Self::OpenCode | Self::Antigravity => &[],
        }
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
    let program = crate::claude::acp::locate(agent).ok_or(provider.missing)?;
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
        assert!(super::Harness::Claude.hidden_commands().contains(&"autocompact"));
        assert!(super::Harness::Codex.hidden_commands().contains(&"approvals"));
        assert!(!super::Harness::Claude.hidden_commands().contains(&"compact"), "compacting stays");
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
