//! The coding agents Claudiu runs sessions in, each through its own ACP server. The core of the app treats them
//! alike; what is particular to one (Claude Code's plugin, accounts and flags) is in its `Profile` here and in the
//! code that builds its launch. No GPUI here.

pub mod acp;
mod antigravity;
pub mod claude;
mod codex;
mod opencode;
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

/// An ACP server's program and its arguments.
pub type Server = (PathBuf, Vec<String>);

/// How Claudiu drives one harness: the parts that differ between them. Each lives in its own file.
pub struct Adapter {
    pub name: &'static str,
    /// The ACP server's program and arguments, or why it can't start.
    pub server: fn() -> Result<Server, String>,
    /// How a stored session opens again.
    pub reopen: Reopen,
    /// The harness's own command line for a session, to carry on in a terminal. None when it has no way to resume
    /// one by id.
    pub resume_command: fn(&str) -> Option<Vec<String>>,
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
}

/// The server of a harness that shares its ACP server with a subagent provider.
fn subagent_server(agent: Agent) -> Result<Server, String> {
    let provider = agent.provider();
    let program = crate::claude::acp::locate(agent).ok_or(provider.missing)?;
    Ok((program, provider.args.iter().map(|s| s.to_string()).collect()))
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
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

/// The line to paste into a terminal to carry on with session `id` in `cwd`: change into the folder, then resume.
pub fn continue_command(harness: Harness, cwd: &Path, id: &str) -> Option<String> {
    let words = (harness.adapter().resume_command)(id)?;
    let line = words.iter().map(|w| shell_word(w)).collect::<Vec<_>>().join(" ");
    let cd = if cfg!(windows) { "cd /d" } else { "cd" };
    Some(format!("{cd} {} && {line}", shell_word(&cwd.to_string_lossy())))
}

/// A word as a shell reads it back unchanged: as is when it is plain, quoted otherwise.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain {
        word.to_string()
    } else if cfg!(windows) {
        format!("\"{word}\"")
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_harness_resumes_in_its_own_command() {
        let resume = |h: Harness| (h.adapter().resume_command)("abc");
        assert_eq!(resume(Harness::Claude), Some(words(&["claude", "--resume", "abc"])));
        assert_eq!(resume(Harness::Codex), Some(words(&["codex", "resume", "abc"])));
        assert_eq!(resume(Harness::OpenCode), Some(words(&["opencode", "--session", "abc"])));
        assert_eq!(resume(Harness::Antigravity), None);
    }

    #[test]
    #[cfg(unix)]
    fn carrying_on_in_a_terminal_changes_into_the_quoted_folder_first() {
        assert_eq!(continue_command(Harness::Claude, Path::new("/w/app"), "abc").as_deref(), Some("cd /w/app && claude --resume abc"));
        assert_eq!(continue_command(Harness::Codex, Path::new("/w/it's here"), "abc").as_deref(), Some("cd '/w/it'\\''s here' && codex resume abc"));
        assert_eq!(continue_command(Harness::Antigravity, Path::new("/w"), "abc"), None);
    }

    #[test]
    fn an_mcp_server_lists_its_environment_by_name() {
        let server = stdio_server("claudiu", Path::new("/bin/claudiu"), &["--mcp-title"], &[("CLAUDIU_RECORD", "r1".into())]);
        assert_eq!(server, json!({"name": "claudiu", "command": "/bin/claudiu", "args": ["--mcp-title"], "env": [{"name": "CLAUDIU_RECORD", "value": "r1"}]}));
    }

    #[test]
    fn the_store_names_harnesses_in_lowercase() {
        assert_eq!(serde_json::to_value(Harness::OpenCode).unwrap(), json!("opencode"));
        assert_eq!(serde_json::from_value::<Harness>(json!("claude")).unwrap(), Harness::Claude);
    }
}
