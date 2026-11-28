use super::*;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Data {
    pub version: u32,
    pub projects: Vec<Project>,
    pub sessions: Vec<SessionRecord>,
    /// Last Claude Code 5-hour and 7-day usage reading, shown at launch while the next reading runs.
    /// Serialized as `"account"` so older m4code versions can still load `state.json`. Read it through `usage_pools`.
    #[serde(default, rename = "account", alias = "claude_usage")]
    pub claude_usage: UsageLimits,
    /// The last limits reading per provider key, shown at launch while the next reading runs.
    #[serde(default)]
    pub subagent_limits: BTreeMap<String, Vec<crate::harness::limits::Pool>>,
    pub layout: Layout,
    pub settings: Settings,
}

impl Data {
    /// The last limits reading for `harness`. The host agent's (Claude Code's) is saved as `account`, the key older
    /// versions read; every other agent's is saved under its key.
    pub fn usage_pools(&self, harness: crate::harness::Harness) -> Vec<crate::harness::limits::Pool> {
        match harness.agent() {
            None => vec![crate::harness::limits::Pool { name: harness.name().into(), usage: self.claude_usage.clone() }],
            Some(_) => self.subagent_limits.get(harness.key()).cloned().unwrap_or_default(),
        }
    }

    /// Merges `pools` into the stored reading for `harness`, keeping the newer reading of each window. Returns whether
    /// anything changed; the caller marks the store dirty.
    pub fn merge_usage_pools(&mut self, harness: crate::harness::Harness, pools: Vec<crate::harness::limits::Pool>) -> bool {
        use crate::harness::limits::{merge_pools, merge_usage};
        if harness.agent().is_none() {
            return pools.into_iter().next().is_some_and(|pool| merge_usage(&mut self.claude_usage, &pool.usage));
        }
        let current = self.usage_pools(harness);
        let merged = merge_pools(&current, pools);
        if merged == current {
            return false;
        }
        self.subagent_limits.insert(harness.key().into(), merged);
        true
    }
}
