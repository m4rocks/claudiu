use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{Context, Window};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ThreadEvent, ThreadView};

#[derive(Deserialize, Serialize)]
pub(super) struct Queued {
    pub id: Uuid,
    #[serde(skip)]
    pub order: u64,
    #[serde(skip)]
    pub startup: bool,
    pub text: String,
    pub compact: bool,
    // Reports and initial prompts have their own owners and are not restored from the user queue.
    pub by_user: bool,
    pub ahead: bool,
    pub shown: bool,
}

impl Queued {
    pub fn new(text: String, by_user: bool) -> Self {
        Self { id: Uuid::new_v4(), order: next_order(), startup: false, text, compact: false, by_user, ahead: false, shown: false }
    }
}

pub(super) fn next_order() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub(super) struct Edit {
    pub id: Uuid,
    pub draft: String,
}

fn path(record: &str) -> PathBuf {
    crate::datadir::data_dir().join("queues").join(format!("{}.json", crate::datadir::safe_file_stem(record)))
}

pub(super) fn load(record: &str) -> VecDeque<Queued> {
    if record.is_empty() {
        return VecDeque::new();
    }
    load_from(&path(record))
}

fn load_from(path: &std::path::Path) -> VecDeque<Queued> {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

pub(super) fn save<'a>(record: &str, messages: impl Iterator<Item = &'a Queued>) -> std::io::Result<()> {
    if record.is_empty() {
        return Ok(());
    }
    save_at(&path(record), messages)
}

fn save_at<'a>(path: &std::path::Path, messages: impl Iterator<Item = &'a Queued>) -> std::io::Result<()> {
    let messages = messages.filter(|item| item.by_user).collect::<Vec<_>>();
    if messages.is_empty() {
        return match std::fs::remove_file(path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
    }
    crate::datadir::write_atomic(path, &serde_json::to_string(&messages)?)
}

impl ThreadView {
    pub(super) fn queue_changed(&mut self) {
        let mut waiting = self.queue.iter().collect::<Vec<_>>();
        for item in &self.steering {
            let at = waiting.iter().position(|queued| !queued.ahead && queued.order > item.order)
                .unwrap_or(waiting.len());
            waiting.insert(at, item);
        }
        let pending = self.preparing.iter().chain(waiting);
        let error = save(&self.record_id, pending).err();
        if let Some(error) = &error && !self.queue_save_error {
            let at = self.thread.notice(format!("Could not save queued messages: {error}"), true);
            self.remeasure(at);
        }
        self.queue_save_error = error.is_some();
    }

    pub(super) fn unread_steers(&self) -> impl Iterator<Item = &str> {
        self.steering.iter().map(|item| item.text.as_str())
    }

    pub(super) fn waiting_messages(&self) -> impl Iterator<Item = (Uuid, &str)> {
        self.queue.iter().filter(|item| item.by_user).map(|item| (item.id, item.text.as_str()))
    }

    pub(super) fn unqueue(&mut self, id: Uuid, cx: &mut Context<Self>) -> Option<Queued> {
        let at = self.queue.iter().position(|item| item.by_user && item.id == id)?;
        let item = self.queue.remove(at)?;
        cx.notify();
        Some(item)
    }

    pub(super) fn edit_queued(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if !self.questions.is_empty() || self.queue_edit.is_some() {
            return;
        }
        if let Some(item) = self.queue.iter().find(|item| item.by_user && item.id == id) {
            let text = item.text.clone();
            self.queue_edit = Some(Edit { id, draft: self.draft(cx) });
            self.set_draft(&text, window, cx);
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    pub(super) fn finish_queue_edit(&mut self, save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.draft(cx);
        if save && (!crate::panels::composer::can_send(&text) || !self.questions.is_empty()) {
            return;
        }
        let Some(edit) = self.queue_edit.take() else { return };
        if save && let Some(item) = self.queue.iter_mut().find(|item| item.id == edit.id) {
            item.text = text.trim().to_string();
            item.shown = false;
            self.queue_changed();
        }
        self.set_draft(&edit.draft, window, cx);
        self.focus_composer(window, cx);
        self.send_next(cx);
        cx.notify();
    }

    pub(super) fn send_queued_now(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.interrupting || !self.questions.is_empty() || self.queue_edit.as_ref().is_some_and(|edit| edit.id == id) {
            return;
        }
        let Some(mut item) = self.unqueue(id, cx) else { return };
        if self.can_steer(&item.text, crate::store::FollowUp::Steer) {
            self.steer(item, cx);
        } else {
            item.ahead = true;
            self.queue.push_front(item);
            self.queue_held = false;
            self.queue_changed();
            self.send_next(cx);
            cx.notify();
        }
    }

    pub(crate) fn restore_queue_after_handoff(&mut self, cx: &mut Context<Self>) {
        self.queue.retain(|item| !item.by_user);
        let saved = load(&self.record_id);
        self.queue_held |= !saved.is_empty();
        self.queue.extend(saved.into_iter().map(|mut item| {
            item.order = next_order();
            item.ahead = false;
            item.shown = false;
            item
        }));
        cx.notify();
    }

    pub(super) fn preserve_handoff_message(&mut self) {
        if let Some((_, _, text, false)) = self.handoff_after_turn.take() {
            self.queue.push_back(Queued::new(text, true));
            self.queue_changed();
        }
    }

    pub(super) fn resume_queue(&mut self, cx: &mut Context<Self>) {
        self.queue_held = false;
        if !self.working && self.steering.is_empty()
            && let Some((target, effort, message, by_agent)) = self.handoff_after_turn.take()
        {
            let model = (!target.id.is_empty()).then_some(target.id);
            cx.emit(ThreadEvent::Handoff { to: target.harness, model, message, effort, by_agent });
        } else {
            self.send_next(cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{Queued, load_from, save_at};

    #[test]
    fn waiting_user_messages_survive_restart_in_dispatch_order() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("queue.json");
        let first = Queued::new("first".into(), true);
        let last = Queued::new("last".into(), true);
        let report = Queued::new("report".into(), false);
        save_at(&path, [&first, &report, &last].into_iter()).unwrap();
        let restored = load_from(&path);
        assert_eq!(restored.iter().map(|q| q.text.as_str()).collect::<Vec<_>>(), ["first", "last"]);
        assert_eq!(restored[0].id, first.id);
        save_at(&path, std::iter::empty()).unwrap();
        assert!(load_from(&path).is_empty());
    }
}
