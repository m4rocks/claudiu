//! Continue in fresh session (linked): summarizes an idle active session, starts a new session in the
//! same project and working directory (preserving model, effort, and permission), seeds it with the
//! handoff summary, and links both sessions so the original thread stays intact.

use super::*;
use crate::harness::thread::{Entry, is_hidden_prompt, parse_subagent_report};

/// Maximum byte size of the fallback structured transcript summary.
const FALLBACK_SUMMARY_MAX_BYTES: usize = 6000;
/// Number of trailing entries passed to text generation when summarizing.
const SUMMARY_TRANSCRIPT_ENTRIES: usize = 40;

struct FreshSessionSnapshot {
    harness: Harness,
    model: Option<String>,
    effort: Option<String>,
    permission: Option<Permission>,
    chrome: bool,
    auto_compact_override: Option<bool>,
    source_title: String,
}

impl Workspace {
    /// Start a new linked session in the same project/cwd seeded with a handoff summary of `id`.
    /// Guards active turns and in-flight operations with a toast.
    pub(super) fn continue_in_fresh_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.live_view(id).and_then(SessionView::thread).cloned() else {
            self.toast("Open an active agent session first.", cx);
            return;
        };
        {
            let t = thread.read(cx);
            if t.has_active_turn() {
                self.toast("Wait for the current turn to finish before continuing in a fresh session.", cx);
                return;
            }
            if t.is_read_only_unconfirmed() || t.is_workspace_switching() || self.switching_worktree.contains_key(id) {
                self.toast("Wait for the workspace change to finish before continuing in a fresh session.", cx);
                return;
            }
        }
        if self.compacting.contains_key(id) || self.pending_handoffs.contains_key(id) {
            self.toast("Wait for the current session operation to finish before continuing in a fresh session.", cx);
            return;
        }
        if self.continuing_fresh.contains_key(id) {
            self.toast("Already preparing a fresh linked session…", cx);
            return;
        }
        let Some(rec) = self.store.session(id).cloned() else { return };
        let meta = rec.agent.clone().unwrap_or_default();
        let source_entity = thread.entity_id();
        let source_title = rec.display_title().to_string();

        let (snapshot, transcript, entries) = {
            let t = thread.read(cx);
            let model = t
                .thread
                .option("model")
                .map(|o| o.current.clone())
                .or_else(|| meta.chosen_model.clone())
                .filter(|m| !m.is_empty());
            let effort = t
                .thread
                .option("thought_level")
                .map(|o| o.current.clone())
                .or_else(|| t.pending_effort.clone())
                .or_else(|| meta.chosen_effort.clone())
                .filter(|e| !e.is_empty());
            let permission = t.permission_for_relaunch().or(meta.permission);
            let snapshot = FreshSessionSnapshot {
                harness: t.harness,
                model,
                effort,
                permission,
                chrome: meta.chrome,
                auto_compact_override: meta.auto_compact_override,
                source_title: source_title.clone(),
            };
            (snapshot, t.thread.transcript(SUMMARY_TRANSCRIPT_ENTRIES), t.thread.entries.clone())
        };

        let fallback = build_structured_fallback_summary(&source_title, &entries, FALLBACK_SUMMARY_MAX_BYTES);
        if transcript.trim().is_empty() {
            self.finish_continue_in_fresh_session(id, source_entity, snapshot, fallback, window, cx);
            return;
        }

        let request = uuid::Uuid::new_v4();
        self.continuing_fresh.insert(id.to_string(), request);
        self.toast("Summarizing session for fresh linked thread…", cx);
        cx.notify();

