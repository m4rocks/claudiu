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
    GitInstructions,
    ClaudeApiSkill,
    ExplorePlanAgents,
    AdvisorTool,
    BackgroundTasks,
}

/// What turning an item off does in the settings file.
enum Effect {
    /// Settings keys set to the given values.
    Keys(&'static [(&'static str, bool)]),
    /// Variables set in the `env` block, which Claude Code applies to the session.
    Env(&'static [(&'static str, &'static str)]),
    /// Tool names added to `permissions.deny`.
    Deny(&'static [&'static str]),
}

/// How much a trim saves; shown beside each switch (see `Trim::saving`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saving {
    High,
    Medium,
    Low,
}

impl Saving {
    pub fn label(self) -> &'static str {
        match self {
            Saving::High => "high savings",
            Saving::Medium => "medium savings",
            Saving::Low => "low savings",
        }
    }
}

/// What the setup window's "recommended trims" switch turns off: the high and medium savers that take nothing away
/// from how Claude works. The Advisor tool, Git instructions, background tasks and the Explore and Plan agents save
/// tokens too, but a new user has not chosen to lose them.
pub const RECOMMENDED: [Trim; 6] = [Trim::Artifacts, Trim::BundledSkills, Trim::ClaudeApiSkill, Trim::ClaudeAiConnectors, Trim::ScheduleWakeup, Trim::ReportFindings];

/// `trimmed` with the recommended trims added; what the user had turned off stays off.
pub fn with_recommended(trimmed: &[Trim]) -> Vec<Trim> {
    normalized(&[trimmed, &RECOMMENDED[..]].concat())
}

impl Trim {
    /// In the order the page lists them, grouped (see `GROUPS`).
    pub const ALL: [Trim; 20] = [
        Self::Artifacts,
        Self::BundledSkills,
        Self::ClaudeApiSkill,
        Self::Workflows,
        Self::RemoteControl,
        Self::ClaudeAiConnectors,
        Self::ExplorePlanAgents,
        Self::AdvisorTool,
        Self::BackgroundTasks,
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
        Self::GitInstructions,
    ];

    /// The page's groups: a heading and the items under it.
    pub const GROUPS: [(&'static str, &'static [Trim]); 6] = [
        ("FEATURES", &[Self::Artifacts, Self::BundledSkills, Self::ClaudeApiSkill, Self::Workflows, Self::RemoteControl, Self::ClaudeAiConnectors]),
        ("AGENTS AND TASKS", &[Self::ExplorePlanAgents, Self::AdvisorTool, Self::BackgroundTasks]),
        ("PLANNING", &[Self::PlanMode, Self::AskUserQuestion]),
        ("SCHEDULING", &[Self::ScheduledTasks, Self::ScheduleWakeup, Self::RemoteTrigger]),
        ("MESSAGING", &[Self::SendMessage, Self::PushNotification]),
        ("OTHER", &[Self::DesignSync, Self::NotebookEdit, Self::ReportFindings, Self::GitInstructions]),
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
            Self::GitInstructions => "Git instructions",
            Self::ClaudeApiSkill => "Claude API skill",
            Self::ExplorePlanAgents => "Explore and Plan agents",
            Self::AdvisorTool => "Advisor tool",
            Self::BackgroundTasks => "Background tasks",
        }
    }

    /// How many tokens turning it off saved per request in a measured run on Claude Code 2.1.319 (first request,
    /// alone against no trims): high is 1,300 to 13,400, medium 330 to 830, low under 100. Low ones are tools that load
    /// only when Claude searches for them. The Advisor tool is high only when an advisor model is set, and 0 otherwise.
    pub fn saving(self) -> Saving {
        match self {
            Self::Artifacts | Self::BundledSkills | Self::ClaudeAiConnectors | Self::ScheduleWakeup | Self::AdvisorTool => Saving::High,
            Self::GitInstructions | Self::ReportFindings | Self::ClaudeApiSkill | Self::BackgroundTasks | Self::ExplorePlanAgents => Saving::Medium,
            Self::Workflows
            | Self::RemoteControl
            | Self::PlanMode
            | Self::AskUserQuestion
            | Self::ScheduledTasks
            | Self::RemoteTrigger
            | Self::SendMessage
            | Self::PushNotification
            | Self::DesignSync
            | Self::NotebookEdit => Saving::Low,
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
            Self::GitInstructions => Effect::Keys(&[("includeGitInstructions", false)]),
            Self::ClaudeApiSkill => Effect::Env(&[("CLAUDE_CODE_DISABLE_CLAUDE_API_SKILL", "1")]),
            Self::ExplorePlanAgents => Effect::Env(&[("CLAUDE_CODE_DISABLE_EXPLORE_PLAN_AGENTS", "1")]),
            Self::AdvisorTool => Effect::Env(&[("CLAUDE_CODE_DISABLE_ADVISOR_TOOL", "1")]),
            Self::BackgroundTasks => Effect::Env(&[("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1")]),
        }
    }
}

