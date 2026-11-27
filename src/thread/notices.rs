//! Shared lifecycle for chat operations. Producers own an event, never a second transcript message.
use gpui::Context;

use super::{ThreadEvent, ThreadView};
use crate::harness::{CatalogModel, thread::NoticeKind};

impl ThreadView {
    pub(super) fn begin_compaction(&mut self, cx: &mut Context<Self>) {
        self.compaction.started();
        let row = self.thread.begin_notice(NoticeKind::Compaction, "Compacting context…");
        self.remeasure(row);
        self.persist_entries();
        cx.notify();
    }

    pub(crate) fn finish_compaction(&mut self, completed: bool, cx: &mut Context<Self>) {
        self.compaction.running = false;
        let row = self.thread.finish_notice(NoticeKind::Compaction,
            if completed { "Context compacted." } else { "Context compaction did not finish." }, !completed);
        self.remeasure(row);
        self.persist_entries();
        cx.notify();
    }

    pub(crate) fn cancel_compaction(&mut self, cx: &mut Context<Self>) {
        self.finish_compaction(false, cx);
        self.send_next(cx);
    }

    pub(crate) fn fail_handoff(&mut self, text: String, cx: &mut Context<Self>) {
        if self.thread.active_notice(NoticeKind::Handoff).is_none() {
            self.thread.begin_notice(NoticeKind::Handoff, text.clone());
        }
        let row = self.thread.finish_notice(NoticeKind::Handoff, text, true);
        self.remeasure(row);
        self.persist_entries();
        cx.notify();
    }

    pub(super) fn request_usage_handoff(&mut self, target: CatalogModel, cx: &mut Context<Self>) {
        // Some providers return their quota failure as an assistant message as well as a failed result.
        if matches!(self.thread.entries.last(), Some(super::Entry::Agent(text))
            if crate::harness::limits::looks_like_usage_limit_error(text))
        {
            let row = self.thread.entries.len() - 1;
            self.thread.entries.pop();
            self.markdown.remove(&row);
            self.agent_cache.remove(&row);
            self.remeasure(row);
        }
        // Both a live usage reading and the turn result may report the same exhausted limit.
        if self.thread.active_notice(NoticeKind::Handoff).is_some() {
            if !self.has_active_turn() && let Some((target, _, _, _)) = self.handoff_after_turn.take() {
                self.emit_usage_handoff(target, cx);
            }
            return;
        }
        let row = self.thread.begin_notice(NoticeKind::Handoff,
            format!("Usage limit reached on {} — handing over to {}…", self.harness.name(), target.name));
        self.remeasure(row);
        if self.has_active_turn() {
            self.schedule_handoff(target, true, cx);
        } else {
            self.emit_usage_handoff(target, cx);
        }
        cx.notify();
    }

    fn emit_usage_handoff(&self, target: CatalogModel, cx: &mut Context<Self>) {
        cx.emit(ThreadEvent::Handoff {
            to: target.harness, model: (!target.id.is_empty()).then_some(target.id),
            message: String::new(), effort: None, by_agent: true,
        });
    }
}

