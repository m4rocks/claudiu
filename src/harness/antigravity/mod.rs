//! Google Antigravity, which speaks ACP itself (`agy acp`). It resumes a session without replaying it, as T3 Code
//! found, so an old Antigravity conversation opens empty and carries on from where it was.

pub mod limits;
pub mod read_only;

#[cfg(feature = "adapter-antigravity")]
use super::subagents::Agent;

#[cfg(feature = "adapter-antigravity")]
use super::{Adapter, Reopen};
#[cfg(feature = "adapter-antigravity")]
use self::background::{background_update, task_finished};

#[cfg(feature = "adapter-antigravity")]
pub const ADAPTER: Adapter = Adapter {
    name: "Antigravity",
    server_name: "agy",
    server: || super::subagent_server(Agent::Antigravity),
    reopen: Reopen::Resume,
    delete_model_session: false,
    compact_command: Some("compact"),
    default_models: || super::default_model_entry(super::Harness::Antigravity),
    steers: false,
    prepare: |input, plan| {
        if let Some(edits) = input.child_edits {
            plan.args = super::runner::antigravity_args(&super::child_request(Agent::Antigravity, input, edits))?;
        }
        Ok(())
    },
    configure_mcp: super::stdio_mcp,
    launch_args: |_, full_access| {
        if full_access { vec!["--dangerously-skip-permissions".into()] } else { Vec::new() }
    },
    modes: [Some("plan"), Some("auto"), Some("default")],
    mode_aliases: &[("default", super::Permission::Auto), ("accept-edits", super::Permission::Auto)],
    full_access_relaunch: true,
    native_compaction_managed: false,
    fallback_question: false,
    discover: |_| super::runner::locate(Agent::Antigravity),
    probe_env: super::no_probe_env,
    config_root: |home| home.join(".gemini").join("config"),
    project_skills: ".agents/skills",
    hidden_commands: &["swarm", "teamwork", "grill-me", "browser", "goal", "schedule", "learn", "boost"],
    worktree: super::WorktreeHooks::DEFAULT,
    protocol: super::ProtocolHooks {
        background: Some(super::BackgroundHooks { from_update: background_update, finished: task_finished, live_execute_tools: true }),
        ..super::ProtocolHooks::DEFAULT
    },
};

#[cfg(feature = "adapter-antigravity")]
mod background {
    use super::super::{BackgroundTask, BackgroundUpdate, background_output_texts, background_task_label, tool_output_texts};
    use serde_json::Value;
    use std::path::PathBuf;
    const BG_TASK_PREFIX: &str = "Tool is running as a background task with task id:";
    const BG_TASK_LOG_PREFIX: &str = "Task logs are available at:";

    fn background_task_from_tool(call: &super::super::thread::ToolCall) -> Option<BackgroundTask> {
        let texts = tool_output_texts(call);
        let task_id = texts.iter().find_map(|text| {
            let rest = text.split_once(BG_TASK_PREFIX)?.1.trim_start();
            let id = rest.split_whitespace().next()?.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '.'));
            (!id.is_empty()).then(|| id.to_string())
        })?;
        let log_path = texts.iter().find_map(|text| {
            let rest = text.split_once(BG_TASK_LOG_PREFIX)?.1.trim_start();
            let raw = rest.lines().next()?.trim();
            let path = raw.strip_prefix("file://").unwrap_or(raw);
            (!path.is_empty()).then(|| PathBuf::from(path))
        });
        let label = background_task_label(call);
        Some(BackgroundTask { id: task_id, label, log_path })
    }

    pub(super) fn background_update(call: &super::super::thread::ToolCall, update: &Value) -> BackgroundUpdate {
        if update.get("rawOutput").is_none() && update.get("content").is_none() {
            return BackgroundUpdate::default();
        }
        let started = (call.kind == "execute").then(|| background_task_from_tool(call)).flatten();
        let output = background_output_texts(call, update);
        BackgroundUpdate { started, finished: Vec::new(), output }
    }

    pub(super) fn task_finished(text: &str, task_id: &str) -> bool {
        if text.contains("No background tasks are currently running")
            || text.contains("All your subagents and background tasks have been stopped")
        {
            return true;
        }
        text.lines().any(|line| {
            let Some((_, rest)) = line.split_once(task_id) else { return false };
            let status = rest.trim_start_matches(['"', '\'', ')', ']', ':', ' ']).to_ascii_lowercase();
            ["finished", "canceled", "cancelled", "killed", "exited"].iter()
                .any(|word| status.strip_prefix(word).is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '.', ':'])))
        })
    }

}
