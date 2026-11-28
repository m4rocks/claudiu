//! Google Antigravity, which speaks ACP itself (`agy acp`). It resumes a session without replaying it, as T3 Code
//! found, so an old Antigravity conversation opens empty and carries on from where it was.

pub mod limits;
pub mod read_only;

use super::subagents::Provider;
use crate::panels::logos;

#[cfg(feature = "adapter-antigravity")]
use super::subagents::Agent;

#[cfg(feature = "adapter-antigravity")]
use super::{Adapter, Reopen};
#[cfg(feature = "adapter-antigravity")]
use self::background::{background_update, task_finished};

/// Section 6 of Google's Antigravity terms (https://antigravity.google/terms) and Google's answers on the AI Developers
/// Forum: a client may host Antigravity while the user types the prompts and Antigravity keeps its own system
/// prompt, tools and history. So m4code doesn't seed it with another conversation, send it prompts of its own, or
/// move a conversation without the user asking.
#[cfg(feature = "adapter-antigravity")]
const CAPABILITIES: super::capabilities::Capabilities = {
    use super::capabilities::{Capability::{Allowed, Refused}, Handoff};
    const REPLAY: &str = "Google's Antigravity terms don't allow seeding a new Antigravity session with an earlier conversation.";
    super::capabilities::Capabilities {
        rewind: Refused(REPLAY),
        fork: Refused(REPLAY),
        continue_fresh: Allowed,
        compaction_fallback: Refused("Antigravity compacts only with its own /compact, and it doesn't list one in this session."),
        text_generation: Refused(TEXT_GENERATION_REFUSED),
        handoff_in: Handoff::Never("Google's Antigravity terms don't allow moving another model's conversation onto Antigravity."),
        handoff_out: Handoff::UserOnly("Only you can move an Antigravity conversation to another model."),
        subagent: Allowed,
        spawns_subagents: Allowed,
    }
};

#[cfg(feature = "adapter-antigravity")]
const TEXT_GENERATION_REFUSED: &str = "Google's Antigravity terms don't allow m4code's own prompts, such as titles, commit messages and summaries.";

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
    custom_path: false,
    probe_env: super::no_probe_env,
    config_root: |home| home.join(".gemini").join("config"),
    project_skills: ".agents/skills",
    // `/swarm` and `/teamwork` stay: they are how Google points multi-agent work on personal plans.
    hidden_commands: &["grill-me", "browser", "goal", "schedule", "learn", "boost"],
    worktree: super::WorktreeHooks::DEFAULT,
    protocol: super::ProtocolHooks {
        background: Some(super::BackgroundHooks { from_update: background_update, finished: task_finished, live_execute_tools: true }),
        ..super::ProtocolHooks::DEFAULT
    },
    sign_in: super::SignIn {
        program: "agy",
        locate: |_| super::find("agy"),
        args: &[],
        session_hint: "Run `agy` and sign in, then retry the session.",
        run_hint: "Antigravity isn't signed in. Run `agy` and sign in, then try again.",
    },
    // Google's terms allow running `/usage` when the user asks, not on a timer, so m4code reads it only on a refresh.
    usage: Some(super::UsageSource {
        read: |_| limits::read(&super::runner::locate(Agent::Antigravity).ok_or("agy was not found")?, &crate::datadir::data_dir()),
        every: None,
        after_turn: false,
        live: None,
    }),
    text_generation_launch: |_, _| Err(TEXT_GENERATION_REFUSED.into()),
    probe_session_meta: None,
    copies_skills: true,
    capabilities: CAPABILITIES,
    // Google points multi-agent work on personal plans to `/swarm` and `/teamwork`, so the instructions don't steer
    // Antigravity away from them.
    mcp_off: &[crate::mcp::PREFER_OWN_TOOLS],
    reports: None,
    native_sessions: None,
    // Text generation ran agy from this folder before Google's terms ruled it out.
    tidy: || {
        let _ = std::fs::remove_dir_all(crate::datadir::data_dir().join("antigravity-text-generation"));
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

/// How this agent runs as a custom subagent (`harness/subagents.rs`).
pub const PROVIDER: Provider = Provider {
    dashboard_url: Some("https://antigravity.google.com/remote?settingsOpen=true&settingsScreen=Models"),
    color: |t| t.antigravity,
    marks: logos::antigravity_marks,
    name: "Antigravity",
    key: "antigravity",
    task_name: "Google Antigravity",
    triggers: "Antigravity, Gemini",
    program: "agy",
    args: &["acp"],
    windows_fallback: Some("agy/bin/agy.exe"),
    server: "agy",
    missing: "agy was not found. Antigravity subagents need the Antigravity CLI installed.",
    // Google's FAQ names Claude Code among the third-party software barred from Antigravity logins, under Section 6 of
    // the Antigravity Additional Terms (https://antigravity.google/docs/faq, https://antigravity.google/terms).
    warning: Some("Google's Antigravity terms forbid using another company's agent, such as Claude Code, with a personal Google sign-in. That covers the Free, AI Plus, Pro and Ultra plans. Google can suspend or terminate the Antigravity and Gemini CLI accounts of anyone who does. Switch these on at your own risk."),
    cross_harness_default: false,
    task_args: super::runner::antigravity_args,
    environment: super::runner::no_env,
    config_options: super::runner::antigravity_options,
    inbox_without_steering: false,
    steering_prompt_required: false,
    deferred_tool_target: false,
    // agy gives a command about ten seconds, then moves it to the background and ends the turn.
    resumes_after_background: true,
};
