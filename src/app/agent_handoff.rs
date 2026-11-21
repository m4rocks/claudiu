//! Dynamic in-chat handoff between harnesses: switching the provider in the chat composer and sending a message
//! seamlessly transitions the same session to the new harness, preserving prior conversation history visually and
//! sending the transcript + the user's new message as the first turn.

use super::*;
use crate::harness::thread::Entry;

/// How much of the conversation goes along, newest kept: enough for the plan and recent steps, without filling the
/// new agent's context.
const MAX_CHARS: usize = 40_000;
/// Entries read from the end before the cut, so a long session is not turned into text whole.
const LAST_ENTRIES: usize = 400;

/// The first message sent to the new harness when taking over an existing conversation along with the user's message.
pub fn handoff_message_with_prompt(from: &str, transcript: &str, user_message: &str) -> String {
    let mut start = transcript.len().saturating_sub(MAX_CHARS);
    while !transcript.is_char_boundary(start) {
        start += 1;
    }
    let cut = if start > 0 { "[earlier messages left out]\n\n" } else { "" };
    format!(
        "You are taking over work from a {from} session in this folder. Its conversation so far is below; tool steps \
         show as [kind: title]. Read it, check the current state of the files, then answer the user's next message.\n\n\
         <conversation>\n{cut}{}\n</conversation>\n\nUser's next message:\n{user_message}",
        &transcript[start..]
    )
}

impl Workspace {
    /// Switch session `id` in place to `to`, carrying over the conversation transcript and sending `user_message`.
    pub(super) fn dynamic_handoff(&mut self, id: &str, to: Harness, user_message: String, effort: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.live_view(id).and_then(|v| v.thread()).cloned() else { return };
        let Some(mut rec) = self.store.session(id).cloned() else { return };
        let cwd = rec.cwd.clone();
        let (from_name, transcript, mut prior_entries) = {
            let t = thread.read(cx);
            (t.harness.name(), t.thread.transcript(LAST_ENTRIES), t.thread.entries.clone())
        };
        prior_entries.push(Entry::Notice {
            text: format!("Switched from {from_name} to {}", to.name()),
            error: false,
        });
        let prompt = if transcript.trim().is_empty() {
            user_message
        } else {
            handoff_message_with_prompt(from_name, &transcript, &user_message)
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
        if let Some(level) = effort {
            start.options.retain(|(cat, _)| *cat != "thought_level");
            start.options.push(("thought_level", level));
        }
        self.start_thread(rec, start, false, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_carries_the_conversation_and_the_new_message_keeping_the_end_when_long() {
        let short = handoff_message_with_prompt("Claude Code", "User: plan it\n\nAgent: step 1", "now implement step 2");
        assert!(short.contains("from a Claude Code session") && short.contains("<conversation>\nUser: plan it\n\nAgent: step 1\n</conversation>"));
        assert!(short.ends_with("User's next message:\nnow implement step 2"));
        assert!(!short.contains("left out"));

        let long = format!("User: {}é the end", "x".repeat(MAX_CHARS));
        let message = handoff_message_with_prompt("Codex", &long, "finish up");
        assert!(message.contains("[earlier messages left out]") && message.contains("é the end\n</conversation>"), "the newest part goes along");
        assert!(message.ends_with("User's next message:\nfinish up"));
        assert!(!message.contains("User: x"), "the oldest part is cut");
    }
}
