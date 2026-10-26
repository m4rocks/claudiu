//! What new Claude sessions start with (Settings → Sessions): model, effort and permission mode, passed as
//! `claude` flags. Empty means Claude Code's own default. Resumed sessions get none of this; their conversation
//! already has its own.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
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
    pub const ALL: [PermissionMode; 6] = [Self::AcceptEdits, Self::Auto, Self::BypassPermissions, Self::Manual, Self::DontAsk, Self::Plan];

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
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NewSessionDefaults {
    /// A model alias or id, already checked with `usage::valid_model_name`.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
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

#[derive(Debug, PartialEq, Eq)]
pub struct InvalidModel;

/// A model name as `--model` takes it: trimmed and lowercased, as in the Advisor dialog. `None` for an empty field;
/// `Err` for anything that isn't an alias or id. "off" is refused because it means nothing here.
pub fn parse_model(text: &str) -> Result<Option<String>, InvalidModel> {
    let name = text.trim().to_lowercase();
    if name.is_empty() {
        return Ok(None);
    }
    if name == "off" || !super::usage::valid_model_name(&name) {
        return Err(InvalidModel);
    }
    Ok(Some(name))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn all_set(mode: PermissionMode) -> NewSessionDefaults {
        NewSessionDefaults { model: Some("opus".into()), effort: Some(Effort::High), permission_mode: Some(mode) }
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
    fn model_names_are_cleaned_and_off_is_refused() {
        assert_eq!(parse_model(" Opus "), Ok(Some("opus".into())));
        assert_eq!(parse_model("  "), Ok(None));
        assert_eq!(parse_model("off"), Err(InvalidModel));
        assert_eq!(parse_model("op us!"), Err(InvalidModel));
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
