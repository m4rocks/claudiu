//! Google Antigravity, which speaks ACP itself (`agy acp`). It resumes a session without replaying it, as T3 Code
//! found, so an old Antigravity conversation opens empty and carries on from where it was.

use crate::claude::subagents::Agent;

use super::{Adapter, Reopen};

pub const ADAPTER: Adapter = Adapter {
    name: "Antigravity",
    server_name: "agy",
    server: || super::subagent_server(Agent::Antigravity),
    reopen: Reopen::Resume,
    delete_model_session: false,
    compact_command: None,
    default_modes: super::standard_default_modes,
    default_models: || super::default_model_entry(super::Harness::Antigravity),
    // agy has no documented way to resume a session by id from its command line.
};
