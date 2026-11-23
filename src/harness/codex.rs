//! OpenAI Codex, through Zed's `codex-acp`, the same server Codex subagents use. It builds on Codex's own Rust core
//! rather than the `codex` binary, but reads the user's `~/.codex` config and `codex login`.

use crate::claude::subagents::Agent;

use super::{Adapter, Reopen};
use serde_json::Value;

pub(crate) struct AsyncQuestion {
    pub title: String,
    pub options: Vec<String>,
}

pub(crate) fn async_questions(params: &Value) -> Option<Vec<AsyncQuestion>> {
    if params["mode"] != "form" || params.pointer("/_meta/codex/delivery").and_then(Value::as_str) != Some("async") {
        return None;
    }
    let properties = params.pointer("/requestedSchema/properties")?.as_object()?;
    let questions: Vec<_> = (0..properties.len()).map_while(|index| {
        let field = properties.get(&format!("question_{index}"))?;
        let title = field["title"].as_str()?.to_string();
        let options = field["oneOf"].as_array().into_iter().flatten().filter_map(|option| {
            let label = option["const"].as_str()?;
            let note = properties.get(&format!("question_{index}_note"));
            let is_placeholder = label == "None of the above" && note.is_some()
                && option["description"] == "Provide a different answer in the note field.";
            (!is_placeholder).then(|| label.to_string())
        }).collect();
        Some(AsyncQuestion { title, options })
    }).collect();
    (!questions.is_empty()).then_some(questions)
}

pub(crate) fn async_question_text(questions: &[AsyncQuestion]) -> String {
    questions.iter().map(|question| {
        std::iter::once(question.title.clone()).chain(question.options.iter().map(|option| format!("- {option}")))
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
    default_modes: super::standard_default_modes,
    default_models: || super::default_model_entry(super::Harness::Codex),
    steers: false,
};
