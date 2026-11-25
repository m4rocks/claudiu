//! Google Antigravity, which speaks ACP itself (`agy acp`). It resumes a session without replaying it, as T3 Code
//! found, so an old Antigravity conversation opens empty and carries on from where it was.

pub mod limits;
pub mod read_only;

use super::subagents::Agent;

use super::{Adapter, Reopen};

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
    discover: |_| crate::platform::find_executable("agy"),
    probe_env: super::no_probe_env,
    config_root: |home| home.join(".gemini").join("config"),
    project_skills: ".agents/skills",
    hidden_commands: &["swarm"],
    worktree: super::WorktreeHooks::DEFAULT,
};
