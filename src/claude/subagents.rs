//! Custom subagents: agent types that run a task in another coding agent instead of Claude (Settings → Custom
//! subagents). What differs from one agent to the next lives in its `Provider`.
//!
//! m4code lists the agent types in `M4CODE_SUBAGENTS`, and the mod registers them, so Claude Code names them
//! `m4code:<type>`. Interactive Claude Code ignores `claude --agents` (only `-p` reads it), so the mod is the one way
//! to define them per session. The mod answers each turn of those agents through the `m4code --acp` helper
//! (`claude/acp.rs`).

use std::time::Duration;

use super::limits::Pool;
use crate::panels::{logos, theme};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The agent types the mod answers for, as JSON. Absent when no custom subagent is on.
pub const ENV: &str = "M4CODE_SUBAGENTS";
/// The path of this executable, which the mod runs as `<path> --acp`.
pub const EXE_ENV: &str = "M4CODE_EXE";

/// Claude Code takes up to 64 characters in an agent type's name, before the plugin's `m4code:`.
const TYPE_NAME_MAX: usize = 64;

/// A coding agent a custom subagent runs the task in. Its serde name is the key the mod reads in `M4CODE_SUBAGENTS`
/// and the prefix of its agent types. Adding one takes a variant here, its place in `ALL`, and a `Provider`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Codex,
    OpenCode,
    Antigravity,
}

/// How a provider reads its limits and how often it asks again.
pub struct Limits {
    pub read: fn() -> Result<Vec<Pool>, String>,
    pub every: Duration,
}

