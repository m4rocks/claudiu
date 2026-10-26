//! Claude Code features and tools the user trimmed (Settings → Claude Code tools) and a session's sidebar choices
//! (auto-compact, background compaction, advisor), applied per process through
//! `--settings <file>`; nothing in the user's own Claude Code config changes.
//!
//! A feature flag such as `disableWorkflows` removes the feature, its tools and Claude Code's instructions about it.
//! A bare tool name in `permissions.deny` removes that tool's definition (a scoped rule like `Bash(rm *)` would only
//! block calls). See https://www.aihero.dev/how-to-kill-the-bloat-in-claude-codes-system-prompt.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::store::Launch;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trim {
    BundledSkills,
    Workflows,
    RemoteControl,
    ClaudeAiConnectors,
    Artifacts,
    PlanMode,
    AskUserQuestion,
    ScheduledTasks,
    ScheduleWakeup,
    RemoteTrigger,
    SendMessage,
    PushNotification,
    DesignSync,
    NotebookEdit,
    ReportFindings,
}

/// What turning an item off does in the settings file.
enum Effect {
    /// Settings keys set to the given values.
    Keys(&'static [(&'static str, bool)]),
    /// Tool names added to `permissions.deny`.
    Deny(&'static [&'static str]),
}

impl Trim {
    /// In the order the page lists them, grouped (see `GROUPS`).
    pub const ALL: [Trim; 15] = [
        Self::BundledSkills,
        Self::Workflows,
        Self::RemoteControl,
        Self::ClaudeAiConnectors,
        Self::Artifacts,
        Self::PlanMode,
        Self::AskUserQuestion,
        Self::ScheduledTasks,
        Self::ScheduleWakeup,
        Self::RemoteTrigger,
        Self::SendMessage,
        Self::PushNotification,
        Self::DesignSync,
        Self::NotebookEdit,
        Self::ReportFindings,
    ];

    /// The page's groups: a heading and the items under it.
    pub const GROUPS: [(&'static str, &'static [Trim]); 5] = [
        ("FEATURES", &[Self::BundledSkills, Self::Workflows, Self::RemoteControl, Self::ClaudeAiConnectors, Self::Artifacts]),
        ("PLANNING", &[Self::PlanMode, Self::AskUserQuestion]),
        ("SCHEDULING", &[Self::ScheduledTasks, Self::ScheduleWakeup, Self::RemoteTrigger]),
        ("MESSAGING", &[Self::SendMessage, Self::PushNotification]),
        ("OTHER", &[Self::DesignSync, Self::NotebookEdit, Self::ReportFindings]),
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::BundledSkills => "Bundled skills",
            Self::Workflows => "Workflows",
            Self::RemoteControl => "Remote control",
            Self::ClaudeAiConnectors => "claude.ai connectors",
            Self::Artifacts => "Artifacts",
            Self::PlanMode => "Plan mode",
            Self::AskUserQuestion => "AskUserQuestion",
            Self::ScheduledTasks => "Scheduled tasks",
            Self::ScheduleWakeup => "ScheduleWakeup",
            Self::RemoteTrigger => "RemoteTrigger",
            Self::SendMessage => "SendMessage",
            Self::PushNotification => "PushNotification",
            Self::DesignSync => "DesignSync",
            Self::NotebookEdit => "NotebookEdit",
            Self::ReportFindings => "ReportFindings",
        }
    }

    fn effect(self) -> Effect {
        match self {
            Self::BundledSkills => Effect::Keys(&[("disableBundledSkills", true)]),
            Self::Workflows => Effect::Keys(&[("disableWorkflows", true)]),
            Self::RemoteControl => Effect::Keys(&[("disableRemoteControl", true)]),
            Self::ClaudeAiConnectors => Effect::Keys(&[("disableClaudeAiConnectors", true)]),
            Self::Artifacts => Effect::Keys(&[("enableArtifact", false), ("disableArtifact", true)]),
            Self::PlanMode => Effect::Deny(&["EnterPlanMode", "ExitPlanMode"]),
            Self::AskUserQuestion => Effect::Deny(&["AskUserQuestion"]),
            Self::ScheduledTasks => Effect::Deny(&["CronCreate", "CronDelete", "CronList"]),
            Self::ScheduleWakeup => Effect::Deny(&["ScheduleWakeup"]),
            Self::RemoteTrigger => Effect::Deny(&["RemoteTrigger"]),
            Self::SendMessage => Effect::Deny(&["SendMessage"]),
            Self::PushNotification => Effect::Deny(&["PushNotification"]),
            Self::DesignSync => Effect::Deny(&["DesignSync"]),
            Self::NotebookEdit => Effect::Deny(&["NotebookEdit"]),
            Self::ReportFindings => Effect::Deny(&["ReportFindings"]),
        }
    }
}

/// `trimmed` in the page's order, without repeats: how it is stored, so equal sets compare equal.
pub fn normalized(trimmed: &[Trim]) -> Vec<Trim> {
    Trim::ALL.into_iter().filter(|t| trimmed.contains(t)).collect()
}

