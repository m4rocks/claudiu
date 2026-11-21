//! OpenAI Codex, through Zed's `codex-acp`, the same server Codex subagents use. It builds on Codex's own Rust core
//! rather than the `codex` binary, but reads the user's `~/.codex` config and `codex login`.

use crate::claude::subagents::Agent;

use super::{Adapter, Reopen, words};

pub const ADAPTER: Adapter = Adapter {
    name: "Codex",
    server: || super::subagent_server(Agent::Codex),
    reopen: Reopen::Load,
    resume_command: |id| Some(words(&["codex", "resume", id])),
};