        let gen_model = self.store.data.settings.text_generation_model.clone().unwrap_or_else(|| {
            crate::store::TextGenerationModel {
                harness: snapshot.harness,
                id: snapshot.model.clone().unwrap_or_default(),
            }
        });
        let env = self.harness_model_env(gen_model.harness).ok();
        let system = "Summarize the following coding-session conversation concisely as a handoff brief for a fresh linked session. \
                      Preserve the user's goal, key decisions, modified or relevant file paths, symbol names, errors/blockers, and clear next steps.";
        let session_id = id.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let summary = if let Some(env) = env {
                let timeout = cx.background_executor().timer(std::time::Duration::from_secs(60));
                match crate::harness::text_generation::complete(&gen_model, env, system, &transcript, timeout).await {
                    Ok(text) if !text.trim().is_empty() => text.trim().to_string(),
                    _ => fallback,
                }
            } else {
                fallback
            };
            let _ = this.update_in(cx, |ws, window, cx| {
                if ws.continuing_fresh.get(&session_id) != Some(&request) {
                    return;
                }
                ws.continuing_fresh.remove(&session_id);
                ws.finish_continue_in_fresh_session(&session_id, source_entity, snapshot, summary, window, cx);
            });
        })
        .detach();
    }

    fn finish_continue_in_fresh_session(
        &mut self,
        source_id: &str,
        source_entity: gpui::EntityId,
        snapshot: FreshSessionSnapshot,
        summary: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(thread) = self.live_view(source_id).and_then(SessionView::thread)
            && thread.entity_id() == source_entity
            && thread.read(cx).has_active_turn()
        {
            self.toast("The original session started a new turn; cancelled fresh session handoff.", cx);
            cx.notify();
            return;
        }
        let Some(rec) = self.store.session(source_id).cloned() else { return };
        let cwd = self.resume_cwd(&rec, cx);
        let project_id = rec.project_id.clone().or_else(|| self.store.project_for_cwd(&cwd));
        let new_record_id = new_id();
        let title = fresh_session_title(&snapshot.source_title);
        let meta = AgentMeta {
            chrome: snapshot.chrome,
            harness: snapshot.harness,
            chosen_model: snapshot.model.clone(),
            chosen_effort: snapshot.effort.clone(),
            permission: snapshot.permission,
            auto_compact_override: snapshot.auto_compact_override,
            ..Default::default()
        };
        let prompt = fresh_session_seed_prompt(&snapshot.source_title, &summary);
        let Some(mut start) = self.thread_start(
            &new_record_id,
            &cwd,
            &meta,
            Some(title.clone()),
            Some(prompt),
            Starter::User,
            cx,
        ) else {
            return;
        };
        if let Some(permission) = snapshot.permission {
            start.permission = Some(permission);
        }
        if let Some(model_id) = snapshot.model {
            start.options.retain(|(category, _)| *category != "model");
            start.options.push(("model", model_id));
        }
        if let Some(effort) = snapshot.effort {
            start.options.retain(|(category, _)| *category != "thought_level");
            start.options.push(("thought_level", effort));
        }
        start.prior_entries = vec![Entry::notice(
            format!("Continued from \"{}\" (linked). Handoff summary loaded.", snapshot.source_title),
            false,
        )];

        let new_record = SessionRecord {
            id: new_record_id.clone(),
            kind: SessionKind::Agent,
            title,
            custom_title: None,
            auto_title: None,
            project_id,
            cwd,
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            agent: Some(meta),
        };
        self.start_thread(new_record, start, true, window, cx);
        self.link(source_id, &new_record_id);
        self.store.mark_dirty();
        self.store.save_if_dirty();
        cx.notify();
    }
}

/// Build a concise title for the fresh linked session based on the previous session's display title.
pub(crate) fn fresh_session_title(source_title: &str) -> String {
    let trimmed = source_title.trim();
    if trimmed.is_empty() || trimmed == "New session" {
        return "New session".to_string();
    }
    if trimmed.ends_with("(cont.)") {
        trimmed.to_string()
    } else {
        format!("{trimmed} (cont.)")
    }
}

/// Build the hidden seed prompt injected as the initial context of the fresh linked session.
pub(crate) fn fresh_session_seed_prompt(source_title: &str, summary: &str) -> String {
    format!(
        "[m4code notice] You are continuing work in a fresh linked session from \"{source_title}\". \
         Retain the handoff summary below for the next user request. Do not write an acknowledgement or take further action.\n\n\
         ## Handoff summary\n{summary}"
    )
}

