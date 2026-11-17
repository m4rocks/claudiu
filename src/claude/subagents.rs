//! Custom subagents: agent types that run a task in another coding agent instead of Claude (Settings → Custom
//! subagents): OpenAI Codex and OpenCode.
//!
//! Claudiu lists the agent types in `CLAUDIU_SUBAGENTS`, and the mod registers them, so Claude Code names them
//! `Claudiu:<type>`. Interactive Claude Code ignores `claude --agents` (only `-p` reads it), so the mod is the one way
//! to define them per session. The mod answers each turn of those agents through the `claudiu --acp` helper
//! (`claude/acp.rs`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The agent types the mod answers for, as JSON. Absent when no custom subagent is on.
pub const ENV: &str = "CLAUDIU_SUBAGENTS";
/// The path of this executable, which the mod runs as `<path> --acp`.
pub const EXE_ENV: &str = "CLAUDIU_EXE";

/// Claude Code takes up to 64 characters in an agent type's name, before the plugin's `Claudiu:`.
const TYPE_NAME_MAX: usize = 64;

/// A coding agent a custom subagent runs the task in. Its serde name is the key the mod reads in `CLAUDIU_SUBAGENTS`
/// and the prefix of its agent types.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Codex,
    OpenCode,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::Codex, Agent::OpenCode];

    /// The name the settings page and the subagent's text use.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::OpenCode => "OpenCode",
        }
    }

    /// The key in `CLAUDIU_SUBAGENTS` and the start of every agent type name.
    pub fn key(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
        }
    }

    /// What Claude reads to decide when to use an agent type of this agent.
    fn description(self, model: &str) -> String {
        match self {
            Agent::Codex => format!("Runs the task in OpenAI Codex on {model}. Use only when the user asks for Codex, GPT or {model}."),
            Agent::OpenCode => format!("Runs the task in OpenCode on {model}. Use only when the user asks for OpenCode or {model}."),
        }
    }

    /// The mod answers these agents' turns, so Claude runs for one only if the mod lost track of it (a reload, and
    /// the agent gone from the agent list). It then says so rather than attempt the task.
    fn stub_prompt(self) -> String {
        let name = self.name();
        format!("Reply with exactly this and nothing else: {name} couldn't run this task because Claudiu's mod lost track of this agent. Start a new {name} subagent.")
    }
}

/// A model an agent offers: its id, which the agent is given, and the name its model picker shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModel {
    pub id: String,
    pub name: String,
}

/// What the settings page holds for one agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentChoice {
    pub on: bool,
    /// The ids of the models switched on, each one agent type.
    pub models: Vec<String>,
    /// The agent may edit files and run commands; otherwise it only reads.
    pub edits: bool,
    /// The models the agent offered when the page last asked, shown while it asks again.
    pub catalog: Vec<AgentModel>,
}

/// Saved as `Stored`, the flat shape state files already hold for Codex: a field that changed type would make the
/// whole state file fail to load, and the store moves such a file aside.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Stored", into = "Stored")]
pub struct CustomSubagents {
    pub codex: AgentChoice,
    pub opencode: AgentChoice,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    codex: bool,
    #[serde(default)]
    codex_models: Vec<String>,
    #[serde(default)]
    codex_edits: bool,
    #[serde(default)]
    codex_catalog: Vec<AgentModel>,
    #[serde(default)]
    opencode: bool,
    #[serde(default)]
    opencode_models: Vec<String>,
    #[serde(default)]
    opencode_edits: bool,
    #[serde(default)]
    opencode_catalog: Vec<AgentModel>,
}

impl From<Stored> for CustomSubagents {
    fn from(s: Stored) -> Self {
        CustomSubagents {
            codex: AgentChoice { on: s.codex, models: s.codex_models, edits: s.codex_edits, catalog: s.codex_catalog },
            opencode: AgentChoice { on: s.opencode, models: s.opencode_models, edits: s.opencode_edits, catalog: s.opencode_catalog },
        }
    }
}

impl From<CustomSubagents> for Stored {
    fn from(c: CustomSubagents) -> Self {
        Stored {
            codex: c.codex.on,
            codex_models: c.codex.models,
            codex_edits: c.codex.edits,
            codex_catalog: c.codex.catalog,
            opencode: c.opencode.on,
            opencode_models: c.opencode.models,
            opencode_edits: c.opencode.edits,
            opencode_catalog: c.opencode.catalog,
        }
    }
}

