use super::*;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Data {
    pub version: u32,
    pub projects: Vec<Project>,
    pub sessions: Vec<SessionRecord>,
    /// Last Claude Code 5-hour and 7-day usage reading, shown at launch while the next reading runs.
    /// Serialized as `"account"` so older m4code versions can still load `state.json`.
    #[serde(default, rename = "account", alias = "claude_usage")]
    pub claude_usage: UsageLimits,
    /// The last limits reading per provider key, shown at launch while the next reading runs.
    #[serde(default)]
    pub subagent_limits: BTreeMap<String, Vec<crate::harness::limits::Pool>>,
    pub layout: Layout,
    pub settings: Settings,
}
