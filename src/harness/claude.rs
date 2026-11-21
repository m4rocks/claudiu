//! Claude Code, through Anthropic's ACP adapter (`@agentclientprotocol/claude-agent-acp`). The adapter runs the
//! user's own `claude` binary (`CLAUDE_CODE_EXECUTABLE`), so their install, settings, plugins and sign-in apply.
//! Claudiu's mod, accounts and launch flags reach Claude Code through `_meta.claudeCode.options`.

use std::path::Path;

use serde_json::{Value, json};

use super::{Adapter, Reopen, words};

/// Pinned so an upstream adapter change reaches users only with a Claudiu release.
const PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@0.109.0";

/// Where the adapter finds the user's Claude Code. Without it, it would use the copy bundled with its own SDK.
pub const EXECUTABLE_ENV: &str = "CLAUDE_CODE_EXECUTABLE";

pub const ADAPTER: Adapter = Adapter {
    name: "Claude Code",
    server: || {
        let npx = super::find("npx").ok_or("npx was not found on PATH. Claude Code sessions need Node.js for their ACP adapter.")?;
        Ok((npx, words(&["-y", PACKAGE])))
    },
    reopen: Reopen::Load,
    resume_command: |id| Some(words(&["claude", "--resume", id])),
};

/// The adapter's process environment for a session of `claude`.
pub fn environment(claude: &Path) -> Vec<(String, String)> {
    vec![(EXECUTABLE_ENV.into(), claude.to_string_lossy().into_owned())]
}

/// Claude Code's part of `session/new`: the adapter passes `_meta.claudeCode.options` to Claude Code. `plugin` is
/// Claudiu's mod; `allowed` the tools it may use unasked; `extra` the flags given as `--name value` (None for a bare
/// flag).
pub fn claude_meta(plugin: Option<&Path>, allowed: &[String], extra: &[(&str, Option<String>)]) -> Value {
    let mut options = json!({});
    if let Some(dir) = plugin {
        options["plugins"] = json!([{ "type": "local", "path": dir }]);
    }
    if !allowed.is_empty() {
        options["allowedTools"] = json!(allowed);
    }
    if !extra.is_empty() {
        options["extraArgs"] = extra.iter().map(|(k, v)| (k.to_string(), v.clone().map_or(Value::Null, Value::String))).collect::<serde_json::Map<_, _>>().into();
    }
    json!({ "claudeCode": { "options": options } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn options_carry_the_mod_tools_and_flags() {
        let meta = claude_meta(Some(Path::new("/mod")), &["mcp__claudiu__show_visual".into()], &[("name", Some("Fix it".into())), ("chrome", None)]);
        let options = &meta["claudeCode"]["options"];
        assert_eq!(options["plugins"], json!([{"type": "local", "path": "/mod"}]));
        assert_eq!(options["allowedTools"], json!(["mcp__claudiu__show_visual"]));
        assert_eq!(options["extraArgs"], json!({"name": "Fix it", "chrome": null}));
        assert_eq!(claude_meta(None, &[], &[]), json!({"claudeCode": {"options": {}}}), "nothing given adds nothing");
    }

    #[test]
    fn the_adapter_runs_the_users_claude() {
        assert_eq!(environment(&PathBuf::from("/opt/claude")), vec![(EXECUTABLE_ENV.to_string(), "/opt/claude".to_string())]);
    }
}