/// One agent type: its name in Claude Code and the model the agent runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentType {
    pub name: String,
    pub model: String,
}

/// The agent type name of a model: the agent's key and the id, dots and slashes turned into dashes because type names
/// can hold neither (`gpt-6.1-sol` → `codex-gpt-6-1-sol`, `openai/gpt-6.1-sol` → `opencode-openai-gpt-6-1-sol`).
/// None for an id no type name can carry.
pub fn type_name(agent: Agent, model: &str) -> Option<String> {
    let allowed = !model.is_empty() && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    let name = format!("{}-{}", agent.key(), model.replace(['.', '/'], "-"));
    (allowed && name.len() <= TYPE_NAME_MAX).then_some(name)
}

/// The agent types of the models switched on. A model whose type name an earlier one took gives none.
pub fn agent_types(agent: Agent, models: &[String]) -> Vec<AgentType> {
    let mut types: Vec<AgentType> = vec![];
    for model in models {
        if let Some(name) = type_name(agent, model)
            && !types.iter().any(|t| t.name == name)
        {
            types.push(AgentType { name, model: model.clone() });
        }
    }
    types
}

impl CustomSubagents {
    pub fn get(&self, agent: Agent) -> &AgentChoice {
        match agent {
            Agent::Codex => &self.codex,
            Agent::OpenCode => &self.opencode,
        }
    }

    pub fn get_mut(&mut self, agent: Agent) -> &mut AgentChoice {
        match agent {
            Agent::Codex => &mut self.codex,
            Agent::OpenCode => &mut self.opencode,
        }
    }

    /// Each agent switched on that has a type, with its types.
    fn types(&self) -> Vec<(Agent, Vec<AgentType>)> {
        Agent::ALL
            .into_iter()
            .filter(|a| self.get(*a).on)
            .map(|a| (a, agent_types(a, &self.get(a).models)))
            .filter(|(_, types)| !types.is_empty())
            .collect()
    }

