//! Settings that make Claude Code cost more, found in Claudiu's own environment and in Claude Code's `settings.json`
//! `env` block. Each one changed a real request in a measured run on Claude Code 2.1.319: with prompt caching off
//! nothing was read from the cache, and with tool search off the first request grew from 18k to 26k tokens.

use std::path::Path;

use serde_json::{Map, Value};

/// One variable that raises cost, and where it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub name: &'static str,
    pub source: &'static str,
    pub effect: &'static str,
}

const NO_CACHE: &str = "Turns prompt caching off, so every request pays full price for the whole conversation.";
const NO_TOOL_SEARCH: &str = "Sends every tool definition in each request (about 8,000 more tokens in a measured run).";

/// A variable and the effect of having it on. Caching variables are on when truthy, tool search is off when falsy.
const CACHING: [(&str, &str); 6] = [
    ("DISABLE_PROMPT_CACHING", NO_CACHE),
    ("DISABLE_PROMPT_CACHING_FABLE", NO_CACHE),
    ("DISABLE_PROMPT_CACHING_HAIKU", NO_CACHE),
    ("DISABLE_PROMPT_CACHING_MYTHOS", NO_CACHE),
    ("DISABLE_PROMPT_CACHING_OPUS", NO_CACHE),
    ("DISABLE_PROMPT_CACHING_SONNET", NO_CACHE),
];

fn truthy(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn falsy(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off")
}

/// What raises cost in `process_env` (Claudiu's environment, which the sessions it starts inherit) and `file_env` (the
/// `env` block of `settings.json`). A variable set in both places is reported for the process environment.
pub fn find(process_env: &dyn Fn(&str) -> Option<String>, file_env: &Map<String, Value>) -> Vec<Finding> {
    let look = |name: &str| -> Option<(&'static str, String)> {
        if let Some(v) = process_env(name) {
            return Some(("Claudiu's environment", v));
        }
        file_env.get(name).and_then(Value::as_str).map(|v| ("Claude Code's settings.json", v.to_string()))
    };
    let mut found = vec![];
    for (name, effect) in CACHING {
        if let Some((source, _)) = look(name).filter(|(_, v)| truthy(v)) {
            found.push(Finding { name, source, effect });
        }
    }
    if let Some((source, _)) = look("ENABLE_TOOL_SEARCH").filter(|(_, v)| falsy(v)) {
        found.push(Finding { name: "ENABLE_TOOL_SEARCH", source, effect: NO_TOOL_SEARCH });
    }
    found
}

/// The `env` block of the `settings.json` at `path`; empty when the file is missing or is not a JSON object.
pub fn read_file_env(path: &Path) -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(path) else { return Map::new() };
    let Ok(Value::Object(settings)) = serde_json::from_str(&text) else { return Map::new() };
    settings.get("env").and_then(Value::as_object).cloned().unwrap_or_default()
}

/// `find` on the real environment and Claude Code's own `settings.json`.
pub fn scan() -> Vec<Finding> {
    let file_env = crate::claude::claude_home().map(|home| read_file_env(&home.join("settings.json"))).unwrap_or_default();
    find(&|name| std::env::var(name).ok(), &file_env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn none(_: &str) -> Option<String> {
        None
    }

    fn names(found: &[Finding]) -> Vec<&'static str> {
        found.iter().map(|f| f.name).collect()
    }

    #[test]
    fn a_clean_setup_has_no_findings() {
        assert!(find(&none, &Map::new()).is_empty());
    }

    #[test]
    fn caching_off_is_found_in_the_environment_and_says_where() {
        let env = |name: &str| (name == "DISABLE_PROMPT_CACHING_HAIKU").then(|| "1".to_string());
        let found = find(&env, &Map::new());
        assert_eq!(found, [Finding { name: "DISABLE_PROMPT_CACHING_HAIKU", source: "Claudiu's environment", effect: NO_CACHE }]);
    }

    #[test]
    fn the_settings_file_env_block_is_read_too() {
        let file = json!({ "DISABLE_PROMPT_CACHING": "true", "ENABLE_TOOL_SEARCH": "false" }).as_object().unwrap().clone();
        let found = find(&none, &file);
        assert_eq!(names(&found), ["DISABLE_PROMPT_CACHING", "ENABLE_TOOL_SEARCH"]);
        assert!(found.iter().all(|f| f.source == "Claude Code's settings.json"));
    }

    #[test]
    fn a_variable_set_to_off_is_not_a_finding() {
        let file = json!({ "DISABLE_PROMPT_CACHING": "0", "ENABLE_TOOL_SEARCH": "true" }).as_object().unwrap().clone();
        assert!(find(&none, &file).is_empty());
    }

    #[test]
    fn the_process_environment_wins_over_the_file() {
        let env = |name: &str| (name == "DISABLE_PROMPT_CACHING").then(|| "1".to_string());
        let file = json!({ "DISABLE_PROMPT_CACHING": "1" }).as_object().unwrap().clone();
        assert_eq!(find(&env, &file)[0].source, "Claudiu's environment");
    }

    #[test]
    fn a_missing_or_broken_settings_file_gives_an_empty_env_block() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_file_env(&tmp.path().join("missing.json")).is_empty());
        std::fs::write(tmp.path().join("bad.json"), "{ nope").unwrap();
        assert!(read_file_env(&tmp.path().join("bad.json")).is_empty());
    }
}
