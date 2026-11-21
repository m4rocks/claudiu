//! What new Claude sessions start with (Settings → Sessions): model, effort and permission mode, passed as
//! `claude` flags. Empty means Claude Code's own default. Resumed sessions get none of this; their conversation
//! already has its own.

use serde::{Deserialize, Serialize};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    AcceptEdits,
    Auto,
    BypassPermissions,
    Manual,
    DontAsk,
    Plan,
}

impl PermissionMode {
    pub const ALL: [PermissionMode; 6] = [Self::Manual, Self::AcceptEdits, Self::Auto, Self::DontAsk, Self::Plan, Self::BypassPermissions];

    /// The value `--permission-mode` takes.
    pub fn name(self) -> &'static str {
        match self {
            Self::AcceptEdits => "acceptEdits",
            Self::Auto => "auto",
            Self::BypassPermissions => "bypassPermissions",
            Self::Manual => "manual",
            Self::DontAsk => "dontAsk",
            Self::Plan => "plan",
        }
    }

    /// Plain-English label shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "Ask Before Editing",
            Self::AcceptEdits => "Accept Edits",
            Self::Auto => "Auto",
            Self::DontAsk => "Don't Ask",
            Self::Plan => "Plan Mode",
            Self::BypassPermissions => "Full Access",
        }
    }

    /// The ACP mode id for Claude Code.
    pub fn acp_id(self) -> &'static str {
        match self {
            Self::Manual => "default",
            other => other.name(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NewSessionDefaults {
    /// A model alias or id, as the harness lists it.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
    /// Native ACP mode id chosen for the active harness (e.g. Claude, Codex, OpenCode, or Antigravity).
    #[serde(default)]
    pub mode_id: Option<String>,
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

/// The flags to add to a new session: each default that is set, unless `leading` already carries that flag or the
/// starter doesn't get it. `plan_trimmed` is whether Plan mode is trimmed (`trim.rs`): the model couldn't propose
/// leaving plan mode then, so a `plan` default is not passed.
pub fn flags(defaults: &NewSessionDefaults, plan_trimmed: bool, leading: &[String], by: Starter) -> Vec<String> {
    let given = |flag: &str| leading.iter().any(|a| a == flag || a.strip_prefix(flag).is_some_and(|rest| rest.starts_with('=')));
    let mut out = Vec::new();
    if let Some(model) = defaults.model.as_deref().filter(|_| !given("--model")) {
        out.extend(["--model".to_string(), model.to_string()]);
    }
    if let Some(effort) = defaults.effort.filter(|_| !given("--effort")) {
        out.extend(["--effort".to_string(), effort.name().to_string()]);
    }
    let mode = defaults.permission_mode.filter(|mode| match mode {
        PermissionMode::Plan => !plan_trimmed && by != Starter::FixGit,
        PermissionMode::BypassPermissions => by != Starter::OtherSession,
        _ => true,
    });
    if let Some(mode) = mode.filter(|_| !given("--permission-mode")) {
        out.extend(["--permission-mode".to_string(), mode.name().to_string()]);
    }
    out
}

/// The same defaults for a session that opens through ACP: the permission mode's id, and the config options to set
/// by category. Manual is ACP's `default` mode.
pub fn acp_settings(defaults: &NewSessionDefaults, plan_trimmed: bool, by: Starter) -> (Option<String>, Vec<(&'static str, String)>) {
    let flags = flags(defaults, plan_trimmed, &[], by);
    let (mut mode, mut options) = (None, vec![]);
    for pair in flags.chunks(2) {
        let [flag, value] = pair else { continue };
        match flag.as_str() {
            "--model" => options.push(("model", value.clone())),
            "--effort" => options.push(("thought_level", value.clone())),
            "--permission-mode" => mode = Some(if value == PermissionMode::Manual.name() { "default".to_string() } else { value.clone() }),
            _ => {}
        }
    }
    (mode, options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_sessions_get_the_same_defaults_by_category() {
        let (mode, options) = acp_settings(&all_set(PermissionMode::Manual), false, Starter::User);
        assert_eq!(mode.as_deref(), Some("default"));
        assert_eq!(options, vec![("model", "opus".to_string()), ("thought_level", "high".to_string())]);
        let (mode, _) = acp_settings(&all_set(PermissionMode::Plan), false, Starter::FixGit);
        assert_eq!(mode, None, "the same rules as the flags");
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn all_set(mode: PermissionMode) -> NewSessionDefaults {
        NewSessionDefaults { model: Some("opus".into()), effort: Some(Effort::High), permission_mode: Some(mode), mode_id: None }
    }

    #[test]
    fn nothing_set_adds_nothing() {
        assert!(flags(&NewSessionDefaults::default(), false, &[], Starter::User).is_empty());
    }

    #[test]
    fn all_three_are_passed() {
        assert_eq!(
            flags(&all_set(PermissionMode::Plan), false, &[], Starter::User),
            strings(&["--model", "opus", "--effort", "high", "--permission-mode", "plan"])
        );
    }

    #[test]
    fn a_flag_the_caller_passes_wins() {
        let leading = strings(&["a prompt", "--model", "claude-sonnet-5-5", "--effort=medium"]);
        assert!(flags(&all_set(PermissionMode::Plan), false, &leading, Starter::FixGit).is_empty());
        assert_eq!(flags(&all_set(PermissionMode::Auto), false, &leading, Starter::User), strings(&["--permission-mode", "auto"]));
    }

    #[test]
    fn plan_is_not_passed_while_plan_mode_is_trimmed() {
        assert!(flags(&all_set(PermissionMode::Plan), true, &[], Starter::User).iter().all(|a| a != "--permission-mode"));
        assert!(flags(&all_set(PermissionMode::Auto), true, &[], Starter::User).contains(&"auto".to_string()));
    }

    #[test]
    fn fix_git_gets_no_plan_and_other_sessions_get_no_bypass() {
        assert!(!flags(&all_set(PermissionMode::Plan), false, &[], Starter::FixGit).contains(&"--permission-mode".to_string()));
        assert!(flags(&all_set(PermissionMode::Plan), false, &[], Starter::OtherSession).contains(&"plan".to_string()));
        assert!(!flags(&all_set(PermissionMode::BypassPermissions), false, &[], Starter::OtherSession).contains(&"--permission-mode".to_string()));
        assert!(flags(&all_set(PermissionMode::BypassPermissions), false, &[], Starter::User).contains(&"bypassPermissions".to_string()));
        assert!(flags(&all_set(PermissionMode::BypassPermissions), false, &[], Starter::FixGit).contains(&"bypassPermissions".to_string()));
    }

    #[test]
    fn names_match_what_the_cli_takes_and_what_is_saved() {
        for mode in PermissionMode::ALL {
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{}\"", mode.name()));
        }
        for effort in Effort::ALL {
            assert_eq!(serde_json::to_string(&effort).unwrap(), format!("\"{}\"", effort.name()));
        }
        let old_state: NewSessionDefaults = serde_json::from_str("{}").unwrap();
        assert_eq!(old_state, NewSessionDefaults::default());
    }
}