/// Everything m4code knows about one agent, including its ACP server and usage limits.
pub struct Provider {
    /// The limits reader and refresh cadence, absent when the provider has no limits to show.
    pub limits: Option<Limits>,
    /// The color of its sidebar limits.
    pub color: u32,
    /// The logos and optional short label that identify a pool in the sidebar.
    pub marks: fn(&str) -> (&'static [&'static logos::Logo], Option<String>),
    /// The name the settings page and the subagent's text use.
    pub name: &'static str,
    /// The key in `M4CODE_SUBAGENTS` and in the state file, and the start of every agent type name.
    pub key: &'static str,
    /// The name Claude reads in the agent type's description.
    task_name: &'static str,
    /// What the user asks for, besides the model, when Claude should pick this agent.
    triggers: &'static str,
    /// The ACP server: a program on the PATH and its arguments.
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// Where the installer puts `program` on Windows, under `%LOCALAPPDATA%`, for when it isn't on the PATH.
    pub windows_fallback: Option<&'static str>,
    /// The program as failures name it.
    pub server: &'static str,
    /// The failure when `program` can't be found.
    pub missing: &'static str,
    /// The failure when the agent answers that it isn't signed in.
    pub sign_in: &'static str,
    /// A risk the settings page shows before the agent is switched on, such as a sign-in its maker's terms forbid here.
    pub warning: Option<&'static str>,
    /// Arguments after `args` for one task's server, made ready here.
    pub task_args: fn(&super::acp::Request) -> Result<Vec<String>, String>,
    /// The environment the server starts with, for agents that take the model, effort and edits that way.
    pub environment: fn(&super::acp::Request) -> Vec<(&'static str, String)>,
    /// ACP config options (`session/set_config_option`) set before the task, for agents that take them that way.
    pub config_options: fn(&super::acp::Request) -> Vec<(&'static str, String)>,
    /// Whether messages Claude sends mid-task go through this helper even when the agent can't steer: they then follow
    /// up in the same session. Otherwise Claude Code queues them itself.
    pub inbox_without_steering: bool,
    /// Whether a steer asks the agent to answer `promptRequired`, rather than start a turn of its own, when idle.
    pub steering_prompt_required: bool,
    /// Whether a tool call can start with only the tool's name, its target arriving in the first update.
    pub deferred_tool_target: bool,
    /// Whether the agent can end its turn while a command runs on in the background, then carry on by itself once it
    /// finishes, with no new prompt and no second stop reason.
    pub resumes_after_background: bool,
}

const CODEX: Provider = Provider {
    limits: Some(Limits { read: || {
        let usage = super::codex_limits::codex_home().and_then(|home| super::codex_limits::read(&home)).ok_or("Codex has no limits reading yet")?;
        Ok(vec![Pool { name: "Codex".into(), usage }])
    }, every: Duration::from_secs(3) }),
    color: theme::CODEX,
    marks: logos::codex_marks,
    name: "Codex",
    key: "codex",
    task_name: "OpenAI Codex",
    triggers: "Codex, GPT",
    program: "npx",
    // Pinned so an upstream adapter change reaches users only with a m4code release.
    args: &["-y", "@agentclientprotocol/codex-acp@2.6.0"],
    windows_fallback: None,
    server: "codex-acp",
    missing: "npx was not found on PATH. Codex subagents need Node.js.",
    sign_in: "Codex isn't signed in. Run `codex login`, then try again.",
    warning: None,
    task_args: super::acp::no_args,
    environment: super::acp::codex_env,
    config_options: super::acp::no_options,
    inbox_without_steering: true,
    steering_prompt_required: true,
    deferred_tool_target: false,
    resumes_after_background: false,
};

const OPENCODE: Provider = Provider {
    limits: None,
    color: theme::TEXT,
    marks: |name| (&[], Some(name.to_uppercase())),
    name: "OpenCode",
    key: "opencode",
    task_name: "OpenCode",
    triggers: "OpenCode",
    program: "opencode",
    args: &["acp"],
    windows_fallback: None,
    server: "opencode",
    missing: "opencode was not found on PATH. OpenCode subagents need OpenCode installed.",
    sign_in: "OpenCode isn't signed in. Run `opencode auth login`, then try again.",
    warning: None,
    task_args: super::acp::no_args,
    environment: super::acp::opencode_env,
    config_options: super::acp::no_options,
    inbox_without_steering: false,
    steering_prompt_required: false,
    deferred_tool_target: true,
    resumes_after_background: false,
};

const ANTIGRAVITY: Provider = Provider {
    limits: Some(Limits { read: || {
        let agy = super::acp::locate(Agent::Antigravity).ok_or("agy was not found")?;
        super::antigravity_limits::read(&agy, &crate::datadir::data_dir())
    }, every: super::antigravity_limits::REFRESH_EVERY }),
    color: theme::ANTIGRAVITY,
    marks: logos::antigravity_marks,
    name: "Antigravity",
    key: "antigravity",
    task_name: "Google Antigravity",
    triggers: "Antigravity, Gemini",
    program: "agy",
    args: &["acp"],
    windows_fallback: Some("agy/bin/agy.exe"),
    server: "agy",
    missing: "agy was not found. Antigravity subagents need the Antigravity CLI installed.",
    sign_in: "Antigravity isn't signed in. Run `agy` and sign in, then try again.",
    // Google's FAQ names Claude Code among the third-party software barred from Antigravity logins, under Section 6 of
    // the Antigravity Additional Terms (https://antigravity.google/docs/faq, https://antigravity.google/terms).
    warning: Some("Google's Antigravity terms forbid using another company's agent, such as Claude Code, with a personal Google sign-in. That covers the Free, AI Plus, Pro and Ultra plans. Google can suspend or terminate the Antigravity and Gemini CLI accounts of anyone who does. Switch this on at your own risk."),
    task_args: super::acp::antigravity_args,
    environment: super::acp::no_env,
    config_options: super::acp::antigravity_options,
    inbox_without_steering: false,
    steering_prompt_required: false,
    deferred_tool_target: false,
    // agy gives a command about ten seconds, then moves it to the background and ends the turn.
    resumes_after_background: true,
};

/// One value per agent, such as each agent's part of the settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMap<T>([T; Agent::ALL.len()]);

impl<T> AgentMap<T> {
    pub fn from_fn(mut f: impl FnMut(Agent) -> T) -> Self {
        Self(std::array::from_fn(|i| f(Agent::ALL[i])))
    }

    pub fn get(&self, agent: Agent) -> &T {
        &self.0[agent.index()]
    }

    pub fn get_mut(&mut self, agent: Agent) -> &mut T {
        &mut self.0[agent.index()]
    }
}

impl<T: Default> Default for AgentMap<T> {
    fn default() -> Self {
        Self::from_fn(|_| T::default())
    }
}

impl Agent {
    /// In the order the settings page lists them.
    pub const ALL: [Agent; 3] = [Agent::Codex, Agent::OpenCode, Agent::Antigravity];

    pub fn provider(self) -> &'static Provider {
        match self {
            Agent::Codex => &CODEX,
            Agent::OpenCode => &OPENCODE,
            Agent::Antigravity => &ANTIGRAVITY,
        }
    }

    /// Its place in `ALL`.
    fn index(self) -> usize {
        Agent::ALL.iter().position(|a| *a == self).expect("every agent is in ALL")
    }

    pub fn name(self) -> &'static str {
        self.provider().name
    }

    pub fn key(self) -> &'static str {
        self.provider().key
    }

    /// What Claude reads to decide when to use an agent type of this agent.
    fn description(self, model: &str) -> String {
        let p = self.provider();
        if model.is_empty() {
            return format!("Runs the task in {} on its default model. Use only when the user asks for {}.", p.task_name, p.triggers);
        }
        format!("Runs the task in {} on {model}. Use only when the user asks for {} or {model}.", p.task_name, p.triggers)
    }

    /// The mod answers these agents' turns, so Claude runs for one only if the mod lost track of it (a reload, and
    /// the agent gone from the agent list). It then says so rather than attempt the task.
    fn stub_prompt(self) -> String {
        let name = self.name();
        format!("Reply with exactly this and nothing else: {name} couldn't run this task because m4code's mod lost track of this agent. Start a new {name} subagent.")
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

/// Keeps the flat provider keys used by existing state files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomSubagents(pub AgentMap<AgentChoice>);

impl Default for CustomSubagents {
    fn default() -> Self {
        Self(AgentMap::from_fn(|agent| {
            let enabled = agent != Agent::Antigravity;
            AgentChoice { on: enabled, edits: enabled, ..Default::default() }
        }))
    }
}

impl Serialize for CustomSubagents {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut stored = Map::new();
        for agent in Agent::ALL {
            let choice = self.get(agent);
            let key = agent.key();
            stored.insert(key.into(), json!(choice.on));
            stored.insert(format!("{key}_models"), json!(choice.models));
            stored.insert(format!("{key}_edits"), json!(choice.edits));
            stored.insert(format!("{key}_catalog"), json!(choice.catalog));
        }
        stored.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CustomSubagents {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut stored = Map::<String, Value>::deserialize(deserializer)?;
        fn field<T: serde::de::DeserializeOwned, E: serde::de::Error>(stored: &mut Map<String, Value>, key: &str, fallback: T) -> Result<T, E> {
            stored.remove(key).map(serde_json::from_value).transpose().map(|v| v.unwrap_or(fallback)).map_err(E::custom)
        }
        let mut result = Self::default();
        for agent in Agent::ALL {
            let key = agent.key();
            let defaults = result.get(agent).clone();
            *result.get_mut(agent) = AgentChoice {
                on: field::<_, D::Error>(&mut stored, key, defaults.on)?,
                models: field::<_, D::Error>(&mut stored, &format!("{key}_models"), vec![])?,
                edits: field::<_, D::Error>(&mut stored, &format!("{key}_edits"), defaults.edits)?,
                catalog: field::<_, D::Error>(&mut stored, &format!("{key}_catalog"), vec![])?,
            };
        }
        Ok(result)
    }
}

/// One agent type: its name in Claude Code and the model the agent runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentType {
    pub name: String,
    pub model: String,
}

