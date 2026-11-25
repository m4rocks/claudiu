use super::*;

/// The value to set `config` to, given the options the agent `offered`. A thinking level the model doesn't offer
/// becomes the highest one it does below it, or none, which leaves the model's own default: Claude's effort is a hint,
/// not worth failing the task for. Anything else is sent as asked, so an unknown model fails with the agent's reason.
pub(super) fn offered_value(offered: &Value, config: &str, wanted: &str) -> Option<String> {
    let choices: Option<Vec<&str>> = offered
        .as_array()
        .and_then(|options| options.iter().find(|o| o["id"] == config))
        .and_then(|option| option["options"].as_array())
        .map(|choices| choices.iter().filter_map(|c| c["value"].as_str()).collect());
    let Some(choices) = choices.filter(|_| config == "thought_level") else { return Some(wanted.into()) };
    let rank = THOUGHT_LEVELS.iter().position(|l| *l == wanted)?;
    THOUGHT_LEVELS[..=rank].iter().rev().find(|l| choices.contains(l)).map(|l| l.to_string())
}

pub(super) fn effort_level(effort: Option<&str>) -> Option<&str> {
    match effort {
        Some("max") => Some("xhigh"),
        Some(level @ ("low" | "medium" | "high" | "xhigh")) => Some(level),
        _ => None,
    }
}

/// The config Codex gets on top of the user's own: the model and the reasoning effort, where given. Codex has no `max`,
/// so `max` becomes its highest level, `xhigh`; a level Codex doesn't know is left to the user's config.
pub(super) fn codex_config(model: Option<&str>, effort: Option<&str>) -> Option<Value> {
    let mut config = serde_json::Map::new();
    if let Some(model) = model {
        config.insert("model".into(), json!(model));
    }
    let effort = effort_level(effort);
    if let Some(effort) = effort {
        config.insert("model_reasoning_effort".into(), json!(effort));
    }
    (!config.is_empty()).then_some(Value::Object(config))
}

/// The tools OpenCode may use read-only: reading and searching files. Everything else, the shell included, is denied,
/// since OpenCode has no sandbox that would let a command run without changing anything.
fn opencode_read_only() -> Value {
    json!({ "*": "deny", "read": "allow", "glob": "allow", "grep": "allow", "list": "allow", "m4code_workspace_context": "allow" })
}

/// The config OpenCode gets on top of the user's own. Read-only pins the `build` agent and denies under it too,
/// because an agent's own permissions in a project's or the user's opencode.json beat the top-level ones.
pub(super) fn opencode_config(model: Option<&str>, edits: bool) -> Value {
    let mut config = json!({});
    if let Some(model) = model {
        config["model"] = json!(model);
    }
    if !edits {
        config["default_agent"] = json!("build");
        config["permission"] = opencode_read_only();
        config["agent"] = json!({ "build": { "permission": opencode_read_only() } });
    }
    config
}

