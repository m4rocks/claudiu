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
    /// Fix Git with Claude, which has to edit files and run git, so plan mode would stop it.
    FixGit,
    /// Another session, through `start_code_session`: it can't start a session that skips every prompt unless the
    /// user chose that there.
    OtherSession,
}

/// What a new `harness` session starts with: the mode id to switch to, and the config options to set by category.
/// `offered` is what the harness has of the four modes. Read-only is not for Fix Git, which has to edit, nor for
/// Claude Code while Plan mode is trimmed (`trim.rs`), since the model couldn't propose leaving it; another session
/// can't start one with full access. A default the session can't have falls to the nearest safer mode.
pub fn acp_settings(defaults: &NewSessionDefaults, harness: Harness, offered: &[Permission], plan_trimmed: bool, by: Starter) -> (Option<&'static str>, Vec<(&'static str, String)>) {
    let mut options = vec![];
    if let Some(model) = &defaults.model {
        options.push(("model", model.clone()));
    }
    if let Some(effort) = defaults.effort {
        options.push(("thought_level", effort.name().to_string()));
    }
    let allowed: Vec<Permission> = offered
        .iter()
        .copied()
        .filter(|p| match p {
            Permission::ReadOnly => by != Starter::FixGit && !(plan_trimmed && harness == Harness::Claude),
            Permission::FullAccess => by != Starter::OtherSession,
            _ => true,
        })
        .collect();
    let mode = defaults.permission_mode.and_then(|p| p.or_safer(&allowed)).and_then(|p| p.native(harness));
    (mode, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_set(mode: Permission) -> NewSessionDefaults {
        NewSessionDefaults { model: Some("opus".into()), effort: Some(Effort::High), permission_mode: Some(mode) }
    }

    fn mode(defaults: &NewSessionDefaults, harness: Harness, plan_trimmed: bool, by: Starter) -> Option<&'static str> {
        acp_settings(defaults, harness, &Permission::ALL, plan_trimmed, by).0
    }

    #[test]
    fn nothing_set_sets_nothing() {
        assert_eq!(acp_settings(&NewSessionDefaults::default(), Harness::Claude, &Permission::ALL, false, Starter::User), (None, vec![]));
    }

    #[test]
    fn each_harness_gets_its_own_mode_id_and_the_options_by_category() {
        let (mode, options) = acp_settings(&all_set(Permission::Ask), Harness::Claude, &Permission::ALL, false, Starter::User);
        assert_eq!(mode, Some("default"));
        assert_eq!(options, vec![("model", "opus".to_string()), ("thought_level", "high".to_string())]);
        let opencode = [Permission::ReadOnly, Permission::FullAccess];
        assert_eq!(acp_settings(&all_set(Permission::Ask), Harness::OpenCode, &opencode, false, Starter::User).0, Some("plan"), "no Ask in OpenCode: the safer read-only");
        assert_eq!(acp_settings(&all_set(Permission::Auto), Harness::Codex, &Permission::ALL, false, Starter::User).0, Some("agent"));
    }

    #[test]
    fn read_only_is_not_passed_while_plan_mode_is_trimmed_in_claude() {
        assert_eq!(mode(&all_set(Permission::ReadOnly), Harness::Claude, true, Starter::User), None);
        assert_eq!(mode(&all_set(Permission::ReadOnly), Harness::Codex, true, Starter::User), Some("read-only"));
        assert_eq!(mode(&all_set(Permission::Auto), Harness::Claude, true, Starter::User), Some("auto"));
    }

    #[test]
    fn fix_git_gets_no_read_only_and_other_sessions_get_no_full_access() {
        assert_eq!(mode(&all_set(Permission::ReadOnly), Harness::Claude, false, Starter::FixGit), None);
        assert_eq!(mode(&all_set(Permission::ReadOnly), Harness::Claude, false, Starter::OtherSession), Some("plan"));
        assert_eq!(mode(&all_set(Permission::FullAccess), Harness::Claude, false, Starter::OtherSession), Some("auto"), "steps down, never up");
        assert_eq!(mode(&all_set(Permission::FullAccess), Harness::Claude, false, Starter::User), Some("bypassPermissions"));
        assert_eq!(mode(&all_set(Permission::FullAccess), Harness::Claude, false, Starter::FixGit), Some("bypassPermissions"));
    }

    #[test]
    fn saved_defaults_load_into_the_four_modes() {
        let load = |json: &str| serde_json::from_str::<NewSessionDefaults>(json).unwrap().permission_mode;
        assert_eq!(serde_json::from_str::<NewSessionDefaults>("{}").unwrap(), NewSessionDefaults::default());
        assert_eq!(load(r#"{"permission_mode": "manual", "mode_id": "default"}"#), Some(Permission::Ask));
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