#[cfg(all(test, feature = "screenshots"))]
impl ThreadView {
    pub(crate) fn screenshot_events(&mut self, cx: &mut Context<Self>) {
        use super::{Entry, ProviderShell, Question, QuestionOption, QuestionReply, Queued};
        use crate::harness::thread::{PlanItem, ToolCall, ToolContent, ToolDetails, ToolStatus, prepend_quote, subagent_report_prompt};
        use std::{collections::{BTreeSet, VecDeque}, time::{Duration, Instant}};
        self.working = true;
        self.turn_started = Some(Instant::now() - Duration::from_secs(42));
        self.detail = true;
        self.compaction.running = false;
        self.thread.entries = vec![
            Entry::Agent("## Chat component gallery\nReal chat components, with example events shown together for comparison.".into()),
            Entry::User(prepend_quote("Attached files:\n- C:/project/design.png", "Review the attached design.")),
            Entry::Agent("A normal reply supports **Markdown**, `code`, links and quoted context.\n\n> A standard Markdown quote keeps its left bar.".into()),
            Entry::Thought("Check the event lifecycle, then verify rendering.".into()),
            Entry::Tool(ToolCall { id: "gallery-read".into(), title: "Read src/thread/notices.rs".into(), kind: "read".into(), status: ToolStatus::Done, ..Default::default() }),
            Entry::Tool(ToolCall { id: "gallery-edit".into(), title: "Update the shared notice".into(), kind: "edit".into(), status: ToolStatus::Done,
                content: vec![ToolContent::Diff { path: self.cwd.join("src/main.rs"), old: Some("old_notice();\n".into()), new: "notice();\n".into() }], ..Default::default() }),
            Entry::Tool(ToolCall { id: "gallery-test".into(), title: "Run tests".into(), kind: "execute".into(), status: ToolStatus::Done,
                details: Box::new(ToolDetails { command: Some("cargo test".into()), stdout: Some("All notice lifecycle checks passed.".into()), exit_code: Some(0), ..Default::default() }), ..Default::default() }),
            Entry::Tool(ToolCall { id: "gallery-failed".into(), title: "Failed tool example".into(), kind: "execute".into(), status: ToolStatus::Failed, ..Default::default() }),
            Entry::Tool(ToolCall { id: "gallery-running".into(), title: "Running tool example".into(), kind: "search".into(), status: ToolStatus::Running, ..Default::default() }),
            Entry::User(subagent_report_prompt("gallery-agent", "codex", "Independent review", "completed", "The notice component is shared by every chat notice.")),
            Entry::Agent("### Session notices\nThe same surface is used for progress, completion, warnings, errors and local actions.".into()),
        ];
        for text in [
            "Handing over to Codex\u{2026}", "Switched to Codex",
            "Compacting context\u{2026}", "Context compacted.",
            "Approaching 7-day usage limit (91%) \u{2014} notified the model.",
            "Interrupted", "The reply reached its length limit.",
            "Restored files to checkpoint", super::FORK_SEED, super::WORKTREE_SEED,
        ] { self.thread.notice(text, false); }
        self.thread.notice("Could not save queued messages: example storage error.", true);
        let row = self.thread.entries.len();
        self.thread.entries.push(Entry::User(prepend_quote("Please retry this request with higher thinking:", "Review the notice lifecycle.")));
        self.record_user_notice(row, super::user_actions::UserNotice::ResentHigherThinking { effort: "High".into() }, cx);
        self.thread.entries.push(Entry::User(prepend_quote("Which layout should we use?", "The compact bordered notice.")));
        self.usage_limit = Some("Usage limit reached. Resets in 5m.".into());
        self.fallback_model = Some(CatalogModel { harness: crate::harness::Harness::Codex, id: "gpt".into(), name: "Codex".into() });
        self.pending_options.insert("model".into(), (1, "gpt".into()));
        self.error = Some("Sign in to reconnect this session.".into());
        self.authentication_required = true;
        self.stalled = true;
        self.thread.plan = vec![PlanItem { text: "Centralize notice rendering".into(), status: "completed".into() }, PlanItem { text: "Verify the event gallery".into(), status: "in_progress".into() }];
        self.provider_shells = vec![
            ProviderShell { id: "gallery-shell".into(), label: "Background build".into(), stopping: false },
            ProviderShell { id: "gallery-dev".into(), label: "npm run dev".into(), stopping: true },
        ];
        self.tray = Some(super::dock::Tray::Background);
        self.queue = VecDeque::from([Queued::new("Follow up after this turn.".into(), true)]);
        self.questions.push_back(Question {
            id: uuid::Uuid::new_v4(), reply: QuestionReply::Bridge("gallery-question".into()),
            title: "Question / permission form".into(), options: vec![
                QuestionOption { id: "allow".into(), name: "Use the shared notice (Recommended)".into(), description: Some("One surface for every notice.".into()), kind: "allow_once".into() },
                QuestionOption { id: "deny".into(), name: "Cancel".into(), description: None, kind: "reject_once".into() },
            ], highlight: 0, multi_select: false, selected: BTreeSet::new(), takes_own_answer: true,
        });
        self.remeasure(0);
        for row in 0..self.thread.entries.len() { self.sync_markdown(row, cx); }
        self.list.set_follow_mode(gpui::FollowMode::Normal);
        self.list.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: gpui::px(0.0) });
        cx.notify();
    }
}
