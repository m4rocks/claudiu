//! OpenCode, which speaks ACP itself (`opencode acp`), on the providers the user set up in OpenCode.

use crate::claude::subagents::Agent;

use super::{Adapter, Reopen};

pub const ADAPTER: Adapter = Adapter {
    name: "OpenCode",
    server_name: "opencode",
    server: || super::subagent_server(Agent::OpenCode),
    reopen: Reopen::Load,
    delete_model_session: true,
};
