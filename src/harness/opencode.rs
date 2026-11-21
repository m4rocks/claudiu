//! OpenCode, which speaks ACP itself (`opencode acp`), on the providers the user set up in OpenCode.

use crate::claude::subagents::Agent;

use super::{Adapter, Reopen, words};

pub const ADAPTER: Adapter = Adapter {
    name: "OpenCode",
    server: || super::subagent_server(Agent::OpenCode),
    reopen: Reopen::Load,
    resume_command: |id| Some(words(&["opencode", "--session", id])),
};
