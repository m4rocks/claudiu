//! What new Claude sessions start with (Settings → Sessions): model, effort and permission mode, passed as
//! `claude` flags. Empty means Claude Code's own default. Resumed sessions get none of these flags; their conversation
//! already has its own. Auto effort is the exception: a session keeps the choice it was launched with, resumed or not
//! (`ClaudeMeta::auto_effort`), and gets its range through the environment (`auto_env`).

use serde::{Deserialize, Serialize};

// Declared from lowest to highest: `AutoEffort` clamps by this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

/// Auto effort: the mod has Haiku pick an effort for each prompt, within `lowest..=highest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoEffort {
    pub on: bool,
    pub lowest: Effort,
    pub highest: Effort,
}

impl Default for AutoEffort {
    fn default() -> Self {
        Self { on: false, lowest: Effort::Low, highest: Effort::High }
    }
}

impl AutoEffort {
    pub fn clamp(self, effort: Effort) -> Effort {
        effort.clamp(self.lowest, self.highest)
    }

    /// A new lowest level; the highest moves up to it when it was below.
    pub fn with_lowest(self, lowest: Effort) -> Self {
        Self { lowest, highest: self.highest.max(lowest), ..self }
    }

    /// A new highest level; the lowest moves down to it when it was above.
    pub fn with_highest(self, highest: Effort) -> Self {
        Self { highest, lowest: self.lowest.min(highest), ..self }
    }
}

/// The range the mod reads, e.g. `low-high`.
pub const AUTO_EFFORT_ENV: &str = "CLAUDIU_AUTO_EFFORT";
/// Claude Code sends effort per message with this capability, which keeps the prompt cache when effort changes
/// between prompts. Anthropic's own API only; elsewhere every change writes the whole conversation to the cache again.
pub const CAPABILITIES_ENV: &str = "CLAUDE_CODE_MODEL_CAPABILITIES";

/// What a session launched with auto effort gets in its environment. `capabilities_set`: the user already set
/// `CAPABILITIES_ENV`, which is then left as it is.
pub fn auto_env(auto: AutoEffort, capabilities_set: bool) -> Vec<(String, String)> {
    let mut env = vec![(AUTO_EFFORT_ENV.to_string(), format!("{}-{}", auto.lowest.name(), auto.highest.name()))];
    if !capabilities_set {
        env.push((CAPABILITIES_ENV.to_string(), "per_turn_effort".to_string()));
    }
    env
}

/// Whether a session started by `by` runs with auto effort. Fix Git keeps the fixed effort it passes.
pub fn auto_effort_for(defaults: &NewSessionDefaults, by: Starter) -> bool {
    defaults.auto_effort.on && by != Starter::FixGit
}

/// The effort a new session launches with: the default, held inside the auto range when `auto` is on.
pub fn launch_effort(defaults: &NewSessionDefaults, auto: bool) -> Option<Effort> {
    defaults.effort.map(|e| if auto { defaults.auto_effort.clamp(e) } else { e })
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
    /// Off, with a low–high range, in a state file from before this setting.
    #[serde(default)]
    pub auto_effort: AutoEffort,
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
        NewSessionDefaults { model: Some("opus".into()), effort: Some(Effort::High), permission_mode: Some(mode), auto_effort: AutoEffort::default() }
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
    fn auto_effort_is_off_low_to_high_in_an_old_state_file() {
        let old: NewSessionDefaults = serde_json::from_str(r#"{"model":"opus","effort":"max"}"#).unwrap();
        assert_eq!(old.auto_effort, AutoEffort { on: false, lowest: Effort::Low, highest: Effort::High });
        let saved = NewSessionDefaults { auto_effort: AutoEffort { on: true, lowest: Effort::Medium, highest: Effort::Max }, ..old };
        let back: NewSessionDefaults = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert_eq!(back, saved);
    }

    #[test]
    fn moving_one_end_of_the_range_past_the_other_pulls_it_along() {
        let range = AutoEffort { on: true, lowest: Effort::Low, highest: Effort::Medium };
        assert_eq!(range.with_lowest(Effort::High), AutoEffort { on: true, lowest: Effort::High, highest: Effort::High });
        assert_eq!(range.with_lowest(Effort::Medium), AutoEffort { on: true, lowest: Effort::Medium, highest: Effort::Medium });
        let range = AutoEffort { on: true, lowest: Effort::High, highest: Effort::Max };
        assert_eq!(range.with_highest(Effort::Medium), AutoEffort { on: true, lowest: Effort::Medium, highest: Effort::Medium });
        assert_eq!(range.with_highest(Effort::Xhigh), AutoEffort { on: true, lowest: Effort::High, highest: Effort::Xhigh });
    }

    #[test]
    fn with_auto_on_the_launch_effort_is_held_inside_the_range() {
        let mut d = all_set(PermissionMode::Auto);
        d.effort = Some(Effort::Max);
        d.auto_effort = AutoEffort { on: true, lowest: Effort::Low, highest: Effort::High };
        assert_eq!(launch_effort(&d, true), Some(Effort::High));
        assert_eq!(launch_effort(&d, false), Some(Effort::Max), "a session without auto launches with the default as set");
        d.effort = Some(Effort::Low);
        d.auto_effort.lowest = Effort::Medium;
        assert_eq!(launch_effort(&d, true), Some(Effort::Medium));
        d.effort = None;
        assert_eq!(launch_effort(&d, true), None, "Claude Code's own default stays");
    }

    #[test]
    fn every_starter_but_fix_git_gets_auto_effort_while_it_is_on() {
        let mut d = NewSessionDefaults::default();
        assert!(!auto_effort_for(&d, Starter::User));
        d.auto_effort.on = true;
        assert!(auto_effort_for(&d, Starter::User) && auto_effort_for(&d, Starter::OtherSession));
        assert!(!auto_effort_for(&d, Starter::FixGit));
    }

    #[test]
    fn the_environment_names_the_range_and_leaves_a_capability_the_user_set() {
        let auto = AutoEffort { on: true, lowest: Effort::Low, highest: Effort::Xhigh };
        let pairs = |v: Vec<(String, String)>| v.into_iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>();
        assert_eq!(pairs(auto_env(auto, false)), ["CLAUDIU_AUTO_EFFORT=low-xhigh", "CLAUDE_CODE_MODEL_CAPABILITIES=per_turn_effort"]);
        assert_eq!(pairs(auto_env(auto, true)), ["CLAUDIU_AUTO_EFFORT=low-xhigh"]);
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
