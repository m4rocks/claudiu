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
    pub const GUARDS_OFF: &str = "guards_off";
    pub const ACCESS_REQUESTED: &str = "access_requested";
    pub const ACCESS_RESOLVED: &str = "access_resolved";
    pub const GUARD_REFUSED: &str = "guard_refused";
    pub const FIREWALL: &str = "firewall";
    pub const DELEGATION_STARTED: &str = "delegation_started";
    pub const DELEGATION_ENDED: &str = "delegation_ended";
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub cwd: String,
    pub title: Option<String>,
    pub adapter_session_id: Option<String>,
    pub created_ms: i64,
    /// The task that started this session as a subagent.
    pub parent: Option<SessionId>,
}

/// One reading of a session's context window, from the adapter's usage updates.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageRow {
    pub ts_ms: i64,
    pub used: Option<i64>,
    pub size: Option<i64>,
    pub raw: Value,
}

/// A write outside the project that the model asked for and the user has not answered yet.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessRequest {
    pub id: u64,
    pub path: std::path::PathBuf,
    pub reason: String,
}

/// The user's answer to an [`AccessRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Deny,
    /// Allowed for this task only.
    Task,
    /// Allowed for every task in this project.
    Project,
}