/// `trimmed` in the page's order, without repeats: how it is stored, so equal sets compare equal.
pub fn normalized(trimmed: &[Trim]) -> Vec<Trim> {
    Trim::ALL.into_iter().filter(|t| trimmed.contains(t)).collect()
}

/// One line Claudiu adds to `settings.json`.
#[derive(Clone, PartialEq)]
enum Entry {
    Key(&'static str, bool),
    Deny(&'static str),
    Env(String, String),
    /// A skill turned off in `skillOverrides`.
    Skill(String),
}

impl Entry {
    /// How `applied` remembers it.
    fn id(&self) -> String {
        match self {
            Entry::Key(key, _) => key.to_string(),
            Entry::Deny(name) => format!("deny:{name}"),
            Entry::Env(name, value) => format!("env:{name}={value}"),
            Entry::Skill(name) => format!("skill:{name}"),
        }
    }

    /// The entry an earlier `id` stood for, so it can be taken out again. The value is part of an env id because the
    /// caps change between saves.
    fn from_id(id: &str) -> Option<Entry> {
        if let Some(rest) = id.strip_prefix("env:") {
            let (name, value) = rest.split_once('=')?;
            return Some(Entry::Env(name.into(), value.into()));
        }
        if let Some(name) = id.strip_prefix("skill:") {
            return Some(Entry::Skill(name.into()));
        }
        Trim::ALL.into_iter().flat_map(entries).find(|entry| entry.id() == id)
    }
}

fn entries(trim: Trim) -> Vec<Entry> {
    match trim.effect() {
        Effect::Keys(keys) => keys.iter().map(|(key, value)| Entry::Key(key, *value)).collect(),
        Effect::Deny(names) => names.iter().map(|name| Entry::Deny(name)).collect(),
        Effect::Env(vars) => vars.iter().map(|(name, value)| Entry::Env(name.to_string(), value.to_string())).collect(),
    }
}

fn present(settings: &Map<String, Value>, entry: &Entry) -> bool {
    match entry {
        Entry::Key(key, value) => settings.get(*key) == Some(&Value::Bool(*value)),
        Entry::Deny(name) => {
            let list = settings.get("permissions").and_then(|p| p.get("deny")).and_then(Value::as_array);
            list.is_some_and(|list| list.iter().any(|v| v.as_str() == Some(name)))
        }
        Entry::Env(name, value) => settings.get("env").and_then(|e| e.get(name)).and_then(Value::as_str) == Some(value),
        Entry::Skill(name) => settings.get("skillOverrides").and_then(|o| o.get(name)).and_then(Value::as_str) == Some("off"),
    }
}

/// Add `entry`. A value the user set differently for the same env variable or skill stays; `apply` notices that the
/// entry is not present afterwards and does not claim it.
fn add(settings: &mut Map<String, Value>, entry: &Entry) {
    match entry {
        Entry::Key(key, value) => {
            settings.insert((*key).into(), Value::Bool(*value));
        }
        Entry::Deny(name) => {
            let permissions = settings.entry("permissions").or_insert_with(|| json!({}));
            if let Some(permissions) = permissions.as_object_mut()
                && let Some(list) = permissions.entry("deny").or_insert_with(|| json!([])).as_array_mut()
            {
                list.push(Value::String((*name).into()));
            }
        }
        Entry::Env(name, value) => {
            let env = settings.entry("env").or_insert_with(|| json!({}));
            if let Some(env) = env.as_object_mut() {
                env.entry(name.clone()).or_insert_with(|| Value::String(value.clone()));
            }
        }
        Entry::Skill(name) => {
            let overrides = settings.entry("skillOverrides").or_insert_with(|| json!({}));
            if let Some(overrides) = overrides.as_object_mut() {
                overrides.entry(name.clone()).or_insert_with(|| Value::String("off".into()));
            }
        }
    }
}

fn remove(settings: &mut Map<String, Value>, entry: &Entry) {
    match entry {
        Entry::Key(key, value) => {
            if settings.get(*key) == Some(&Value::Bool(*value)) {
                settings.remove(*key);
            }
        }
        Entry::Deny(name) => {
            let Some(permissions) = settings.get_mut("permissions").and_then(Value::as_object_mut) else { return };
            if let Some(list) = permissions.get_mut("deny").and_then(Value::as_array_mut) {
                list.retain(|v| v.as_str() != Some(*name));
                if list.is_empty() {
                    permissions.remove("deny");
                }
            }
            if permissions.is_empty() {
                settings.remove("permissions");
            }
        }
        Entry::Env(name, value) => {
            let Some(env) = settings.get_mut("env").and_then(Value::as_object_mut) else { return };
            if env.get(name).and_then(Value::as_str) == Some(value) {
                env.remove(name);
            }
            if env.is_empty() {
                settings.remove("env");
            }
        }
        Entry::Skill(name) => {
            let Some(overrides) = settings.get_mut("skillOverrides").and_then(Value::as_object_mut) else { return };
            if overrides.get(name).and_then(Value::as_str) == Some("off") {
                overrides.remove(name);
            }
            if overrides.is_empty() {
                settings.remove("skillOverrides");
            }
        }
    }
}

/// The caps and skills the user set on the Claude Code tools page. Empty means Claude Code's own behavior.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Most characters of a Bash command's output that reach the model (`BASH_MAX_OUTPUT_LENGTH`).
    #[serde(default)]
    pub bash_output: Option<u32>,
    /// Most tokens a single file read returns (`CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS`).
    #[serde(default)]
    pub file_read_tokens: Option<u32>,
    /// Skills Claude no longer sees (`skillOverrides` set to `off`); the user can still type them.
    #[serde(default)]
    pub skills_off: Vec<String>,
}

impl Limits {
    /// The page's three text fields. Empty text leaves a cap unset. Skills are separated by commas or spaces.
    pub fn parse(bash_output: &str, file_read_tokens: &str, skills: &str) -> Result<Limits, String> {
        let number = |what: &str, text: &str| match text.trim() {
            "" => Ok(None),
            t => t.parse::<u32>().ok().filter(|n| *n > 0).map(Some).ok_or(format!("{what}: use a whole number above 0, or leave it empty.")),
        };
        let mut skills_off: Vec<String> = vec![];
        for name in skills.split([',', ' ', '\n']).map(str::trim).filter(|n| !n.is_empty()) {
            let valid = name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
            if !valid {
                return Err(format!("Skills: “{name}” is not a skill name (letters, digits, - _ : .)."));
            }
            if !skills_off.iter().any(|s| s == name) {
                skills_off.push(name.to_string());
            }
        }
        Ok(Limits { bash_output: number("Bash output", bash_output)?, file_read_tokens: number("File reads", file_read_tokens)?, skills_off })
    }

