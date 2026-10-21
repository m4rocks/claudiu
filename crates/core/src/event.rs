use std::fmt;

use serde_json::Value;

/// Claudiu's own id for a session (Task in the UI). It never changes, unlike the adapter's session id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> SessionId {
        SessionId(uuid::Uuid::new_v4().to_string())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One thing that happened in a session. Statuses, notifications and cost are all derived from these.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Grows with every event in the store, across sessions.
    pub id: i64,
    pub session: SessionId,
    pub ts_ms: i64,
    pub kind: String,
    pub payload: Value,
}

pub mod kind {
    pub const SESSION_STARTED: &str = "session_started";
    pub const USER_PROMPT: &str = "user_prompt";
    pub const AGENT_UPDATE: &str = "agent_update";
    pub const TOOL_CALL: &str = "tool_call";
    pub const AGENT_REQUEST: &str = "agent_request";
    pub const TURN_ENDED: &str = "turn_ended";
    pub const INTERRUPTED: &str = "interrupted";
    pub const ERROR: &str = "error";
    pub const ADAPTER_EXIT: &str = "adapter_exit";
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub cwd: String,
    pub title: Option<String>,
    pub adapter_session_id: Option<String>,
    pub created_ms: i64,
}

/// One reading of a session's context window, from the adapter's usage updates.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageRow {
    pub ts_ms: i64,
    pub used: Option<i64>,
    pub size: Option<i64>,
    pub raw: Value,
}
