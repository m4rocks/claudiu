//! What new sessions start with (Settings → General): model, effort and permission mode, set through ACP once the
//! session opens. Empty means the harness's own default. Resumed sessions get none of this; their conversation
//! already has its own.

use serde::{Deserialize, Serialize};

use crate::harness::Harness;
use crate::harness::permission::Permission;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 5] = [Self::Low, Self::Medium, Self::High, Self::Xhigh, Self::Max];

    /// The value `--effort` takes.
    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Plain-English label shown in Settings and pickers.
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Xhigh => "Extra High",
            Self::Max => "Max",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "SavedDefaults")]
pub struct NewSessionDefaults {
    /// A model alias or id, as the harness lists it.
    pub model: Option<String>,
    pub effort: Option<Effort>,
    /// The same for every harness; each session gets its harness's mode for it (`harness::permission`).
    pub permission_mode: Option<Permission>,
}

/// `NewSessionDefaults` as state files have saved it. Before the four universal modes, a default chosen for another
/// harness than Claude Code was saved only as that harness's mode id, in `mode_id`.
#[derive(Deserialize)]
struct SavedDefaults {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<Effort>,
    #[serde(default)]
    permission_mode: Option<Permission>,
    #[serde(default)]
    mode_id: Option<String>,
}

impl From<SavedDefaults> for NewSessionDefaults {
    fn from(saved: SavedDefaults) -> Self {
        // `accept-edits` was Antigravity's, and `fullAccess` the id Settings showed before an adapter had answered.
        let from_id = |id: String| match id.as_str() {
            "default" | "workspace-write" | "ask" | "manual" | "dontAsk" => Some(Permission::ReadOnly),
            "accept-edits" => Some(Permission::Auto),
            "fullAccess" => Some(Permission::FullAccess),
            _ => Harness::ALL.into_iter().find_map(|h| Permission::of(h, &id)),
        };
        NewSessionDefaults { model: saved.model, effort: saved.effort, permission_mode: saved.permission_mode.or_else(|| saved.mode_id.and_then(from_id)) }
    }
}

/// Who is starting the session; some defaults are not for every caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Starter {
    /// The user: the New session button, or the GitHub pane's Ask Claude.
    User,
    Handoff,
    /// Fix Git with Claude, which has to edit files and run git, so plan mode would stop it.
    FixGit,
    /// Another session, through `start_code_session`: it can't start a session that skips every prompt unless the
    /// user chose that there.
    OtherSession,
}

/// Keep agent-started sessions from inheriting Full Access, including through Auto's fallback.
pub fn initial_permission(wanted: Permission, offered: &[Permission], by: Starter) -> Option<Permission> {
    if by == Starter::OtherSession && wanted != Permission::ReadOnly {
        return [Permission::Auto, Permission::ReadOnly].into_iter().find(|p| offered.contains(p));
    }
    wanted.resolve(offered)
}

pub fn config_options(defaults: &NewSessionDefaults) -> Vec<(&'static str, String)> {
    let mut options = vec![];
    if let Some(model) = &defaults.model {
        options.push(("model", model.clone()));
    }
    if let Some(effort) = defaults.effort {
        options.push(("thought_level", effort.name().to_string()));
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_set(mode: Permission) -> NewSessionDefaults {
        NewSessionDefaults { model: Some("opus".into()), effort: Some(Effort::High), permission_mode: Some(mode) }
    }

    #[test]
    fn options_are_set_by_category() {
        assert!(config_options(&NewSessionDefaults::default()).is_empty());
        assert_eq!(config_options(&all_set(Permission::Auto)), vec![("model", "opus".to_string()), ("thought_level", "high".to_string())]);
    }

    #[test]
    fn only_user_started_sessions_can_inherit_the_full_access_fallback() {
        let offered = [Permission::ReadOnly, Permission::FullAccess];
        assert_eq!(initial_permission(Permission::Auto, &offered, Starter::User), Some(Permission::FullAccess));
        assert_eq!(initial_permission(Permission::Auto, &offered, Starter::Handoff), Some(Permission::FullAccess));
        assert_eq!(initial_permission(Permission::Auto, &offered, Starter::OtherSession), Some(Permission::ReadOnly));
        assert_eq!(initial_permission(Permission::FullAccess, &Permission::ALL, Starter::OtherSession), Some(Permission::Auto));
    }

    #[test]
    fn saved_defaults_load_into_the_shared_modes() {
        let load = |json: &str| serde_json::from_str::<NewSessionDefaults>(json).unwrap().permission_mode;
        assert_eq!(serde_json::from_str::<NewSessionDefaults>("{}").unwrap(), NewSessionDefaults::default());
        assert_eq!(load(r#"{"permission_mode": "manual", "mode_id": "default"}"#), Some(Permission::ReadOnly));
        assert_eq!(load(r#"{"mode_id": "agent-full-access"}"#), Some(Permission::FullAccess), "a Codex default saved only by its id");
        assert_eq!(load(r#"{"mode_id": "accept-edits"}"#), Some(Permission::Auto));
        assert_eq!(load(r#"{"mode_id": "custom"}"#), None);
        let saved = serde_json::to_string(&all_set(Permission::Auto)).unwrap();
        assert_eq!(serde_json::from_str::<NewSessionDefaults>(&saved).unwrap(), all_set(Permission::Auto));
    }

    #[test]
    fn effort_names_match_what_is_saved() {
        for effort in Effort::ALL {
            assert_eq!(serde_json::to_string(&effort).unwrap(), format!("\"{}\"", effort.name()));
        }
    }
}
