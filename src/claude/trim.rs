//! Claude Code features and tools the user trimmed (Settings → Claude Code tools). Claudiu writes the trims into
//! Claude Code's own `settings.json` and takes them out again when a switch goes back on; every account gets the
//! same file through `config_sync`, and Claude Code sessions started outside Claudiu see the trims too.
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

/// One line Claudiu adds to `settings.json` for a trim.
#[derive(Clone, Copy, PartialEq)]
enum Entry {
    Key(&'static str, bool),
    Deny(&'static str),
}

impl Entry {
    /// How `applied` remembers it.
    fn id(self) -> String {
        match self {
            Entry::Key(key, _) => key.to_string(),
            Entry::Deny(name) => format!("deny:{name}"),
        }
    }
}

fn entries(trim: Trim) -> Vec<Entry> {
    match trim.effect() {
        Effect::Keys(keys) => keys.iter().map(|(key, value)| Entry::Key(key, *value)).collect(),
        Effect::Deny(names) => names.iter().map(|name| Entry::Deny(name)).collect(),
    }
}

fn present(settings: &Map<String, Value>, entry: Entry) -> bool {
    match entry {
        Entry::Key(key, value) => settings.get(key) == Some(&Value::Bool(value)),
        Entry::Deny(name) => {
            let list = settings.get("permissions").and_then(|p| p.get("deny")).and_then(Value::as_array);
            list.is_some_and(|list| list.iter().any(|v| v.as_str() == Some(name)))
        }
    }
}

fn add(settings: &mut Map<String, Value>, entry: Entry) {
    match entry {
        Entry::Key(key, value) => {
            settings.insert(key.into(), Value::Bool(value));
        }
        Entry::Deny(name) => {
            let permissions = settings.entry("permissions").or_insert_with(|| json!({}));
            if let Some(permissions) = permissions.as_object_mut()
                && let Some(list) = permissions.entry("deny").or_insert_with(|| json!([])).as_array_mut()
            {
                list.push(Value::String(name.into()));
            }
        }
    }
}

fn remove(settings: &mut Map<String, Value>, entry: Entry) {
    match entry {
        Entry::Key(key, value) => {
            if settings.get(key) == Some(&Value::Bool(value)) {
                settings.remove(key);
            }
        }
        Entry::Deny(name) => {
            let Some(permissions) = settings.get_mut("permissions").and_then(Value::as_object_mut) else { return };
            if let Some(list) = permissions.get_mut("deny").and_then(Value::as_array_mut) {
                list.retain(|v| v.as_str() != Some(name));
                if list.is_empty() {
                    permissions.remove("deny");
                }
            }
            if permissions.is_empty() {
                settings.remove("permissions");
            }
        }
    }
}

/// Put the trimmed items into `settings` and take out the ones Claudiu added earlier (`applied`) that are no longer
/// trimmed. A line the user had before Claudiu (a tool they denied themselves) is never recorded, so turning the trim
/// off leaves it. Returns what Claudiu now owns, to pass back next time.
pub fn apply(settings: &mut Map<String, Value>, trimmed: &[Trim], applied: &[String]) -> Vec<String> {
    let wanted: Vec<Entry> = normalized(trimmed).into_iter().flat_map(entries).collect();
    let known: Vec<Entry> = Trim::ALL.into_iter().flat_map(entries).collect();
    for entry in known.iter().filter(|e| applied.contains(&e.id()) && !wanted.contains(e)) {
        remove(settings, *entry);
    }
    let mut owned = vec![];
    for entry in wanted {
        let was_ours = applied.contains(&entry.id());
        if !present(settings, entry) {
            add(settings, entry);
            owned.push(entry.id());
        } else if was_ours {
            owned.push(entry.id());
        }
    }
    owned
}

/// `apply` to the `settings.json` at `path`, written only when something changed. A file that is not a JSON object is
/// never overwritten.
pub fn write_global(path: &std::path::Path, trimmed: &[Trim], applied: &[String]) -> std::io::Result<Vec<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}".into(),
        Err(e) => return Err(e),
    };
    let Ok(Value::Object(mut settings)) = serde_json::from_str(&text) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "settings.json is not a JSON object"));
    };
    let before = settings.clone();
    let owned = apply(&mut settings, trimmed, applied);
    if settings != before {
        crate::datadir::write_atomic(path, &serde_json::to_string_pretty(&settings).map_err(std::io::Error::other)?)?;
    }
    Ok(owned)
}

