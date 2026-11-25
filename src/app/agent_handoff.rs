//! Dynamic in-chat handoff between harnesses: switching the provider in the chat composer and sending a message
//! seamlessly transitions the same session to the new harness, preserving prior conversation history visually and
//! sending the transcript + the user's new message as the first turn.

use super::*;
use crate::harness::thread::Entry;

/// Entries read from the end before the cut, so a long session is not turned into text whole.
const LAST_ENTRIES: usize = 400;

pub struct Pending {
    source: Live,
    pub(super) meta: AgentMeta,
    draft: Option<String>,
    reconnect: bool,
}

impl Workspace {
    /// Switch session `id` in place to `to` (with an optional specific model & effort), carrying over the conversation
    /// transcript and sending `message`: the user's, or with `by_agent` what the agent said to hand over.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn dynamic_handoff(&mut self, id: &str, to: Harness, model: Option<String>, message: String, by_agent: bool, effort: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.live_view(id).and_then(|v| v.thread()).cloned() else { return };
        let switching_worktree = self.switching_worktree.contains_key(id);
        if switching_worktree || self.pending_handoffs.contains_key(id) {
            if !by_agent {
                thread.update(cx, |v, cx| v.paste_text(&message, window, cx));
            }
            let message = if switching_worktree { "Wait for the worktree switch to finish." } else { "A harness handoff is already starting." };
            self.toast(message, cx);
            return;
        }
        let Some(mut rec) = self.store.session(id).cloned() else { return };
        if to == Harness::Antigravity && rec.agent.as_ref().is_some_and(|m| m.parent_session.is_some()) {
            self.toast("Shared Antigravity subagents are not enabled yet.", cx);
            return;
        }
        let cwd = rec.cwd.clone();
        let draft = (!by_agent).then(|| message.clone());
        let mut meta = rec.agent.clone().unwrap_or_default();
        let target = meta.provider_sessions.get(to.key()).cloned();
        let (from_name, transcript, mut prior_entries, count, permission) = {
            let t = thread.read(cx);
            (t.harness.name(), t.thread.transcript_after(target.as_ref().map_or(0, |s| s.after_entry), LAST_ENTRIES), t.thread.entries.clone(), t.thread.entries.len(), t.handoff_permission())
        };
        if !meta.session_id.is_empty() {
            meta.provider_sessions.insert(meta.harness.key().into(), crate::store::ProviderSession {
                session_id: meta.session_id.clone(), model: meta.chosen_model.clone(), effort: meta.chosen_effort.clone(),
                after_entry: count,
            });
        }
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
            message.clone()
        } else {
            crate::harness::thread::handoff_prompt(from_name, &transcript, &message)
        };
        meta.harness = to;
        meta.session_id = target.as_ref().map_or_else(String::new, |s| s.session_id.clone());
        meta.chosen_model = model.clone().or_else(|| target.as_ref().and_then(|s| s.model.clone()));
        meta.chosen_effort = effort.clone().or_else(|| target.as_ref().and_then(|s| s.effort.clone()));
        meta.transcript = None;
        meta.external = ExternalState::Unknown;
        meta.context_tokens = None;
        meta.context_fraction = None;
        meta.context_window = None;
        meta.cache_ttl = None;
        meta.model = None;
        meta.managed_compaction = true;
        rec.agent = Some(meta.clone());
        let Some(mut start) = self.thread_start(&rec.id, &cwd, &meta, rec.chosen_title().map(String::from), Some(prompt), Starter::Handoff, cx) else {
            if let Some(draft) = draft {
                thread.update(cx, |v, cx| v.paste_text(&draft, window, cx));
            }
            return;
        };
        if meta.parent_session.is_none() && permission.is_some() {
            start.permission = permission;
        }
        start.prior_entries = prior_entries;
        if let Some(model_id) = model.filter(|m| !m.is_empty()) {
            start.options.retain(|(cat, _)| *cat != "model");
            start.options.push(("model", model_id));
        }
        if let Some(level) = effort {
            start.options.retain(|(cat, _)| *cat != "thought_level");
            start.options.push(("thought_level", level));
        }
        let Some(index) = self.live.iter().position(|live| live.id == id) else { return };
        let composer = thread.read(cx).composer_state(cx);
        let source = self.live.remove(index);
        self.pending_handoffs.insert(id.into(), Pending { source, meta, draft, reconnect: false });
        let shell = self.shells.remove(id);
        self.start_thread(rec, start, false, window, cx);
        if let Some(shell) = shell {
            self.shells.insert(id.into(), shell);
        }
        if let Some(target) = self.live_view(id).and_then(|view| view.thread()).cloned() {
            target.update(cx, |target, cx| target.restore_composer(composer, window, cx));
        }
    }

    pub(super) fn reconnect_antigravity_permission(&mut self, id: &str, permission: Permission, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_handoffs.contains_key(id) { return; }
        if self.switching_worktree.contains_key(id) {
            self.toast("Wait for the worktree switch to finish.", cx);
            return;
        }
        let Some(rec) = self.store.session(id).cloned() else { return };
        let Some(meta) = rec.agent.clone() else { return };
        let Some(index) = self.live.iter().position(|live| live.id == id) else { return };
        let Some(source) = self.live[index].view.thread().cloned() else { return };
        let Some(mut start) = self.thread_start(&rec.id, &rec.cwd, &meta, rec.chosen_title().map(String::from), None, Starter::Handoff, cx) else { return };
        start.permission = Some(permission);
        start.prior_entries = source.read(cx).thread.entries.clone();

        let previous = self.live.remove(index);
        self.pending_handoffs.insert(id.into(), Pending { source: previous, meta, draft: None, reconnect: true });
        let shell = self.shells.remove(id);
        let foreground = self.active_id().is_some_and(|active| active == id);
        self.start_thread_with_focus(rec, start, false, foreground, window, cx);
        if let Some(shell) = shell {
            self.shells.insert(id.into(), shell);
        }
        if let Some(target) = self.live_view(id).and_then(|view| view.thread()).cloned() {
            let composer = source.read(cx).composer_state(cx);
            target.update(cx, |target, cx| target.restore_composer(composer, window, cx));
        }
    }

    pub(super) fn commit_handoff(&mut self, id: &str, session_id: &str, cx: &mut Context<Self>) {
        let Some(mut pending) = self.pending_handoffs.remove(id) else { return };
        pending.meta.session_id = session_id.to_string();
        if let Some(live) = self.live.iter().find(|live| live.id == id)
            && let Some(view) = live.view.thread()
        {
            let view = view.read(cx);
            if let Some(model) = view.thread.option("model") { pending.meta.chosen_model = Some(model.current.clone()); }
            if let Some(effort) = view.thread.option("thought_level") { pending.meta.chosen_effort = Some(effort.current.clone()); }
        }
        if let Some(record) = self.store.session_mut(id) {
            record.agent = Some(pending.meta);
        }
        if let Some(source) = pending.source.view.thread() {
            source.update(cx, |view, _| view.relinquish_record());
        }
        self.store.mark_dirty();
        cx.notify();
    }

    pub(super) fn rollback_handoff(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(pending) = self.pending_handoffs.remove(id) else { return false };
        let error = self.live_view(id).and_then(|view| view.start_error(cx)).unwrap_or_else(|| "The replacement session stopped.".into());
        let target = self.live_view(id).and_then(|view| view.thread()).cloned();
        if let Some(target) = &target {
            target.update(cx, |view, _| view.relinquish_record());
        }
        self.live.retain(|live| live.id != id);
        if let Some(source) = pending.source.view.thread() {
            source.update(cx, |view, cx| {
                view.restore_queue_after_handoff(cx);
                if let Some(target) = &target {
                    let composer = target.read(cx).composer_state(cx);
                    view.restore_composer(composer, window, cx);
                }
                if let Some(draft) = pending.draft {
                    view.paste_text(&draft, window, cx);
                }
            });
        }
        self.live.push(pending.source);
        let action = if pending.reconnect { "Permission change" } else { "Handoff" };
        self.toast(format!("{action} failed. The original session is still open. {error}"), cx);
        if self.active_id().is_some_and(|active| active == id) {
            self.focus_active(window, cx);
        }
        true
    }

    pub(super) fn cancel_pending_handoff(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_handoffs.remove(id) {
            if let Some(target) = self.live_view(id).and_then(|view| view.thread()) {
                target.update(cx, |view, _| view.relinquish_record());
            }
            if let Some(source) = pending.source.view.thread() {
                source.update(cx, |view, _| view.stop());
            }
        }
    }

    /// Built-in `/clear`: erase the current conversation and relaunch a fresh ACP session on the same record.
    pub(super) fn clear_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = crate::harness::thread::clear_entries(id) {
            self.toast(format!("Could not clear the local conversation: {error}"), cx);
            return;
        }
        if let Some(thread) = self.live_view(id).and_then(|view| view.thread()).cloned() {
            thread.update(cx, |view, _| view.relinquish_record());
        }
        crate::thread::clear_turn_reviews(id, cx.background_executor());
        if let Some(meta) = self.store.session_mut(id).and_then(|r| r.agent.as_mut()) {
            meta.session_id.clear();
            meta.provider_sessions.clear();
            meta.transcript = None;
            meta.context_tokens = None;
            meta.context_fraction = None;
            meta.context_window = None;
            meta.cache_ttl = None;
            self.store.mark_dirty();
        }
        let shell = self.shells.remove(id);
        self.relaunch_empty(id, None, window, cx);
        if let Some(shell) = shell {
            self.shells.insert(id.into(), shell);
        }
    }

    /// Built-in `/compact` fallback for harnesses without native compaction: summarize the transcript via text
    /// generation (falling back to a condensed tail of the transcript if unavailable) and restart the session in place
    /// seeded with the compacted context.
    pub(super) fn compact_session(&mut self, id: &str, transcript: String, instructions: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rec) = self.store.session(id).cloned() else { return };
        let meta = rec.agent.clone().unwrap_or_default();
        let gen_model = self.store.data.settings.text_generation_model.clone().unwrap_or_else(|| {
            crate::store::TextGenerationModel {
                harness: meta.harness,
                id: meta.chosen_model.clone().unwrap_or_default(),
            }
        });
        let env = self.harness_model_env(gen_model.harness).ok();
        let system = match instructions {
            Some(ref extra) if !extra.trim().is_empty() => format!(
                "Summarize the following coding-session conversation concisely so work can continue in a fresh context window. \
                 Preserve key decisions, file paths, symbol names, errors, and open next steps. Focus especially on: {extra}"
            ),
            _ => "Summarize the following coding-session conversation concisely so work can continue in a fresh context window. \
                  Preserve key decisions, file paths, symbol names, errors, and open next steps.".to_string(),
        };
        let session_id = id.to_string();
        let fallback = condensed_transcript(&transcript, 6000);
        cx.spawn_in(window, async move |this, cx| {
            let summary = if let Some(env) = env {
                let timeout = cx.background_executor().timer(std::time::Duration::from_secs(60));
                match crate::harness::text_generation::complete(&gen_model, env, &system, &transcript, timeout).await {
                    Ok(text) if !text.trim().is_empty() => text.trim().to_string(),
                    _ => fallback,
                }
            } else {
                fallback
            };
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.finish_compact_session(&session_id, summary, window, cx);
            });
        })
        .detach();
    }

    fn finish_compact_session(&mut self, id: &str, summary: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut rec) = self.store.session(id).cloned() else { return };
        let Some(thread) = self.live_view(id).and_then(|v| v.thread()).cloned() else { return };
        let mut prior_entries = thread.read(cx).thread.entries.clone();
        prior_entries.push(Entry::Notice { text: "Context compacted.".into(), error: false });
        let mut meta = rec.agent.clone().unwrap_or_default();
        meta.session_id.clear();
        meta.provider_sessions.clear();
        meta.transcript = None;
        meta.context_tokens = None;
        meta.context_fraction = None;
        meta.context_window = None;
        meta.cache_ttl = None;
        rec.agent = Some(meta.clone());
        if let Some(m) = self.store.session_mut(id).and_then(|r| r.agent.as_mut()) {
            *m = meta.clone();
            self.store.mark_dirty();
        }
        let cwd = self.resume_cwd(&rec, cx);
        let prompt = format!(
            "The previous conversation context was compacted into the summary below. \
             Acknowledge briefly that you have the compacted context and are ready to continue.\n\n\
             ## Compacted context\n{summary}"
        );
        let Some(mut start) = self.thread_start(&rec.id, &cwd, &meta, rec.chosen_title().map(String::from), Some(prompt), Starter::User, cx) else {
            return;
        };
        start.prior_entries = prior_entries;
        let shell = self.shells.remove(id);
        self.start_thread(rec, start, false, window, cx);
        if let Some(shell) = shell {
            self.shells.insert(id.into(), shell);
        }
    }
}

/// Fallback condensation when headless text generation isn't available: keeps the trailing `max_bytes` on a UTF-8 boundary.
fn condensed_transcript(transcript: &str, max_bytes: usize) -> String {
    if transcript.len() <= max_bytes {
        return transcript.to_string();
    }
    let mut start = transcript.len() - max_bytes;
    while start < transcript.len() && !transcript.is_char_boundary(start) {
        start += 1;
    }
    format!("…\n{}", &transcript[start..])
}