    /// What the mod reads from `CLAUDIU_SUBAGENTS`, each type as the mod registers it; `None` when no type is on.
    pub fn env(&self) -> Option<String> {
        let types = self.types();
        if types.is_empty() {
            return None;
        }
        let mut env = Map::new();
        for (agent, types) in types {
            let types: Vec<Value> = types
                .iter()
                .map(|t| json!({ "name": t.name, "model": t.model, "description": agent.description(&t.model), "prompt": agent.stub_prompt() }))
                .collect();
            env.insert(agent.key().into(), json!({ "edits": self.get(agent).edits, "types": types }));
        }
        Some(Value::Object(env).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn on(models: &[&str], edits: bool) -> AgentChoice {
        AgentChoice { on: true, models: ids(models), edits, catalog: vec![] }
    }

    #[test]
    fn dots_become_dashes_and_codex_gets_the_real_id() {
        assert_eq!(
            agent_types(Agent::Codex, &ids(&["gpt-6.1-sol", "gpt-5.6-luna"])),
            vec![
                AgentType { name: "codex-gpt-6-1-sol".into(), model: "gpt-6.1-sol".into() },
                AgentType { name: "codex-gpt-5-6-luna".into(), model: "gpt-5.6-luna".into() },
            ]
        );
    }

    #[test]
    fn opencode_provider_slashes_become_dashes() {
        assert_eq!(
            agent_types(Agent::OpenCode, &ids(&["openai/gpt-6.1-sol", "github-copilot/claude-opus-4.8-fast", "opencode-go/qwen4-max"])),
            vec![
                AgentType { name: "opencode-openai-gpt-6-1-sol".into(), model: "openai/gpt-6.1-sol".into() },
                AgentType { name: "opencode-github-copilot-claude-opus-4-8-fast".into(), model: "github-copilot/claude-opus-4.8-fast".into() },
                AgentType { name: "opencode-opencode-go-qwen4-max".into(), model: "opencode-go/qwen4-max".into() },
            ]
        );
    }

    #[test]
    fn ids_no_type_name_can_carry_and_repeats_give_no_type() {
        let long = "g".repeat(59); // "codex-" plus 59 is 65 characters
        let types = agent_types(Agent::Codex, &ids(&["gpt 5", "gpt-5.5", "gpt-5-5", "", &long, "o3"]));
        assert_eq!(types.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["codex-gpt-5-5", "codex-o3"]);
        let fits = "g".repeat(55); // "opencode-" plus 55 is 64 characters
        let types = agent_types(Agent::OpenCode, &ids(&[&format!("{fits}x"), &fits, "a/b.c", "a.b/c"]));
        assert_eq!(types.iter().map(|t| t.name.clone()).collect::<Vec<_>>(), [format!("opencode-{fits}"), "opencode-a-b-c".into()]);
    }

    #[test]
    fn nothing_reaches_the_session_while_every_agent_is_off_or_has_no_model_on() {
        let mut off = CustomSubagents { codex: on(&["gpt-6.1-sol"], true), opencode: on(&["openai/gpt-6.1-sol"], true) };
        off.codex.on = false;
        off.opencode.on = false;
        assert_eq!(off.env(), None);
        let none = CustomSubagents { codex: on(&[], false), opencode: on(&[], false) };
        assert_eq!(none.env(), None);
    }

    #[test]
    fn the_mod_gets_each_agent_on_with_its_edit_switch_and_types() {
        let both = CustomSubagents { codex: on(&["gpt-6.1-sol", "gpt-5.6-luna"], false), opencode: on(&["openai/gpt-6.1-sol"], true) };
        let env: Value = serde_json::from_str(&both.env().unwrap()).unwrap();
        assert_eq!(env.as_object().unwrap().keys().collect::<Vec<_>>(), ["codex", "opencode"]);
        assert_eq!((&env["codex"]["edits"], &env["opencode"]["edits"]), (&json!(false), &json!(true)));
        let names: Vec<&str> = env["codex"]["types"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["codex-gpt-6-1-sol", "codex-gpt-5-6-luna"]);
        assert_eq!(
            env["codex"]["types"][0],
            json!({
                "name": "codex-gpt-6-1-sol",
                "model": "gpt-6.1-sol",
                "description": "Runs the task in OpenAI Codex on gpt-6.1-sol. Use only when the user asks for Codex, GPT or gpt-6.1-sol.",
                "prompt": "Reply with exactly this and nothing else: Codex couldn't run this task because Claudiu's mod lost track of this agent. Start a new Codex subagent.",
            })
        );
        let open = &env["opencode"]["types"][0];
        assert_eq!((&open["name"], &open["model"]), (&json!("opencode-openai-gpt-6-1-sol"), &json!("openai/gpt-6.1-sol")));
        assert_eq!(open["description"], "Runs the task in OpenCode on openai/gpt-6.1-sol. Use only when the user asks for OpenCode or openai/gpt-6.1-sol.");
        assert!(open["prompt"].as_str().unwrap().contains("OpenCode couldn't run this task"));
    }

    #[test]
    fn an_agent_on_with_no_model_on_is_left_out() {
        let codex_only = CustomSubagents { codex: on(&["gpt-6.1-sol"], false), opencode: on(&[], true) };
        let env: Value = serde_json::from_str(&codex_only.env().unwrap()).unwrap();
        assert_eq!(env.as_object().unwrap().keys().collect::<Vec<_>>(), ["codex"]);
    }

    #[test]
    fn a_state_file_from_before_opencode_keeps_its_codex_settings() {
        let saved = r#"{"codex":true,"codex_models":["gpt-6.1-sol"],"codex_edits":true,"codex_catalog":[{"id":"gpt-6.1-sol","name":"6.1 Sol"}]}"#;
        let loaded: CustomSubagents = serde_json::from_str(saved).unwrap();
        assert_eq!(
            loaded,
            CustomSubagents {
                codex: AgentChoice { on: true, models: ids(&["gpt-6.1-sol"]), edits: true, catalog: vec![AgentModel { id: "gpt-6.1-sol".into(), name: "6.1 Sol".into() }] },
                opencode: AgentChoice::default(),
            }
        );
        let again: CustomSubagents = serde_json::from_str(&serde_json::to_string(&loaded).unwrap()).unwrap();
        assert_eq!(again, loaded);
    }
}
