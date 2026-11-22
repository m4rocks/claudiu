//! Claude Code, through Anthropic's Claude Agent SDK. The SDK is a Node library, so m4code ships a small script
//! (`bridge/` in the repository) that drives it and speaks ACP like the other agents. The SDK runs the user's own
//! `claude` binary (`CLAUDE_CODE_EXECUTABLE`), so their install, settings, plugins and sign-in apply. m4code's mod,
//! tools and launch flags reach it through `_meta.claudeCode.options`.

use std::path::Path;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::{Adapter, Reopen, Server};

/// The bridge's files, embedded in the binary. The lockfile pins the SDK, so an SDK change reaches users only with a
/// m4code release.
const FILES: [(&str, &str); 3] = [
    ("claude.mjs", include_str!("../../bridge/claude.mjs")),
    ("package.json", include_str!("../../bridge/package.json")),
    ("package-lock.json", include_str!("../../bridge/package-lock.json")),
];

/// The SDK's own package, present once `npm ci` has finished in the bridge's folder.
const SDK: &str = "node_modules/@anthropic-ai/claude-agent-sdk/package.json";

/// Where the SDK finds the user's Claude Code. Without it, it would look for a copy of its own.
pub const EXECUTABLE_ENV: &str = "CLAUDE_CODE_EXECUTABLE";

pub const ADAPTER: Adapter = Adapter {
    name: "Claude Code",
    server_name: "claude-bridge",
    server: || server(&crate::datadir::data_dir()),
    reopen: Reopen::Load,
    delete_model_session: false,
    compact_command: Some("compact"),
    default_modes: || {
        crate::claude::defaults::PermissionMode::ALL
            .into_iter()
            .map(|m| (m.acp_id().to_string(), m.label().to_string()))
            .collect()
    },
    default_models: super::CatalogModel::default_claude_models,
};

/// Sessions starting together must not install the SDK into the same folder at once.
static INSTALLING: Mutex<()> = Mutex::new(());

/// The bridge, written to the data dir with the SDK installed beside it: `node claude.mjs`.
fn server(data: &Path) -> Result<Server, String> {
    let node = super::find("node").ok_or("Node.js was not found on PATH. Claude Code sessions need it for the Claude Agent SDK.")?;
    let dir = crate::claude::plugin::install_into(data, "claude-bridge", &FILES).map_err(|e| format!("Couldn't write the Claude Code bridge: {e}"))?;
    let _one_at_a_time = INSTALLING.lock().unwrap_or_else(|e| e.into_inner());
    if !dir.join(SDK).is_file() {
        install_sdk(&dir)?;
    }
    Ok((node, vec![dir.join("claude.mjs").to_string_lossy().into_owned()]))
}

/// `npm ci` from the lockfile, without the SDK's optional packages: those are its own copies of `claude` for each
/// platform, and the user's is used instead.
fn install_sdk(dir: &Path) -> Result<(), String> {
    let npm = super::find("npm").ok_or("npm was not found on PATH. Claude Code sessions need it once, to install the Claude Agent SDK.")?;
    let mut cmd = crate::claude::command(&npm);
    cmd.args(["ci", "--omit=optional", "--legacy-peer-deps", "--no-audit", "--no-fund"]).current_dir(dir);
    crate::platform::with_login_path(&mut cmd);
    let out = cmd.output().map_err(|e| format!("Couldn't run npm: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output");
        return Err(format!("Installing the Claude Agent SDK failed: {last}"));
    }
    Ok(())
}

/// The bridge's process environment for a session of `claude`.
pub fn environment(claude: &Path) -> Vec<(String, String)> {
    vec![(EXECUTABLE_ENV.into(), claude.to_string_lossy().into_owned())]
}

/// Claude Code's part of `session/new`: the bridge passes `_meta.claudeCode.options` to the SDK. `plugin` is
/// m4code's mod; `allowed` the tools it may use unasked; `extra` the flags given as `--name value` (None for a bare
/// flag).
/// `settings` is the session's own settings layer (the Agent SDK's `settings` option), such as its trims.
pub fn claude_meta(plugin: Option<&Path>, allowed: &[String], extra: &[(&str, Option<String>)], settings: Option<Value>) -> Value {
    let mut options = json!({});
    if let Some(dir) = plugin {
        options["plugins"] = json!([{ "type": "local", "path": dir }]);
    }
    if !allowed.is_empty() {
        options["allowedTools"] = json!(allowed);
    }
    if let Some(settings) = settings {
        options["settings"] = settings;
    }
    if !extra.is_empty() {
        options["extraArgs"] = extra.iter().map(|(k, v)| (k.to_string(), v.clone().map_or(Value::Null, Value::String))).collect::<serde_json::Map<_, _>>().into();
    }
    json!({ "claudeCode": { "options": options } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_carry_the_mod_tools_and_flags() {
        let meta = claude_meta(Some(Path::new("/mod")), &["mcp__m4code__show_visual".into()], &[("name", Some("Fix it".into())), ("chrome", None)], Some(json!({"disableWorkflows": true})));
        let options = &meta["claudeCode"]["options"];
        assert_eq!(options["plugins"], json!([{"type": "local", "path": "/mod"}]));
        assert_eq!(options["allowedTools"], json!(["mcp__m4code__show_visual"]));
        assert_eq!(options["extraArgs"], json!({"name": "Fix it", "chrome": null}));
        assert_eq!(options["settings"], json!({"disableWorkflows": true}));
        assert_eq!(claude_meta(None, &[], &[], None), json!({"claudeCode": {"options": {}}}), "nothing given adds nothing");
    }

    #[test]
    fn the_bridge_runs_the_users_claude() {
        assert_eq!(environment(Path::new("/opt/claude")), vec![(EXECUTABLE_ENV.to_string(), "/opt/claude".to_string())]);
    }
}