/// Deterministic structured fallback summary when headless text generation is unavailable or fails.
pub(crate) fn build_structured_fallback_summary(title: &str, entries: &[Entry], max_bytes: usize) -> String {
    let mut user_prompts = Vec::new();
    let mut tool_titles = Vec::new();
    let mut last_agent: Option<&str> = None;

    for entry in entries {
        match entry {
            Entry::User(text) if !is_hidden_prompt(text) && parse_subagent_report(text).is_none() => {
                let preview = text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !preview.is_empty() {
                    user_prompts.push(preview);
                }
            }
            Entry::Tool(call) if !call.title.trim().is_empty() => {
                let item = format!("[{}] {}", call.kind, call.title.trim());
                if tool_titles.last().is_none_or(|prev| prev != &item) {
                    tool_titles.push(item);
                }
            }
            Entry::Agent(text) if !text.trim().is_empty() => {
                last_agent = Some(text.trim());
            }
            _ => {}
        }
    }

    if user_prompts.is_empty() && tool_titles.is_empty() && last_agent.is_none() {
        return format!("Continuing from linked session \"{title}\" (no prior messages).");
    }

    let mut out = format!("### Previous session\n{title}\n");
    if !user_prompts.is_empty() {
        out.push_str("\n### User requests\n");
        let skip = user_prompts.len().saturating_sub(8);
        for (ix, prompt) in user_prompts.iter().enumerate().skip(skip) {
            out.push_str(&format!("{}. {}\n", ix + 1, prompt));
        }
    }
    if !tool_titles.is_empty() {
        out.push_str("\n### Key tool activity\n");
        let skip = tool_titles.len().saturating_sub(12);
        for item in tool_titles.iter().skip(skip) {
            out.push_str(&format!("- {item}\n"));
        }
    }
    if let Some(reply) = last_agent {
        out.push_str("\n### Latest assistant response\n");
        out.push_str(reply);
        out.push('\n');
    }

    truncate_utf8(&out, max_bytes)
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.trim_end().to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::thread::ToolCall;

    #[test]
    fn fresh_session_title_appends_cont_once() {
        assert_eq!(fresh_session_title("Fix login"), "Fix login (cont.)");
        assert_eq!(fresh_session_title("Fix login (cont.)"), "Fix login (cont.)");
        assert_eq!(fresh_session_title("New session"), "New session");
        assert_eq!(fresh_session_title("   "), "New session");
    }

    #[test]
    fn fresh_session_seed_prompt_is_hidden_and_contains_summary() {
        let prompt = fresh_session_seed_prompt("Refactor router", "Moved routes to src/router.rs");
        assert!(is_hidden_prompt(&prompt), "seed prompt must stay hidden from visible user bubbles");
        assert!(prompt.contains("Refactor router"));
        assert!(prompt.contains("Moved routes to src/router.rs"));
    }

    #[test]
    fn structured_fallback_summary_handles_empty_and_populated_threads() {
        let empty = build_structured_fallback_summary("Empty chat", &[], 2000);
        assert!(empty.contains("Empty chat"));
        assert!(empty.contains("no prior messages"));

        let entries = vec![
            Entry::User("[m4code notice] hidden".into()),
            Entry::User("Add turn outline\nwith clickable rows".into()),
            Entry::Thought("internal reasoning".into()),
            Entry::Tool(ToolCall {
                kind: "edit".into(),
                title: "src/thread/outline.rs".into(),
                ..Default::default()
            }),
            Entry::Agent("Implemented the turn outline module and wired it up.".into()),
        ];
        let summary = build_structured_fallback_summary("Turn outline work", &entries, 4000);
        assert!(summary.contains("Turn outline work"));
        assert!(summary.contains("1. Add turn outline"));
        assert!(!summary.contains("[m4code notice]"));
        assert!(!summary.contains("internal reasoning"));
        assert!(summary.contains("[edit] src/thread/outline.rs"));
        assert!(summary.contains("Implemented the turn outline module"));
    }

    #[test]
    fn structured_fallback_summary_truncates_on_utf8_boundary() {
        let entries = vec![Entry::Agent("é".repeat(500))];
        let summary = build_structured_fallback_summary("Unicode", &entries, 80);
        assert!(summary.ends_with('…'));
        assert!(summary.len() <= 84);
    }
}
