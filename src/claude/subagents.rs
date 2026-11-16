//! Custom subagents: agent types that run a task in another coding agent instead of Claude (Settings → Custom
//! subagents). Codex is the only one so far.
//!
//! Claudiu checks the settings here, once, and hands the result to the mod in `CLAUDIU_SUBAGENTS`. The mod registers
//! one agent type per entry and answers that agent's turns through the `claudiu --acp` helper (`claude/acp.rs`).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The settings the mod reads, as JSON. Absent when no custom subagent is on.
pub const ENV: &str = "CLAUDIU_SUBAGENTS";
/// The path of this executable, which the mod runs as `<path> --acp`.
pub const EXE_ENV: &str = "CLAUDIU_EXE";

/// Claude Code takes up to 64 characters in an agent type's name.
const TYPE_NAME_MAX: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomSubagents {
    #[serde(default)]
    pub codex: bool,
    /// The model names as typed, trimmed and without blanks, including any the check skips, so the page can still
    /// show them. Empty means one type on Codex's own default model.
    #[serde(default)]
    pub codex_models: Vec<String>,
    /// Codex may edit files and run commands outside its read-only sandbox.
    #[serde(default)]
    pub codex_edits: bool,
}

/// One Codex agent type: Claude Code's name for it (after `Claudiu:`) and the model Codex runs, `None` for Codex's
/// default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexType {
    pub name: String,
    pub model: Option<String>,
}

/// The model field split into names: on commas, each trimmed, blanks dropped.
pub fn parse_models(field: &str) -> Vec<String> {
    field.split(',').map(str::trim).filter(|n| !n.is_empty()).map(String::from).collect()
}

/// The agent types the model names give, and the names that give none: a name with characters other than letters,
/// digits, `.`, `_` and `-`, one too long for a type name, or one whose type name an earlier name already took.
/// Agent type names can't hold a dot, so dots become dashes (`gpt-5.5` → `codex-gpt-5-5`).
pub fn codex_types(models: &[String]) -> (Vec<CodexType>, Vec<String>) {
    if models.is_empty() {
        return (vec![CodexType { name: "codex".into(), model: None }], vec![]);
    }
    let mut types: Vec<CodexType> = vec![];
    let mut skipped = vec![];
    for model in models {
        let allowed = model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        let name = format!("codex-{}", model.replace('.', "-"));
        if !allowed || name.len() > TYPE_NAME_MAX || types.iter().any(|t| t.name == name) {
            skipped.push(model.clone());
        } else {
            types.push(CodexType { name, model: Some(model.clone()) });
        }
    }
    (types, skipped)
}

impl CustomSubagents {
    /// What the mod reads from `CLAUDIU_SUBAGENTS`; `None` when Codex is off.
    pub fn env(&self) -> Option<String> {
        if !self.codex {
            return None;
        }
        let (types, _) = codex_types(&self.codex_models);
        let types: Vec<Value> = types.iter().map(|t| json!({ "name": t.name, "model": t.model })).collect();
        Some(json!({ "codex": { "edits": self.codex_edits, "types": types } }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_field_splits_on_commas_and_drops_blanks() {
        assert_eq!(parse_models(" gpt-5.5 ,, gpt-6.1-sol,  "), names(&["gpt-5.5", "gpt-6.1-sol"]));
        assert_eq!(parse_models(""), Vec::<String>::new());
    }

    #[test]
    fn no_models_gives_one_type_on_codex_default() {
        let (types, skipped) = codex_types(&[]);
        assert_eq!(types, vec![CodexType { name: "codex".into(), model: None }]);
        assert!(skipped.is_empty());
    }

    #[test]
    fn dots_become_dashes_and_codex_gets_the_real_name() {
        let (types, skipped) = codex_types(&names(&["gpt-5.5", "gpt-6.1-sol"]));
        assert_eq!(
            types,
            vec![
                CodexType { name: "codex-gpt-5-5".into(), model: Some("gpt-5.5".into()) },
                CodexType { name: "codex-gpt-6-1-sol".into(), model: Some("gpt-6.1-sol".into()) },
            ]
        );
        assert!(skipped.is_empty());
    }

    #[test]
    fn bad_names_and_repeats_are_skipped() {
        let long = "g".repeat(59); // "codex-" plus 59 is 65 characters
        let (types, skipped) = codex_types(&names(&["gpt 5", "gpt-5.5", "gpt-5-5", "o3", &long]));
        assert_eq!(types.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["codex-gpt-5-5", "codex-o3"]);
        assert_eq!(skipped, names(&["gpt 5", "gpt-5-5", &long]));
    }

    #[test]
    fn the_mod_gets_nothing_while_codex_is_off() {
        let off = CustomSubagents { codex: false, codex_models: names(&["gpt-5.5"]), codex_edits: true };
        assert_eq!(off.env(), None);
    }

    #[test]
    fn the_mod_gets_the_checked_types_and_the_edit_switch() {
        let on = CustomSubagents { codex: true, codex_models: names(&["gpt-5.5", "bad name"]), codex_edits: false };
        let env: Value = serde_json::from_str(&on.env().unwrap()).unwrap();
        assert_eq!(env, json!({ "codex": { "edits": false, "types": [{ "name": "codex-gpt-5-5", "model": "gpt-5.5" }] } }));

        let default = CustomSubagents { codex: true, codex_models: vec![], codex_edits: true };
        let env: Value = serde_json::from_str(&default.env().unwrap()).unwrap();
        assert_eq!(env, json!({ "codex": { "edits": true, "types": [{ "name": "codex", "model": null }] } }));
    }
}
