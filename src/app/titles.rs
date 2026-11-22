//! "Regenerate title": derive a concise title from the session's conversation without depending on a specific harness.

use super::*;

impl Workspace {
    pub(super) fn regen_title(&mut self, id: &str, cx: &mut Context<Self>) {
        let first_prompt = self
            .live
            .iter()
            .find(|l| l.id == id)
            .and_then(|l| l.view.thread())
            .and_then(|t| t.read(cx).thread.sent_texts().into_iter().next());
        let Some(raw) = first_prompt else {
            return self.toast("Nothing to title yet.", false, cx);
        };
        let title: String = raw
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .trim_matches(['"', '\'', '`', '.'])
            .chars()
            .take(60)
            .collect();
        if title.is_empty() {
            return self.toast("Nothing to title yet.", false, cx);
        }
        if let Some(rec) = self.store.session_mut(id) {
            rec.custom_title = Some(title.clone());
        }
        self.toast(format!("Renamed to “{title}”"), false, cx);
    }
}
