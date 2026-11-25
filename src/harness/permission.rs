//! Shared permission choices and their ACP mode IDs.

use serde::{Deserialize, Serialize};

use super::Harness;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "read-only", alias = "plan", alias = "ask", alias = "manual", alias = "dontAsk")]
    ReadOnly,
    #[serde(rename = "auto", alias = "acceptEdits")]
    Auto,
    #[serde(rename = "full-access", alias = "bypassPermissions")]
    FullAccess,
}

impl Permission {
    pub const ALL: [Self; 3] = [Self::ReadOnly, Self::Auto, Self::FullAccess];

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read-only",
            Self::Auto => "Auto",
            Self::FullAccess => "Full Access",
        }
    }

    pub fn detail(self, available: bool) -> String {
        match (self, available) {
            (Self::Auto, false) => "Unavailable for this adapter",
            (Self::ReadOnly, _) => "Read and plan without editing",
            (Self::Auto, true) => "Let the adapter decide when to ask for approval",
            (Self::FullAccess, _) => "Approve commands and edits without asking",
        }.into()
    }

    pub fn native(self, harness: Harness) -> Option<&'static str> {
        harness.adapter().modes[match self {
            Self::ReadOnly => 0,
            Self::Auto => 1,
            Self::FullAccess => 2,
        }]
    }

    /// Existing sessions can be in an adapter's older approval policy.
    pub fn of(harness: Harness, id: &str) -> Option<Self> {
        if matches!(id, "read_only" | "readOnly") {
            return Some(Self::ReadOnly);
        }
        harness.adapter().mode_aliases.iter().find(|(native, _)| *native == id).map(|(_, mode)| *mode)
            .or_else(|| Self::ALL.into_iter().find(|p| p.native(harness) == Some(id)))
    }

    pub fn resolve(self, offered: &[Self]) -> Option<Self> {
        if offered.contains(&self) {
            Some(self)
        } else if self == Self::Auto && offered.contains(&Self::FullAccess) {
            Some(Self::FullAccess)
        } else {
            None
        }
    }

    pub fn listed(self, harness: Harness, modes: &[(String, String)]) -> Option<&'static str> {
        self.native(harness).filter(|id| modes.iter().any(|(mode, _)| mode == id))
    }
}

pub fn offered(harness: Harness, modes: &[(String, String)]) -> Vec<Permission> {
    Permission::ALL.into_iter().filter(|p| p.listed(harness, modes).is_some()).collect()
}

pub fn next(offered: &[Permission], current: Option<Permission>) -> Option<Permission> {
    let at = offered.iter().position(|p| Some(*p) == current).map_or(0, |i| i + 1);
    offered.get(at % offered.len().max(1)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(ids: &[&str]) -> Vec<(String, String)> {
        ids.iter().map(|id| (id.to_string(), id.to_string())).collect()
    }

    #[test]
    fn only_native_auto_modes_enable_auto() {
        let claude = modes(&["default", "acceptEdits", "auto", "plan", "dontAsk", "bypassPermissions"]);
        assert_eq!(offered(Harness::Claude, &claude), Permission::ALL);
        let without_auto = modes(&["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"]);
        assert_eq!(offered(Harness::Claude, &without_auto), [Permission::ReadOnly, Permission::FullAccess]);
        assert_eq!(offered(Harness::OpenCode, &modes(&["build", "plan"])), [Permission::ReadOnly, Permission::FullAccess]);
        assert_eq!(offered(Harness::Antigravity, &modes(&["default", "accept-edits", "auto", "plan"])), Permission::ALL);
        assert_eq!(Permission::of(Harness::Antigravity, "default"), Some(Permission::Auto));
        assert_eq!(Permission::of(Harness::Codex, "workspace-write"), Some(Permission::Auto));
    }

    #[test]
    fn only_auto_falls_back_to_full_access() {
        let offered = [Permission::ReadOnly, Permission::FullAccess];
        assert_eq!(Permission::Auto.resolve(&offered), Some(Permission::FullAccess));
        assert_eq!(Permission::Auto.resolve(&Permission::ALL), Some(Permission::Auto));
        assert_eq!(Permission::ReadOnly.resolve(&[Permission::FullAccess]), None);
        assert_eq!(Permission::FullAccess.resolve(&[Permission::ReadOnly]), None);
    }

    #[test]
    fn the_cycle_skips_unavailable_modes() {
        let offered = [Permission::ReadOnly, Permission::FullAccess];
        assert_eq!(next(&offered, Some(Permission::ReadOnly)), Some(Permission::FullAccess));
        assert_eq!(next(&offered, Some(Permission::FullAccess)), Some(Permission::ReadOnly));
        assert_eq!(next(&[], None), None);
    }

    #[test]
    fn previous_saved_permissions_still_load() {
        let load = |s: &str| serde_json::from_str::<Permission>(&format!("\"{s}\"")).unwrap();
        assert_eq!(load("manual"), Permission::ReadOnly);
        assert_eq!(load("dontAsk"), Permission::ReadOnly);
        assert_eq!(load("acceptEdits"), Permission::Auto);
        assert_eq!(load("plan"), Permission::ReadOnly);
        assert_eq!(load("bypassPermissions"), Permission::FullAccess);
        for p in Permission::ALL {
            assert_eq!(serde_json::from_str::<Permission>(&serde_json::to_string(&p).unwrap()).unwrap(), p);
        }
    }
}
