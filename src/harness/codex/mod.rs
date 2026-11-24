//! OpenAI Codex, through Zed's `codex-acp`, the same server Codex subagents use. It builds on Codex's own Rust core
//! rather than the `codex` binary, but reads the user's `~/.codex` config and `codex login`.

pub mod limits;

use super::subagents::Agent;

use super::{Adapter, Reopen};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AsyncOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AsyncQuestion {
    pub title: String,
    pub options: Vec<AsyncOption>,
    pub multi_select: bool,
}

fn parse_async_titled_options(entries: &[Value], has_note: bool) -> Vec<AsyncOption> {
    entries
        .iter()
        .filter_map(|option| {
            let label = option["const"].as_str()?;
            let is_placeholder = label == "None of the above"
                && has_note
                && option["description"] == "Provide a different answer in the note field.";
            if is_placeholder {
                return None;
            }
            let description = option["description"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
            Some(AsyncOption { label: label.to_string(), description })
        })
        .collect()
}

fn parse_async_enum_options(entries: &[Value], names: Option<&Vec<Value>>) -> Vec<AsyncOption> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let val = v.as_str()?;
            let label = names.and_then(|n| n.get(i)).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).unwrap_or(val);
            Some(AsyncOption { label: label.to_string(), description: None })
        })
        .collect()
}

pub(crate) fn async_questions(params: &Value) -> Option<Vec<AsyncQuestion>> {
    if params["mode"] != "form" || params.pointer("/_meta/codex/delivery").and_then(Value::as_str) != Some("async") {
        return None;
    }
    let properties = params.pointer("/requestedSchema/properties")?.as_object()?;
    let questions: Vec<_> = (0..properties.len()).map_while(|index| {
        let field = properties.get(&format!("question_{index}"))?;
        let title = field["title"].as_str()?.to_string();
        let has_note = properties.contains_key(&format!("question_{index}_note"));
        let multi_select = field["type"].as_str() == Some("array");
        let options = if multi_select {
            let items = &field["items"];
            if let Some(any_of) = items["anyOf"].as_array() {
                parse_async_titled_options(any_of, has_note)
            } else if let Some(enums) = items["enum"].as_array() {
                parse_async_enum_options(enums, items["enumNames"].as_array())
            } else {
                vec![]
            }
        } else if let Some(one_of) = field["oneOf"].as_array() {
            parse_async_titled_options(one_of, has_note)
        } else if let Some(enums) = field["enum"].as_array() {
            parse_async_enum_options(enums, field["enumNames"].as_array())
        } else {
            vec![]
        };
        Some(AsyncQuestion { title, options, multi_select })
    }).collect();
    (!questions.is_empty()).then_some(questions)
}

pub(crate) fn async_question_text(questions: &[AsyncQuestion]) -> String {
    questions.iter().map(|question| {
        std::iter::once(question.title.clone()).chain(question.options.iter().map(|option| format!("- {}", option.label)))
            .collect::<Vec<_>>().join("\n")
    }).collect::<Vec<_>>().join("\n\n")
}

pub const ADAPTER: Adapter = Adapter {
    name: "Codex",
    server_name: "codex-acp",
    server: || super::subagent_server(Agent::Codex),
    reopen: Reopen::Load,
    delete_model_session: false,
    compact_command: Some("compact"),
    default_models: || super::default_model_entry(super::Harness::Codex),
    steers: false,
};
