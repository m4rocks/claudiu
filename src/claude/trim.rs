//! Claude Code features and tools the user trimmed (Settings → Claude Code tools), applied per process through
//! `--settings <file>`; nothing in the user's own Claude Code config changes.
//!
//! A feature flag such as `disableWorkflows` removes the feature, its tools and Claude Code's instructions about it.
//! A bare tool name in `permissions.deny` removes that tool's definition (a scoped rule like `Bash(rm *)` would only
//! block calls). See https://www.aihero.dev/how-to-kill-the-bloat-in-claude-codes-system-prompt.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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
    /// A settings key set to `true`.
    Flag(&'static str),
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
            Self::BundledSkills => Effect::Flag("disableBundledSkills"),
            Self::Workflows => Effect::Flag("disableWorkflows"),
            Self::RemoteControl => Effect::Flag("disableRemoteControl"),
            Self::ClaudeAiConnectors => Effect::Flag("disableClaudeAiConnectors"),
            Self::Artifacts => Effect::Flag("disableArtifact"),
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

/// The contents of the `--settings` file for these trims, or `None` when nothing is trimmed. It holds only what
/// was trimmed: an item left on has no key, so the user's own Claude Code settings still decide it.
pub fn settings_json(trimmed: &[Trim]) -> Option<String> {
    let mut settings = Map::new();
    let mut deny: Vec<&str> = Vec::new();
    for t in normalized(trimmed) {
        match t.effect() {
            Effect::Flag(key) => {
                settings.insert(key.into(), Value::Bool(true));
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

/// Writes the settings file of a session and returns its path for `--settings`; `Ok(None)` when nothing is trimmed.
pub fn prepare(claude_session_id: &str, trimmed: &[Trim]) -> std::io::Result<Option<PathBuf>> {
    let Some(json) = settings_json(trimmed) else { return Ok(None) };
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
        assert_eq!(settings_json(&[]), None);
    }

    #[test]
    fn a_flag_and_a_tool_group_give_the_flag_and_exactly_their_denies() {
        let v: Value = serde_json::from_str(&settings_json(&[PlanMode, Workflows]).unwrap()).unwrap();
        assert_eq!(v, json!({ "disableWorkflows": true, "permissions": { "deny": ["EnterPlanMode", "ExitPlanMode"] } }));
    }

    #[test]
    fn items_left_on_have_no_key() {
        let v: Value = serde_json::from_str(&settings_json(&[ScheduledTasks]).unwrap()).unwrap();
        assert_eq!(v, json!({ "permissions": { "deny": ["CronCreate", "CronDelete", "CronList"] } }));
        let v: Value = serde_json::from_str(&settings_json(&[BundledSkills]).unwrap()).unwrap();
        assert_eq!(v, json!({ "disableBundledSkills": true }), "no permissions key without a denied tool");
    }

    #[test]
    fn order_and_repeats_do_not_matter() {
        assert_eq!(settings_json(&[NotebookEdit, PlanMode, PlanMode]), settings_json(&[PlanMode, NotebookEdit]));
        assert_eq!(normalized(&[ReportFindings, Workflows, Workflows]), [Workflows, ReportFindings]);
    }

    #[test]
    fn every_item_is_listed_once_in_the_page_groups() {
        let listed: Vec<Trim> = Trim::GROUPS.iter().flat_map(|(_, items)| items.iter().copied()).collect();
        assert_eq!(listed, Trim::ALL);
    }
}