    /// The fields as text, for the page.
    pub fn texts(&self) -> (String, String, String) {
        let number = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        (number(self.bash_output), number(self.file_read_tokens), self.skills_off.join(", "))
    }

    fn entries(&self) -> Vec<Entry> {
        let env = |name: &str, n: Option<u32>| n.map(|n| Entry::Env(name.into(), n.to_string()));
        let caps = [env("BASH_MAX_OUTPUT_LENGTH", self.bash_output), env("CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS", self.file_read_tokens)];
        caps.into_iter().flatten().chain(self.skills_off.iter().map(|name| Entry::Skill(name.clone()))).collect()
    }
}

/// Put the trimmed items and limits into `settings` and take out the ones Claudiu added earlier (`applied`) that are
/// no longer wanted. A line the user had before Claudiu (a tool they denied themselves, a cap they set) is never
/// recorded, so turning the trim off leaves it. Returns what Claudiu now owns, to pass back next time.
pub fn apply(settings: &mut Map<String, Value>, trimmed: &[Trim], limits: &Limits, applied: &[String]) -> Vec<String> {
    let wanted: Vec<Entry> = normalized(trimmed).into_iter().flat_map(entries).chain(limits.entries()).collect();
    let wanted_ids: Vec<String> = wanted.iter().map(Entry::id).collect();
    for id in applied.iter().filter(|id| !wanted_ids.contains(id)) {
        if let Some(entry) = Entry::from_id(id) {
            remove(settings, &entry);
        }
    }
    let mut owned = vec![];
    for entry in wanted {
        let was_ours = applied.contains(&entry.id());
        if !present(settings, &entry) {
            add(settings, &entry);
            if present(settings, &entry) {
                owned.push(entry.id());
            }
        } else if was_ours {
            owned.push(entry.id());
        }
    }
    owned
}

/// `apply` to the `settings.json` at `path`, written only when something changed. A file that is not a JSON object is
/// never overwritten.
pub fn write_global(path: &std::path::Path, trimmed: &[Trim], limits: &Limits, applied: &[String]) -> std::io::Result<Vec<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}".into(),
        Err(e) => return Err(e),
    };
    let Ok(Value::Object(mut settings)) = serde_json::from_str(&text) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "settings.json is not a JSON object"));
    };
    let before = settings.clone();
    let owned = apply(&mut settings, trimmed, limits, applied);
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
        let owned = apply(&mut s, &[PlanMode, Workflows], &Limits::default(), &[]);
        assert_eq!(Value::Object(s), json!({ "model": "sonnet", "disableWorkflows": true, "permissions": { "deny": ["EnterPlanMode", "ExitPlanMode"] } }));
        assert_eq!(owned, ["disableWorkflows", "deny:EnterPlanMode", "deny:ExitPlanMode"]);
    }

    #[test]
    fn artifacts_are_turned_off_with_the_current_key_and_the_deprecated_one() {
        let mut s = Map::new();
        apply(&mut s, &[Artifacts], &Limits::default(), &[]);
        assert_eq!(Value::Object(s), json!({ "enableArtifact": false, "disableArtifact": true }));
    }

    #[test]
    fn turning_a_trim_off_takes_out_only_what_claudiu_added() {
        let mut s = object(json!({ "permissions": { "deny": ["NotebookEdit"], "defaultMode": "auto" } }));
        // The user already denied NotebookEdit; Claudiu adds the other two.
        let owned = apply(&mut s, &[NotebookEdit, ScheduledTasks, Workflows], &Limits::default(), &[]);
        assert_eq!(owned, ["disableWorkflows", "deny:CronCreate", "deny:CronDelete", "deny:CronList"], "the user's own deny is not recorded");
        let owned = apply(&mut s, &[], &Limits::default(), &owned);
        assert!(owned.is_empty());
        assert_eq!(Value::Object(s), json!({ "permissions": { "deny": ["NotebookEdit"], "defaultMode": "auto" } }));
    }

    #[test]
    fn an_empty_deny_list_and_permissions_object_are_cleaned_up() {
        let mut s = Map::new();
        let owned = apply(&mut s, &[SendMessage], &Limits::default(), &[]);
        assert!(s.contains_key("permissions"));
        apply(&mut s, &[], &Limits::default(), &owned);
        assert!(s.is_empty());
    }

    #[test]
    fn trimming_again_changes_nothing_and_keeps_ownership() {
        let mut s = Map::new();
        let owned = apply(&mut s, &[BundledSkills], &Limits::default(), &[]);
        let before = s.clone();
        assert_eq!(apply(&mut s, &[BundledSkills], &Limits::default(), &owned), owned);
        assert_eq!(s, before);
    }

    #[test]
    fn the_file_keeps_the_users_key_order_and_is_not_rewritten_when_nothing_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "{\n  \"zeta\": 1,\n  \"alpha\": 2\n}").unwrap();
        let owned = write_global(&path, &[Workflows], &Limits::default(), &[]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.find("zeta").unwrap() < text.find("alpha").unwrap(), "keys keep their order: {text}");
        assert!(text.contains("disableWorkflows"));
        let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
        write_global(&path, &[Workflows], &Limits::default(), &owned).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), stamp, "nothing to change, nothing written");
    }

    #[test]
    fn a_settings_file_that_is_not_json_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(write_global(&path, &[Workflows], &Limits::default(), &[]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn env_trims_go_in_the_env_block_beside_the_users_own_variables() {
        let mut s = object(json!({ "env": { "FOO": "1" } }));
        let owned = apply(&mut s, &[AdvisorTool, BackgroundTasks], &Limits::default(), &[]);
        assert_eq!(
            Value::Object(s.clone()),
            json!({ "env": { "FOO": "1", "CLAUDE_CODE_DISABLE_ADVISOR_TOOL": "1", "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS": "1" } })
        );
        apply(&mut s, &[], &Limits::default(), &owned);
        assert_eq!(Value::Object(s), json!({ "env": { "FOO": "1" } }));
    }

    #[test]
    fn an_env_block_that_claudiu_created_is_removed_when_it_ends_up_empty() {
        let mut s = Map::new();
        let owned = apply(&mut s, &[ExplorePlanAgents], &Limits::default(), &[]);
        apply(&mut s, &[], &Limits::default(), &owned);
        assert!(s.is_empty());
    }

    #[test]
    fn git_instructions_are_turned_off_with_a_false_value_that_is_removed_again() {
        let mut s = object(json!({ "model": "sonnet" }));
        let owned = apply(&mut s, &[GitInstructions], &Limits::default(), &[]);
        assert_eq!(Value::Object(s.clone()), json!({ "model": "sonnet", "includeGitInstructions": false }));
        apply(&mut s, &[], &Limits::default(), &owned);
        assert_eq!(Value::Object(s), json!({ "model": "sonnet" }));
    }

    fn limits(bash: Option<u32>, file: Option<u32>, skills: &[&str]) -> Limits {
        Limits { bash_output: bash, file_read_tokens: file, skills_off: skills.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn caps_and_skills_go_where_claude_code_reads_them() {
        let mut s = object(json!({ "skillOverrides": { "loop": "user-invocable-only" } }));
        apply(&mut s, &[], &limits(Some(2000), Some(5000), &["dataviz", "m4:do"]), &[]);
        assert_eq!(
            Value::Object(s),
            json!({
                "env": { "BASH_MAX_OUTPUT_LENGTH": "2000", "CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS": "5000" },
                "skillOverrides": { "loop": "user-invocable-only", "dataviz": "off", "m4:do": "off" }
            })
        );
    }

    #[test]
    fn changing_a_cap_replaces_the_old_value_and_clearing_it_removes_it() {
        let mut s = Map::new();
        let owned = apply(&mut s, &[], &limits(Some(2000), None, &[]), &[]);
        let owned = apply(&mut s, &[], &limits(Some(3000), None, &[]), &owned);
        assert_eq!(Value::Object(s.clone()), json!({ "env": { "BASH_MAX_OUTPUT_LENGTH": "3000" } }));
        apply(&mut s, &[], &Limits::default(), &owned);
        assert!(s.is_empty());
    }

    #[test]
    fn a_cap_or_skill_the_user_set_differently_is_kept_and_never_removed() {
        let mut s = object(json!({ "env": { "BASH_MAX_OUTPUT_LENGTH": "9000" }, "skillOverrides": { "dataviz": "user-invocable-only" } }));
        let owned = apply(&mut s, &[], &limits(Some(2000), None, &["dataviz"]), &[]);
        assert!(owned.is_empty(), "nothing was added, so nothing is claimed");
        apply(&mut s, &[], &Limits::default(), &owned);
        assert_eq!(Value::Object(s), json!({ "env": { "BASH_MAX_OUTPUT_LENGTH": "9000" }, "skillOverrides": { "dataviz": "user-invocable-only" } }));
    }

    #[test]
    fn turning_a_skill_back_on_removes_the_override_and_an_empty_block() {
        let mut s = Map::new();
        let owned = apply(&mut s, &[], &limits(None, None, &["dataviz"]), &[]);
        apply(&mut s, &[], &limits(None, None, &[]), &owned);
        assert!(s.is_empty());
    }

    #[test]
    fn the_limit_fields_are_parsed_with_empty_meaning_unset() {
        assert_eq!(Limits::parse("", " ", "").unwrap(), Limits::default());
        assert_eq!(Limits::parse("2000", "5000", "dataviz, m4:do  dataviz").unwrap(), limits(Some(2000), Some(5000), &["dataviz", "m4:do"]));
        assert!(Limits::parse("0", "", "").unwrap_err().starts_with("Bash output"));
        assert!(Limits::parse("", "ten", "").unwrap_err().starts_with("File reads"));
        assert!(Limits::parse("", "", "a/b").unwrap_err().starts_with("Skills"));
    }

    #[test]
    fn the_fields_show_the_saved_limits_as_text() {
        assert_eq!(limits(Some(2000), None, &["a", "b"]).texts(), ("2000".to_string(), String::new(), "a, b".to_string()));
    }

    #[test]
    fn each_switch_shows_the_saving_band_it_was_measured_in() {
        let band = |saving: Saving| -> Vec<Trim> { Trim::ALL.into_iter().filter(|t| t.saving() == saving).collect() };
        assert_eq!(band(Saving::High), [Artifacts, BundledSkills, ClaudeAiConnectors, AdvisorTool, ScheduleWakeup]);
        assert_eq!(band(Saving::Medium), [ClaudeApiSkill, ExplorePlanAgents, BackgroundTasks, ReportFindings, GitInstructions]);
        assert_eq!(
            band(Saving::Low),
            [Workflows, RemoteControl, PlanMode, AskUserQuestion, ScheduledTasks, RemoteTrigger, SendMessage, PushNotification, DesignSync, NotebookEdit]
        );
        assert_eq!(Saving::Medium.label(), "medium savings");
    }

    #[test]
    fn the_recommended_trims_are_added_to_what_the_user_already_turned_off() {
        assert_eq!(with_recommended(&[]), [Artifacts, BundledSkills, ClaudeApiSkill, ClaudeAiConnectors, ScheduleWakeup, ReportFindings]);
        assert_eq!(
            with_recommended(&[PlanMode, Artifacts]),
            [Artifacts, BundledSkills, ClaudeApiSkill, ClaudeAiConnectors, PlanMode, ScheduleWakeup, ReportFindings],
            "the user's own trim stays, and nothing is listed twice"
        );
    }

    #[test]
    fn every_item_is_listed_once_in_the_page_groups() {
        let listed: Vec<Trim> = Trim::GROUPS.iter().flat_map(|(_, items)| items.iter().copied()).collect();
        assert_eq!(listed, Trim::ALL);
    }
}
