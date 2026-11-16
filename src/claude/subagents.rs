//! Custom subagents: agent types that run a task in another coding agent instead of Claude (Settings → Custom
//! subagents). Codex is the only one so far.
//!
//! Claudiu defines the agent types itself with `claude --agents`, so they carry no plugin prefix, and hands the mod
//! the same list in `CLAUDIU_SUBAGENTS`. The mod answers each turn of those agents through the `claudiu --acp` helper
//! (`claude/acp.rs`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The agent types the mod answers for, as JSON. Absent when no custom subagent is on.
pub const ENV: &str = "CLAUDIU_SUBAGENTS";
/// The path of this executable, which the mod runs as `<path> --acp`.
pub const EXE_ENV: &str = "CLAUDIU_EXE";

/// Claude Code takes up to 64 characters in an agent type's name.
const TYPE_NAME_MAX: usize = 64;
/// The mod answers these agents' turns, so Claude runs for one only if the mod lost track of it (a reload, and the
/// agent gone from the agent list). It then says so rather than attempt the task.
const STUB_PROMPT: &str = "Reply with exactly this and nothing else: Codex couldn't run this task because Claudiu's mod lost track of this agent. Start a new Codex subagent.";

/// A model codex-acp offers: its id, which Codex is given, and the name its model picker shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexModel {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomSubagents {
    #[serde(default)]
    pub codex: bool,
    /// The ids of the models switched on, each one agent type.
    #[serde(default)]
    pub codex_models: Vec<String>,
    /// Codex may edit files and run commands outside its read-only sandbox.
    #[serde(default)]
    pub codex_edits: bool,
    /// The models codex-acp offered when the page last asked, shown while it asks again.
    #[serde(default)]
    pub codex_catalog: Vec<CodexModel>,
}

/// One Codex agent type: its name in Claude Code and the model Codex runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexType {
    pub name: String,
    pub model: String,
}

/// The agent type name of a model: `codex-` and the id, dots turned into dashes because type names can't hold a dot
/// (`gpt-6.1-sol` → `codex-gpt-6-1-sol`). None for an id no type name can carry.
pub fn type_name(model: &str) -> Option<String> {
    let allowed = !model.is_empty() && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    let name = format!("codex-{}", model.replace('.', "-"));
    (allowed && name.len() <= TYPE_NAME_MAX).then_some(name)
}

/// The agent types of the models switched on. A model whose type name an earlier one took gives none.
pub fn codex_types(models: &[String]) -> Vec<CodexType> {
    let mut types: Vec<CodexType> = vec![];
    for model in models {
        if let Some(name) = type_name(model)
            && !types.iter().any(|t| t.name == name)
        {
            types.push(CodexType { name, model: model.clone() });
        }
    }
    types
}

impl CustomSubagents {
    fn types(&self) -> Vec<CodexType> {
        if self.codex { codex_types(&self.codex_models) } else { vec![] }
    }

    /// What the mod reads from `CLAUDIU_SUBAGENTS`; `None` when no Codex type is on.
    pub fn env(&self) -> Option<String> {
        let types = self.types();
        if types.is_empty() {
            return None;
        }
        let types: Vec<Value> = types.iter().map(|t| json!({ "name": t.name, "model": t.model })).collect();
        Some(json!({ "codex": { "edits": self.codex_edits, "types": types } }).to_string())
    }

    /// The `claude --agents` definitions of the Codex types; `None` when none is on.
    pub fn agents(&self) -> Option<String> {
        let types = self.types();
        if types.is_empty() {
            return None;
        }
        let mut agents = Map::new();
        for t in types {
            let description = format!("Runs the task in OpenAI Codex on {0}. Use only when the user asks for Codex, GPT or {0}.", t.model);
            agents.insert(t.name, json!({ "description": description, "prompt": STUB_PROMPT, "model": "haiku", "tools": [] }));
        }
        Some(Value::Object(agents).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn on(models: &[&str], edits: bool) -> CustomSubagents {
        CustomSubagents { codex: true, codex_models: ids(models), codex_edits: edits, codex_catalog: vec![] }
    }

    #[test]
    fn dots_become_dashes_and_codex_gets_the_real_id() {
        assert_eq!(
            codex_types(&ids(&["gpt-6.1-sol", "gpt-5.6-luna"])),
            vec![
                CodexType { name: "codex-gpt-6-1-sol".into(), model: "gpt-6.1-sol".into() },
                CodexType { name: "codex-gpt-5-6-luna".into(), model: "gpt-5.6-luna".into() },
            ]
        );
    }

    #[test]
    fn ids_no_type_name_can_carry_and_repeats_give_no_type() {
        let long = "g".repeat(59); // "codex-" plus 59 is 65 characters
        let types = codex_types(&ids(&["gpt 5", "gpt-5.5", "gpt-5-5", "", &long, "o3"]));
        assert_eq!(types.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["codex-gpt-5-5", "codex-o3"]);
    }

    #[test]
    fn nothing_reaches_the_session_while_codex_is_off_or_no_model_is_on() {
        let mut off = on(&["gpt-6.1-sol"], true);
        off.codex = false;
        assert_eq!((off.env(), off.agents()), (None, None));
        let none = on(&[], false);
        assert_eq!((none.env(), none.agents()), (None, None));
    }

    #[test]
    fn the_mod_gets_the_types_and_the_edit_switch() {
        let env: Value = serde_json::from_str(&on(&["gpt-6.1-sol"], false).env().unwrap()).unwrap();
        assert_eq!(env, json!({ "codex": { "edits": false, "types": [{ "name": "codex-gpt-6-1-sol", "model": "gpt-6.1-sol" }] } }));
    }

    #[test]
    fn claude_gets_one_unprefixed_agent_per_model() {
        let agents: Value = serde_json::from_str(&on(&["gpt-6.1-sol", "gpt-5.6-luna"], false).agents().unwrap()).unwrap();
        let names: Vec<&String> = agents.as_object().unwrap().keys().collect();
        assert_eq!(names, ["codex-gpt-6-1-sol", "codex-gpt-5-6-luna"]);
        let sol = &agents["codex-gpt-6-1-sol"];
        assert_eq!(sol["description"], "Runs the task in OpenAI Codex on gpt-6.1-sol. Use only when the user asks for Codex, GPT or gpt-6.1-sol.");
        assert_eq!(sol["tools"], json!([]));
    }
}
