//! Dynamic in-chat handoff between harnesses: switching the provider in the chat composer and sending a message
//! seamlessly transitions the same session to the new harness, preserving prior conversation history visually and
//! sending the transcript + the user's new message as the first turn.

use super::*;
use crate::harness::thread::Entry;

/// Entries read from the end before the cut, so a long session is not turned into text whole.
const LAST_ENTRIES: usize = 400;

impl Workspace {
    /// Switch session `id` in place to `to` (with an optional specific model & effort), carrying over the conversation
    /// transcript and sending `message`: the user's, or with `by_agent` what the agent said to hand over.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn dynamic_handoff(&mut self, id: &str, to: Harness, model: Option<String>, message: String, by_agent: bool, effort: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.live_view(id).and_then(|v| v.thread()).cloned() else { return };
        let Some(mut rec) = self.store.session(id).cloned() else { return };
        let cwd = rec.cwd.clone();
        let (from_name, transcript, mut prior_entries) = {
            let t = thread.read(cx);
            (t.harness.name(), t.thread.transcript(LAST_ENTRIES), t.thread.entries.clone())
        };
        let target_label = model.as_deref().filter(|m| !m.is_empty()).unwrap_or_else(|| to.name());
        prior_entries.push(Entry::Notice {
            text: format!("Switched to {target_label}"),
            error: false,
        });
        // An agent's own message goes in as the last thing it said, so the chat shows no message the user didn't type.
        let prompt = if by_agent {
            let said = if transcript.trim().is_empty() { format!("Agent: {message}") } else { format!("{transcript}\n\nAgent: {message}") };
            crate::harness::thread::handoff_prompt(from_name, &said, "")
        } else if transcript.trim().is_empty() {
            message
        } else {
            crate::harness::thread::handoff_prompt(from_name, &transcript, &message)
        };
        let mut meta = rec.claude.clone().unwrap_or_default();
        meta.harness = to;
        meta.session_id.clear();
        rec.claude = Some(meta.clone());
        if let Some(stored) = self.store.session_mut(id) {
            stored.claude = Some(meta.clone());
        }
        self.store.mark_dirty();
        let Some(mut start) = self.thread_start(&rec.id, &cwd, &meta, rec.chosen_title().map(String::from), Some(prompt), Starter::User, cx) else {
            return;
        };
        start.prior_entries = prior_entries;
        if let Some(model_id) = model.filter(|m| !m.is_empty()) {
            start.options.retain(|(cat, _)| *cat != "model");
            start.options.push(("model", model_id));
        }
        if let Some(level) = effort {
            start.options.retain(|(cat, _)| *cat != "thought_level");
            start.options.push(("thought_level", level));
        }
        self.start_thread(rec, start, false, window, cx);
    }
}