/// The contents of the `--settings` file for these trims and the session's sidebar choices, or `None` when there
/// is neither. It holds only what was chosen: an item left alone has no key, so the user's own Claude Code settings
/// still decide it.
pub fn settings_json(trimmed: &[Trim], launch: &Launch) -> Option<String> {
    let mut settings = Map::new();
    if let Some(on) = launch.auto_compact {
        settings.insert("autoCompactEnabled".into(), Value::Bool(on));
    }
    if let Some(on) = launch.precompute_compact {
        settings.insert("precomputeCompactionEnabled".into(), Value::Bool(on));
    }
    if let Some(model) = &launch.advisor {
        settings.insert("advisorModel".into(), Value::String(model.clone()));
    }
    let mut deny: Vec<&str> = Vec::new();
    for t in normalized(trimmed) {
        match t.effect() {
            Effect::Keys(keys) => {
                for (key, value) in keys {
                    settings.insert((*key).into(), Value::Bool(*value));
                }
            }
            Effect::Deny(names) => deny.extend(names),
        }
    }
    if !deny.is_empty() {
        settings.insert("permissions".into(), json!({ "deny": deny }));
    }
    (!settings.is_empty()).then(|| Value::Object(settings).to_string())
}

pub fn settings_dir() -> PathBuf {
    crate::datadir::data_dir().join("settings")
}

/// Writes the settings file of a session and returns its path for `--settings`; `Ok(None)` when it would be empty.
pub fn prepare(claude_session_id: &str, trimmed: &[Trim], launch: &Launch) -> std::io::Result<Option<PathBuf>> {
    let Some(json) = settings_json(trimmed, launch) else { return Ok(None) };
    let path = settings_dir().join(format!("{}.json", crate::datadir::safe_file_stem(claude_session_id)));
    crate::datadir::write_atomic(&path, &json)?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::Trim::*;
    use super::*;

    #[test]
    fn nothing_trimmed_gives_no_settings() {
        assert_eq!(settings_json(&[], &Launch::default()), None);
    }

    #[test]
    fn a_flag_and_a_tool_group_give_the_flag_and_exactly_their_denies() {
        let v: Value = serde_json::from_str(&settings_json(&[PlanMode, Workflows], &Launch::default()).unwrap()).unwrap();
        assert_eq!(v, json!({ "disableWorkflows": true, "permissions": { "deny": ["EnterPlanMode", "ExitPlanMode"] } }));
    }

    #[test]
    fn artifacts_are_turned_off_with_the_current_key_and_the_deprecated_one() {
        let v: Value = serde_json::from_str(&settings_json(&[Artifacts], &Launch::default()).unwrap()).unwrap();
        assert_eq!(v, json!({ "enableArtifact": false, "disableArtifact": true }));
    }

    #[test]
    fn items_left_on_have_no_key() {
        let v: Value = serde_json::from_str(&settings_json(&[ScheduledTasks], &Launch::default()).unwrap()).unwrap();
        assert_eq!(v, json!({ "permissions": { "deny": ["CronCreate", "CronDelete", "CronList"] } }));
        let v: Value = serde_json::from_str(&settings_json(&[BundledSkills], &Launch::default()).unwrap()).unwrap();
        assert_eq!(v, json!({ "disableBundledSkills": true }), "no permissions key without a denied tool");
    }

    #[test]
    fn sidebar_choices_are_written_with_claude_codes_own_keys_and_only_when_set() {
        let launch = Launch { auto_compact: Some(false), precompute_compact: Some(true), advisor: Some("off".into()) };
        let v: Value = serde_json::from_str(&settings_json(&[], &launch).unwrap()).unwrap();
        assert_eq!(v, json!({ "autoCompactEnabled": false, "precomputeCompactionEnabled": true, "advisorModel": "off" }));
        let only_advisor = Launch { advisor: Some("sonnet".into()), ..Launch::default() };
        let v: Value = serde_json::from_str(&settings_json(&[PlanMode], &only_advisor).unwrap()).unwrap();
        assert_eq!(v, json!({ "advisorModel": "sonnet", "permissions": { "deny": ["EnterPlanMode", "ExitPlanMode"] } }));
    }

    #[test]
    fn order_and_repeats_do_not_matter() {
        assert_eq!(settings_json(&[NotebookEdit, PlanMode, PlanMode], &Launch::default()), settings_json(&[PlanMode, NotebookEdit], &Launch::default()));
        assert_eq!(normalized(&[ReportFindings, Workflows, Workflows]), [Workflows, ReportFindings]);
    }

    #[test]
    fn every_item_is_listed_once_in_the_page_groups() {
        let listed: Vec<Trim> = Trim::GROUPS.iter().flat_map(|(_, items)| items.iter().copied()).collect();
        assert_eq!(listed, Trim::ALL);
    }
}