/// The agent type name of a model: the agent's key and the id, with every character a type name can't hold (dots,
/// slashes, colons) turned into a dash, cut to the length Claude Code takes (`gpt-6.1-sol` → `codex-gpt-6-1-sol`,
/// `ollama/qwen3:8b` → `opencode-ollama-qwen3-8b`). None for an empty id.
pub fn type_name(agent: Agent, model: &str) -> Option<String> {
    if model.is_empty() {
        return None;
    }
    let id: String = model.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') { c } else { '-' }).collect();
    // Every character is ASCII now, so cutting at a byte count can't split one.
    let mut name = format!("{}-{id}", agent.key());
    name.truncate(TYPE_NAME_MAX);
    Some(name)
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
        self.0.get(agent)
    }

    pub fn get_mut(&mut self, agent: Agent) -> &mut AgentChoice {
        self.0.get_mut(agent)
    }

    /// Each agent switched on that has a type, with its types.
    fn types(&self) -> Vec<(Agent, Vec<AgentType>)> {
        Agent::ALL
            .into_iter()
            .filter(|a| self.get(*a).on)
            .map(|a| {
                let choice = self.get(a);
                let models = if choice.models.is_empty() { choice.catalog.iter().map(|m| m.id.clone()).collect() } else { choice.models.clone() };
                let types = if models.is_empty() {
                    vec![AgentType { name: format!("{}-default", a.key()), model: String::new() }]
                } else {
                    agent_types(a, &models)
                };
                (a, types)
            })
            .filter(|(_, types)| !types.is_empty())
            .collect()
    }

    /// What the mod reads from `M4CODE_SUBAGENTS`, each type as the mod registers it; `None` when no type is on.
    pub fn env_for(&self, enabled: &[crate::harness::Harness]) -> Option<String> {
        let types: Vec<_> = self.types().into_iter().filter(|(agent, _)| enabled.contains(&crate::harness::Harness::from_agent(*agent))).collect();
        if types.is_empty() {
            return None;
        }
        let mut env = Map::new();
        for (agent, types) in types {
            let types: Vec<Value> = types
                .iter()
                .map(|t| json!({ "name": t.name, "model": t.model, "description": agent.description(&t.model), "prompt": agent.stub_prompt() }))
                .collect();
            env.insert(agent.key().into(), json!({ "edits": self.get(agent).edits, "name": agent.name(), "types": types }));
        }
        Some(Value::Object(env).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_subagent_settings_enable_edits_except_for_antigravity() {
        for choices in [CustomSubagents::default(), serde_json::from_str::<CustomSubagents>("{}").unwrap()] {
            for agent in [Agent::Codex, Agent::OpenCode] {
                assert!(choices.get(agent).on);
                assert!(choices.get(agent).edits);
            }
            assert!(!choices.get(Agent::Antigravity).on);
            assert!(!choices.get(Agent::Antigravity).edits);
        }
        let explicit: CustomSubagents = serde_json::from_str(r#"{"codex":false,"codex_edits":false,"opencode":true,"opencode_edits":false}"#).unwrap();
        assert!(!explicit.get(Agent::Codex).on);
        assert!(!explicit.get(Agent::Codex).edits);
        assert!(explicit.get(Agent::OpenCode).on);
        assert!(!explicit.get(Agent::OpenCode).edits);
    }

    #[test]
    fn every_limits_provider_has_visible_marks_and_a_color() {
        for agent in Agent::ALL {
            let provider = agent.provider();
            if provider.limits.is_some() {
                assert_ne!(provider.color, theme::PANEL_BG, "{}", provider.name);
                for pool in ["Codex", "Gemini Models", "Claude and GPT models", "Gemini 4 Argon", "Some New models"] {
                    let (marks, name) = (provider.marks)(pool);
                    assert!(!marks.is_empty() || name.is_some_and(|name| !name.is_empty()), "{}: {pool}", provider.name);
                    assert!(marks.iter().all(|mark| !mark.svg.is_empty()), "{}: {pool}", provider.name);
                }
            }
        }
    }

    fn choices(codex: AgentChoice, opencode: AgentChoice) -> CustomSubagents {
        let mut choices = CustomSubagents::default();
        *choices.get_mut(Agent::Codex) = codex;
        *choices.get_mut(Agent::OpenCode) = opencode;
        choices
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn on(models: &[&str], edits: bool) -> AgentChoice {
        AgentChoice { on: true, models: ids(models), edits, catalog: vec![] }
    }

    #[test]
    fn antigravity_round_trips_flat_state_and_supplies_mod_texts() {
        let mut choices = CustomSubagents::default();
        *choices.get_mut(Agent::Antigravity) = AgentChoice { catalog: vec![AgentModel { id: "gemini-4-neon".into(), name: "Gemini 4 Neon".into() }], ..on(&["gemini-4-neon"], true) };
        let stored = serde_json::to_value(&choices).unwrap();
        assert_eq!(stored["antigravity"], true);
        assert_eq!(stored["antigravity_models"], json!(["gemini-4-neon"]));
        assert_eq!(stored["antigravity_edits"], true);
        assert_eq!(stored["antigravity_catalog"][0]["name"], "Gemini 4 Neon");
        assert_eq!(serde_json::from_value::<CustomSubagents>(stored).unwrap(), choices);
        let env: Value = serde_json::from_str(&choices.env_for(&crate::harness::Harness::ALL).unwrap()).unwrap();
        assert_eq!(env["antigravity"]["name"], "Antigravity");
        assert_eq!(env["antigravity"]["types"][0]["description"], "Runs the task in Google Antigravity on gemini-4-neon. Use only when the user asks for Antigravity, Gemini or gemini-4-neon.");
        for agent in Agent::ALL {
            assert_eq!(Agent::ALL[agent.index()], agent);
            assert_eq!(agent.key(), serde_json::to_value(agent).unwrap().as_str().unwrap(), "the state file and the mod use the same key");
        }
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
    fn every_model_gets_a_type_and_repeats_give_none() {
        let types = agent_types(Agent::OpenCode, &ids(&["ollama/qwen3:8b", "openrouter/deepseek/deepseek-r1:free", "a b+c", "", "a/b.c", "a.b/c"]));
        let names: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["opencode-ollama-qwen3-8b", "opencode-openrouter-deepseek-deepseek-r1-free", "opencode-a-b-c"]);
        // "a b+c" takes opencode-a-b-c first, so "a/b.c" and "a.b/c" give none.
        assert_eq!(types[2].model, "a b+c");
        let long = "g".repeat(70);
        let types = agent_types(Agent::Codex, &ids(&[&long]));
        assert_eq!(types[0].name, format!("codex-{}", "g".repeat(58)), "cut to 64 characters");
        assert_eq!(types[0].model, long, "the agent still gets the real id");
    }

    #[test]
    fn nothing_reaches_the_session_while_every_agent_is_off() {
        let mut off = choices(on(&["gpt-6.1-sol"], true), on(&["openai/gpt-6.1-sol"], true));
        off.get_mut(Agent::Codex).on = false;
        off.get_mut(Agent::OpenCode).on = false;
        assert_eq!(off.env_for(&crate::harness::Harness::ALL), None);
    }

    #[test]
    fn the_mod_gets_each_agent_on_with_its_edit_switch_and_types() {
        let both = choices(on(&["gpt-6.1-sol", "gpt-5.6-luna"], false), on(&["openai/gpt-6.1-sol"], true));
        let env: Value = serde_json::from_str(&both.env_for(&crate::harness::Harness::ALL).unwrap()).unwrap();
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
                "prompt": "Reply with exactly this and nothing else: Codex couldn't run this task because m4code's mod lost track of this agent. Start a new Codex subagent.",
            })
        );
        let open = &env["opencode"]["types"][0];
        assert_eq!((&open["name"], &open["model"]), (&json!("opencode-openai-gpt-6-1-sol"), &json!("openai/gpt-6.1-sol")));
        assert_eq!(open["description"], "Runs the task in OpenCode on openai/gpt-6.1-sol. Use only when the user asks for OpenCode or openai/gpt-6.1-sol.");
        assert!(open["prompt"].as_str().unwrap().contains("OpenCode couldn't run this task"));
    }

    #[test]
    fn enabled_agents_use_their_default_model_before_the_catalog_arrives() {
        let codex_only = choices(on(&["gpt-6.1-sol"], false), on(&[], true));
        let env: Value = serde_json::from_str(&codex_only.env_for(&crate::harness::Harness::ALL).unwrap()).unwrap();
        assert_eq!(env.as_object().unwrap().keys().collect::<Vec<_>>(), ["codex", "opencode"]);
        assert_eq!(env["opencode"]["types"][0]["name"], "opencode-default");
        assert_eq!(env["opencode"]["types"][0]["model"], "");
        assert!(env["opencode"]["types"][0]["description"].as_str().unwrap().contains("default model"));
        assert_eq!(codex_only.env_for(&[crate::harness::Harness::Claude]), None);
    }

    #[test]
    fn a_state_file_from_before_opencode_keeps_its_codex_settings() {
        let saved = r#"{"codex":true,"codex_models":["gpt-6.1-sol"],"codex_edits":true,"codex_catalog":[{"id":"gpt-6.1-sol","name":"6.1 Sol"}]}"#;
        let loaded: CustomSubagents = serde_json::from_str(saved).unwrap();
        assert_eq!(
            loaded,
            choices(AgentChoice { on: true, models: ids(&["gpt-6.1-sol"]), edits: true, catalog: vec![AgentModel { id: "gpt-6.1-sol".into(), name: "6.1 Sol".into() }] }, CustomSubagents::default().get(Agent::OpenCode).clone())
        );
        let again: CustomSubagents = serde_json::from_str(&serde_json::to_string(&loaded).unwrap()).unwrap();
        assert_eq!(again, loaded);
    }
}
