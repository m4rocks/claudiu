//! "Regenerate title": a new name for a running session, written from what was said in it by the session's own mod
//! (`claude/ask.rs`), and used as the session's chosen name (which also renames it inside Claude Code).

use super::*;

const TITLE_SYSTEM: &str = "You name conversations. Reply with the title only: two to six words, no quotes, no trailing punctuation, \
in the language the conversation is mostly in. Say what the conversation is about, not how it started.";

impl Workspace {
    pub(super) fn regen_title(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.is_running(id, cx) {
            return self.toast("Start the session first: its title is written through the running session.", true, cx);
        }
        let Some(transcript) = self.store.session(id).and_then(|s| s.claude.as_ref()?.transcript.clone()) else {
            return self.toast("Nothing to title yet.", false, cx);
        };
        self.toast("Writing a new title…", false, cx);
        let id = id.to_string();
        let record = id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let said = crate::claude::conversation_text(&transcript);
                    if said.trim().is_empty() {
                        return Err("Nothing to title yet.".to_string());
                    }
                    let prompt = format!("Conversation:\n\n{said}\n\nWrite the title.");
                    let raw = crate::git::commit::generate(&|system, prompt| crate::claude::ask::complete(&record, system, prompt), TITLE_SYSTEM, &prompt)?;
                    let title: String = raw.lines().next().unwrap_or_default().trim().trim_matches(['"', '\'', '`', '.']).chars().take(60).collect();
                    if title.is_empty() { Err("the model returned no title".to_string()) } else { Ok(title) }
                })
                .await;
            let _ = this.update(cx, |ws, cx| match result {
                Ok(title) => {
                    if let Some(rec) = ws.store.session_mut(&id) {
                        rec.custom_title = Some(title.clone());
                    }
                    ws.toast(format!("Renamed to “{title}”"), false, cx);
                }
                Err(e) => ws.toast(e, true, cx),
            });
        })
        .detach();
    }
}
