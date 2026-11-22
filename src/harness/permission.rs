//! The four permission modes m4code shows for every harness, and the mode id each harness's ACP adapter uses for
//! them. A harness that has no mode for one of them doesn't offer it; its other modes (Claude Code's `acceptEdits`
//! and `dontAsk`, Antigravity's `accept-edits`) stay out of the pickers.

use serde::{Deserialize, Serialize};

use super::Harness;

/// Ordered from the least access to the most; `or_safer` relies on that order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "read-only", alias = "plan")]
    ReadOnly,
    #[serde(rename = "ask", alias = "manual", alias = "dontAsk")]
    Ask,
    #[serde(rename = "auto", alias = "acceptEdits")]
    Auto,
    #[serde(rename = "full-access", alias = "bypassPermissions")]
    FullAccess,
}

impl Permission {
    pub const ALL: [Permission; 4] = [Self::ReadOnly, Self::Ask, Self::Auto, Self::FullAccess];

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read-only",
            Self::Ask => "Ask",
            Self::Auto => "Auto",
            Self::FullAccess => "Full Access",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read and plan; no edits or commands",
            Self::Ask => "Ask before running commands or editing files",
            Self::Auto => "Approve safe edits and commands automatically",
            Self::FullAccess => "Run all commands and edits without asking",
        }
    }

    /// The ACP mode id that sets this mode in `harness`, if it has one.
    pub fn native(self, harness: Harness) -> Option<&'static str> {
        match (harness, self) {
            (Harness::Claude, Self::ReadOnly) => Some("plan"),
            (Harness::Claude, Self::Ask) => Some("default"),
            (Harness::Claude, Self::Auto) => Some("auto"),
            (Harness::Claude, Self::FullAccess) => Some("bypassPermissions"),
            (Harness::Codex, Self::ReadOnly) => Some("read-only"),
            (Harness::Codex, Self::Ask) => Some("workspace-write"),
            (Harness::Codex, Self::Auto) => Some("agent"),
            (Harness::Codex, Self::FullAccess) => Some("agent-full-access"),
            (Harness::Antigravity, Self::ReadOnly) => Some("plan"),
            (Harness::Antigravity, Self::Ask) => Some("default"),
            (Harness::Antigravity, Self::Auto) => Some("auto"),
            (Harness::Antigravity, Self::FullAccess) => None,
            (Harness::OpenCode, Self::ReadOnly) => Some("plan"),
            (Harness::OpenCode, Self::Ask | Self::Auto) => None,
            (Harness::OpenCode, Self::FullAccess) => Some("build"),
        }
    }

    /// The mode `harness`'s mode id `id` stands for; None for a mode outside the four.
    pub fn of(harness: Harness, id: &str) -> Option<Permission> {
        Self::ALL.into_iter().find(|p| p.native(harness) == Some(id))
    }

    /// This mode when `offered` has it, or else the nearest one below it, so a session never gets more access than
    /// the user chose. None when nothing that safe is offered.
    pub fn or_safer(self, offered: &[Permission]) -> Option<Permission> {
        offered.iter().copied().filter(|p| *p <= self).max()
    }
}

/// The modes a `harness` session can switch to, given the `(id, name)` modes its adapter listed, least access first.
pub fn offered(harness: Harness, modes: &[(String, String)]) -> Vec<Permission> {
    Permission::ALL.into_iter().filter(|p| p.native(harness).is_some_and(|id| modes.iter().any(|(m, _)| m == id))).collect()
}

/// The mode id Shift+Tab moves to from `current`: the next offered mode, wrapping, or the first one when the session
/// is in a mode outside the four.
pub fn next(harness: Harness, modes: &[(String, String)], current: Option<&str>) -> Option<&'static str> {
    let offered = offered(harness, modes);
    let at = offered.iter().position(|p| p.native(harness) == current).map_or(0, |i| i + 1);
    offered.get(at % offered.len().max(1)).and_then(|p| p.native(harness))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(ids: &[&str]) -> Vec<(String, String)> {
        ids.iter().map(|id| (id.to_string(), id.to_string())).collect()
    }

    #[test]
    fn only_the_mapped_modes_a_harness_lists_are_offered() {
        let claude = modes(&["default", "acceptEdits", "auto", "plan", "dontAsk", "bypassPermissions"]);
        assert_eq!(offered(Harness::Claude, &claude), Permission::ALL.to_vec());
        assert_eq!(offered(Harness::OpenCode, &modes(&["build", "plan"])), vec![Permission::ReadOnly, Permission::FullAccess]);
        assert_eq!(offered(Harness::Antigravity, &modes(&["default", "accept-edits", "auto", "plan"])), vec![Permission::ReadOnly, Permission::Ask, Permission::Auto]);
        assert_eq!(Permission::of(Harness::Claude, "acceptEdits"), None);
        assert_eq!(Permission::of(Harness::Codex, "agent"), Some(Permission::Auto));
    }

    #[test]
    fn a_missing_mode_steps_down_never_up() {
        let opencode = [Permission::ReadOnly, Permission::FullAccess];
        assert_eq!(Permission::Ask.or_safer(&opencode), Some(Permission::ReadOnly));
        assert_eq!(Permission::FullAccess.or_safer(&opencode), Some(Permission::FullAccess));
        assert_eq!(Permission::FullAccess.or_safer(&[Permission::ReadOnly, Permission::Ask]), Some(Permission::Ask));
        assert_eq!(Permission::ReadOnly.or_safer(&[Permission::Ask]), None);
    }

    #[test]
    fn the_cycle_wraps_through_the_offered_modes() {
        let codex = modes(&["read-only", "workspace-write", "agent", "agent-full-access"]);
        assert_eq!(next(Harness::Codex, &codex, Some("agent")), Some("agent-full-access"));
        assert_eq!(next(Harness::Codex, &codex, Some("agent-full-access")), Some("read-only"), "after the last comes the first");
        let claude = modes(&["default", "acceptEdits", "plan"]);
        assert_eq!(next(Harness::Claude, &claude, Some("acceptEdits")), Some("plan"), "a mode outside the four goes to the first");
        assert_eq!(next(Harness::Claude, &[], None), None);
    }

    #[test]
    fn saved_names_from_before_the_four_modes_still_load() {
        let load = |s: &str| serde_json::from_str::<Permission>(&format!("\"{s}\"")).unwrap();
        assert_eq!(load("manual"), Permission::Ask);
        assert_eq!(load("dontAsk"), Permission::Ask);
        assert_eq!(load("acceptEdits"), Permission::Auto);
        assert_eq!(load("plan"), Permission::ReadOnly);
        assert_eq!(load("bypassPermissions"), Permission::FullAccess);
        for p in Permission::ALL {
            assert_eq!(serde_json::from_str::<Permission>(&serde_json::to_string(&p).unwrap()).unwrap(), p);
        }
    }
}
