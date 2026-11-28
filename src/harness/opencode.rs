//! OpenCode, which speaks ACP itself (`opencode acp`), on the providers the user set up in OpenCode.

use super::subagents::Provider;
use super::subagents::Agent;

use super::{Adapter, Reopen};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

fn config_root(home: &Path) -> PathBuf {
    #[cfg(unix)]
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("opencode");
    }
    home.join(".config").join("opencode")
}

pub fn managed_compaction_config(inherited: Option<&str>, session: Option<&str>) -> Result<String, String> {
    let parse = |text: Option<&str>| -> Result<Map<String, Value>, String> {
        let text = text.filter(|t| !t.trim().is_empty()).unwrap_or("{}");
        serde_json::from_str(text).map_err(|e| format!("OpenCode inline config must be a JSON object: {e}"))
    };
    let mut config = parse(inherited)?;
    config.extend(parse(session)?);
    let compaction = config.entry("compaction").or_insert_with(|| json!({}));
    let options = compaction.as_object_mut().ok_or("OpenCode compaction config must be an object.")?;
    options.insert("auto".into(), json!(false));
    options.insert("background".into(), json!(false));
    // Disable OpenCode's built-in `question` tool (not supported over ACP) so the model uses m4code's `ask_user`.
    let permission = config.entry("permission").or_insert_with(|| json!({}));
    if let Some(perms) = permission.as_object_mut() {
        perms.insert("question".into(), json!("deny"));
    }
    Ok(Value::Object(config).to_string())
}
/// A text-generation run, with every permission denied.
fn text_generation_launch(launch: &mut super::acp::Launch, _: &Path) -> Result<Value, String> {
    let mut config = super::text_generation::inline_config("OPENCODE_CONFIG_CONTENT")?;
    config.insert("default_agent".into(), json!("build"));
    config.insert("permission".into(), json!({"*": "deny"}));
    config.insert("agent".into(), json!({"build": {"permission": {"*": "deny"}}}));
    launch.env.push(("OPENCODE_CONFIG_CONTENT".into(), json!(config).to_string()));
    Ok(Value::Null)
}


pub const ADAPTER: Adapter = Adapter {
    name: "OpenCode",
    server_name: "opencode",
    server: || super::subagent_server(Agent::OpenCode),
    reopen: Reopen::Load,
    delete_model_session: true,
    compact_command: Some("compact"),
    default_models: || super::default_model_entry(super::Harness::OpenCode),
    steers: false,
    prepare: prepare_launch,
    configure_mcp: super::stdio_mcp,
    launch_args: super::no_launch_args,
    modes: [Some("plan"), None, Some("build")],
    mode_aliases: &[],
    full_access_relaunch: false,
    native_compaction_managed: true,
    fallback_question: true,
    discover: |_| crate::platform::find_executable("opencode"),
    custom_path: false,
    probe_env: super::no_probe_env,
    config_root,
    project_skills: ".opencode/skills",
    hidden_commands: &["models", "connect", "share", "unshare", "sessions"],
    worktree: super::WorktreeHooks::DEFAULT,
    protocol: super::ProtocolHooks::DEFAULT,
    sign_in: super::SignIn {
        program: "opencode",
        locate: |_| super::find("opencode"),
        args: &["auth", "login"],
        session_hint: "Sign in with `opencode auth login`, then retry the session.",
        run_hint: "OpenCode isn't signed in. Run `opencode auth login`, then try again.",
    },
    usage: None,
    text_generation_launch,
    probe_session_meta: None,
    copies_skills: false,
    capabilities: super::capabilities::Capabilities::ALL,
    mcp_off: &[],
    reports: None,
    native_sessions: None,
    tidy: || {},
};

fn prepare_launch(input: &super::SessionInput<'_>, plan: &mut super::SessionPlan) -> Result<(), String> {
    let inherited = std::env::var("OPENCODE_CONFIG_CONTENT").ok();
    let child = input.child_edits.map(|edits| super::child_request(Agent::OpenCode, input, edits));
    let env = child.as_ref().map(super::runner::opencode_env).unwrap_or_default();
    let local = env.iter().find(|(key, _)| *key == "OPENCODE_CONFIG_CONTENT").map(|(_, value)| value.as_str());
    plan.set_env("OPENCODE_CONFIG_CONTENT", managed_compaction_config(inherited.as_deref(), local)?);
    Ok(())
}

/// How this agent runs as a custom subagent (`harness/subagents.rs`).
pub const PROVIDER: Provider = Provider {
    dashboard_url: Some("https://opencode.ai/console"),
    color: |t| t.foreground,
    marks: |name| (&[], Some(name.to_uppercase())),
    name: "OpenCode",
    key: "opencode",
    task_name: "OpenCode",
    triggers: "OpenCode",
    program: "opencode",
    args: &["acp"],
    windows_fallback: None,
    server: "opencode",
    missing: "opencode was not found on PATH. OpenCode subagents need OpenCode installed.",
    warning: None,
    cross_harness_default: true,
    task_args: super::runner::no_args,
    environment: super::runner::opencode_env,
    config_options: super::runner::no_options,
    inbox_without_steering: false,
    steering_prompt_required: false,
    deferred_tool_target: true,
    resumes_after_background: false,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_overrides_preserve_user_config_and_child_permissions() {
        let value: Value = serde_json::from_str(&managed_compaction_config(
            Some(r#"{"model":"provider/model","compaction":{"auto":true,"keep":{"tokens":24000}},"provider":{"local":{}}}"#),
            Some(r#"{"permission":{"*":"deny","read":"allow"}}"#),
        ).unwrap()).unwrap();
        assert_eq!(value["model"], "provider/model");
        assert_eq!(value["compaction"], json!({"auto":false,"background":false,"keep":{"tokens":24000}}));
        assert_eq!(value["permission"]["*"], "deny");
        assert_eq!(value["permission"]["question"], "deny");
        assert!(value["provider"]["local"].is_object());
        assert!(managed_compaction_config(Some("[]"), None).is_err());
        assert!(managed_compaction_config(Some(r#"{"compaction":false}"#), None).is_err());
    }
}
