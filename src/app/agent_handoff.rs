//! Handing a session's work to another agent, such as planning in one model and building in another: a new session
//! of the other agent starts in the same folder with the conversation so far as its first message. The first session
//! stays as it was.

use super::*;

/// How much of the conversation goes along, newest kept: enough for the plan and recent steps, without filling the
/// new agent's context.
const MAX_CHARS: usize = 40_000;
/// Entries read from the end before the cut, so a long session is not turned into text whole.
const LAST_ENTRIES: usize = 400;

/// The first message of the session that takes over.
pub fn handoff_message(from: &str, transcript: &str) -> String {
    let mut start = transcript.len().saturating_sub(MAX_CHARS);
    while !transcript.is_char_boundary(start) {
        start += 1;
    }
    let cut = if start > 0 { "[earlier messages left out]\n\n" } else { "" };
    format!(
        "You are taking over work from a {from} session in this folder. Its conversation so far is below; tool steps \
         show as [kind: title]. Read it, check the current state of the files, then carry on where it left off. If \
         the next step isn't clear, say what you understand the task to be and ask.\n\n<conversation>\n{cut}{}\n</conversation>",
        &transcript[start..]
    )
}

impl Workspace {
    pub(super) fn hand_to_agent(&mut self, id: &str, to: Harness, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.live_view(id).and_then(|v| v.thread()).cloned() else { return };
        let Some(rec) = self.store.session(id) else { return };
        let cwd = rec.cwd.clone();
        let (from, transcript) = {
            let t = thread.read(cx);
            (t.harness.name(), t.thread.transcript(LAST_ENTRIES))
        };
        if transcript.trim().is_empty() {
            return self.toast("Nothing to hand over yet: the session has no messages.", false, cx);
        }
        self.new_session_with(to, cwd, Some(handoff_message(from, &transcript)), vec![], Starter::OtherSession, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_carries_the_conversation_and_keeps_its_end_when_long() {
        let short = handoff_message("Claude Code", "User: plan it\n\nAgent: step 1");
        assert!(short.contains("from a Claude Code session") && short.contains("<conversation>\nUser: plan it\n\nAgent: step 1\n</conversation>"));
        assert!(!short.contains("left out"));

        let long = format!("User: {}é the end", "x".repeat(MAX_CHARS));
        let message = handoff_message("Codex", &long);
        assert!(message.contains("[earlier messages left out]") && message.ends_with("é the end\n</conversation>"), "the newest part goes along");
        assert!(!message.contains("User: x"), "the oldest part is cut");
    }
}
