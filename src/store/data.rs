use super::*;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Data {
    pub version: u32,
    pub projects: Vec<Project>,
    pub sessions: Vec<SessionRecord>,
    /// Usage from before accounts existed; moved into the main account on load. Still written (empty after the
    /// move) because older Claudiu versions require the field and would set state.json aside without it.
    #[serde(default)]
    pub account: AccountUsage,
    /// The last limits reading per provider key, shown at launch while the next reading runs.
    #[serde(default)]
    pub subagent_limits: BTreeMap<String, Vec<crate::claude::limits::Pool>>,
    /// The main account first, then alts. Kept in `accounts.json`, not state.json: an older Claudiu still
    /// running would rewrite state.json without them.
    #[serde(skip)]
    pub accounts: Vec<Account>,
    /// Account new sessions start with; the main one when unset.
    #[serde(skip)]
    pub active_account: Option<Id>,
    pub layout: Layout,
    pub settings: Settings,
}