/// Where earlier versions wrote a settings file per session; only pruned now.
pub fn settings_dir() -> PathBuf {
    crate::datadir::data_dir().join("settings")
}

#[cfg(test)]
mod tests {
    use super::Trim::*;
    use super::*;

    fn object(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn a_flag_and_a_tool_group_give_the_flag_and_exactly_their_denies() {
        let mut s = object(json!({ "model": "sonnet" }));
        let owned = apply(&mut s, &[PlanMode, Workflows], &[]);
        assert_eq!(Value::Object(s), json!({ "model": "sonnet", "disableWorkflows": true, "permissions": { "deny": ["EnterPlanMode", "ExitPlanMode"] } }));
        assert_eq!(owned, ["disableWorkflows", "deny:EnterPlanMode", "deny:ExitPlanMode"]);
    }

    #[test]
    fn artifacts_are_turned_off_with_the_current_key_and_the_deprecated_one() {
        let mut s = Map::new();
        apply(&mut s, &[Artifacts], &[]);
        assert_eq!(Value::Object(s), json!({ "enableArtifact": false, "disableArtifact": true }));
    }

    #[test]
    fn turning_a_trim_off_takes_out_only_what_claudiu_added() {
        let mut s = object(json!({ "permissions": { "deny": ["NotebookEdit"], "defaultMode": "auto" } }));
        // The user already denied NotebookEdit; Claudiu adds the other two.
        let owned = apply(&mut s, &[NotebookEdit, ScheduledTasks, Workflows], &[]);
        assert_eq!(owned, ["disableWorkflows", "deny:CronCreate", "deny:CronDelete", "deny:CronList"], "the user's own deny is not recorded");
        let owned = apply(&mut s, &[], &owned);
        assert!(owned.is_empty());
        assert_eq!(Value::Object(s), json!({ "permissions": { "deny": ["NotebookEdit"], "defaultMode": "auto" } }));
    }

    #[test]
    fn an_empty_deny_list_and_permissions_object_are_cleaned_up() {
        let mut s = Map::new();
        let owned = apply(&mut s, &[SendMessage], &[]);
        assert!(s.contains_key("permissions"));
        apply(&mut s, &[], &owned);
        assert!(s.is_empty());
    }

    #[test]
    fn trimming_again_changes_nothing_and_keeps_ownership() {
        let mut s = Map::new();
        let owned = apply(&mut s, &[BundledSkills], &[]);
        let before = s.clone();
        assert_eq!(apply(&mut s, &[BundledSkills], &owned), owned);
        assert_eq!(s, before);
    }

    #[test]
    fn the_file_keeps_the_users_key_order_and_is_not_rewritten_when_nothing_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "{\n  \"zeta\": 1,\n  \"alpha\": 2\n}").unwrap();
        let owned = write_global(&path, &[Workflows], &[]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.find("zeta").unwrap() < text.find("alpha").unwrap(), "keys keep their order: {text}");
        assert!(text.contains("disableWorkflows"));
        let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
        write_global(&path, &[Workflows], &owned).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), stamp, "nothing to change, nothing written");
    }

    #[test]
    fn a_settings_file_that_is_not_json_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(write_global(&path, &[Workflows], &[]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn every_item_is_listed_once_in_the_page_groups() {
        let listed: Vec<Trim> = Trim::GROUPS.iter().flat_map(|(_, items)| items.iter().copied()).collect();
        assert_eq!(listed, Trim::ALL);
    }
}
