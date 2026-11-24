use super::*;
use crate::mcp::bridge::{self, Request};
use crate::harness::thread::Entry;
use serde_json::{Value, json};

pub struct PendingWait {
    request: Request,
    until: i64,
}

/// Where a subagent stands, as the parent's tools and the sidebar show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentState {
    Starting,
    Running,
    WaitingForUser,
    Completed,
    Failed,
    Stopped,
}

impl SubagentState {
    pub fn key(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::WaitingForUser => "waiting_for_user",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::WaitingForUser => "Needs you",
            Self::Completed => "Done",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }

    /// A native subagent's state, from the parent's tool call that runs it and whether the parent's turn goes on.
    pub fn of_step(status: crate::harness::thread::ToolStatus, parent_working: bool) -> Self {
        use crate::harness::thread::ToolStatus;
        match status {
            ToolStatus::Pending | ToolStatus::Running if parent_working => Self::Running,
            // A step the parent's interrupted turn left behind is not running any more.
            ToolStatus::Pending | ToolStatus::Running | ToolStatus::Ended => Self::Stopped,
            ToolStatus::Done => Self::Completed,
            ToolStatus::Failed => Self::Failed,
        }
    }

    /// Its turn is over and nothing waits on the user, so the sidebar folds it away.
    pub fn finished(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Stopped)
    }

    fn busy(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}

/// The agent's last message after the user's last one: what the latest turn reported.
fn last_reply(entries: &[Entry]) -> Option<&str> {
    entries.iter().rev().take_while(|entry| !matches!(entry, Entry::User(_))).find_map(|entry| match entry {
        Entry::Agent(text) => Some(text.as_str()),
        _ => None,
    })
}

fn requester(req: &Request) -> &str {
    req.from.strip_prefix("code:").unwrap_or(&req.from)
}

fn message(req: &Request) -> Result<String, String> {
    let text = req.args["prompt"].as_str().unwrap_or_default().trim();
    if text.is_empty() || text.chars().count() > 20_000 {
        return Err("Give a nonempty prompt of at most 20,000 characters.".into());
    }
    Ok(text.to_string())
}

impl Workspace {
    /// The subagents `parent` started, oldest first, as `(id, title)`.
    pub fn subagents_of(&self, parent: &str) -> Vec<(Id, String)> {
        let mut children: Vec<&SessionRecord> =
            self.store.data.sessions.iter().filter(|rec| rec.agent.as_ref().is_some_and(|m| m.parent_session.as_deref() == Some(parent))).collect();
        children.sort_by_key(|rec| rec.created_at);
        children.into_iter().map(|rec| (rec.id.clone(), rec.display_title().to_string())).collect()
    }

    fn owned_subagent(&self, req: &Request) -> Result<SessionRecord, String> {
        let id = req.args["id"].as_str().ok_or("Give the subagent id.")?;
        self.store.session(id)
            .filter(|rec| rec.agent.as_ref().is_some_and(|m| m.parent_session.as_deref() == Some(requester(req))))
            .cloned()
            .ok_or_else(|| "This subagent does not belong to the requesting session.".into())
    }

    /// Where a subagent stands, from its live view, else from how its process last ended.
    pub fn subagent_state(&self, id: &str, cx: &App) -> SubagentState {
        if let Some(view) = self.live_view(id).and_then(|v| v.thread()) {
            let view = view.read(cx);
            return if view.is_loading() {
                SubagentState::Starting
            } else if view.is_waiting() {
                SubagentState::WaitingForUser
            } else if view.is_working() {
                SubagentState::Running
            } else if view.start_error().is_some() || matches!(view.thread.entries.last(), Some(Entry::Notice { error: true, .. })) {
                SubagentState::Failed
            } else {
                SubagentState::Completed
            };
        }
        match self.store.session(id).and_then(|rec| rec.exit_code) {
            Some(code) if code != 0 => SubagentState::Failed,
            _ => SubagentState::Stopped,
        }
    }

    fn subagent_status(&self, req: &Request, cx: &App) -> Result<Value, String> {
        let record = self.owned_subagent(req)?;
        let state = self.subagent_state(&record.id, cx);
        let live = self.live_view(&record.id).and_then(|v| v.thread());
        let report = match live {
            Some(view) => last_reply(&view.read(cx).thread.entries).map(String::from),
            None => last_reply(&crate::harness::thread::load_entries(&record.id)).map(String::from),
        };
        let error = live.and_then(|view| view.read(cx).start_error());
        Ok(json!({"id": record.id, "state": state.key(), "report": report, "error": error}))
    }

    /// When subagent `id`'s turn ends, its parent gets the report as a message: at once when idle, else after its
    /// current turn. A parent blocked in wait_subagent for this child gets it from the wait instead.
    pub(super) fn report_to_parent(&mut self, id: &str, outcome: &str, cx: &mut Context<Self>) {
        let Some(record) = self.store.session(id) else { return };
        let Some(meta) = record.agent.as_ref() else { return };
        let Some(parent) = meta.parent_session.clone() else { return };
        if self.pending_subagent_waits.iter().any(|wait| wait.request.args["id"].as_str() == Some(id)) {
            return;
        }
        let Some(child) = self.live_view(id).and_then(|v| v.thread()) else { return };
        let entries = &child.read(cx).thread.entries;
        let mut body = last_reply(entries).unwrap_or_default().to_string();
        if let Some(Entry::Notice { text, error: true }) = entries.last() {
            body = if body.is_empty() { text.clone() } else { format!("{body}\n\n{text}") };
        }
        let prompt = crate::harness::thread::subagent_report_prompt(id, meta.harness.key(), record.display_title(), outcome, &body);
        if let Some(parent) = self.live_view(&parent).and_then(|v| v.thread()).cloned() {
            parent.update(cx, |view, cx| view.deliver(prompt, cx));
        }
    }

    pub(super) fn defer_subagent_wait(&mut self, req: &Request, cx: &App) -> bool {
        if !self.owned_subagent(req).is_ok_and(|child| self.subagent_state(&child.id, cx).busy()) {
            return false;
        }
        let seconds = req.args["timeout_seconds"].as_i64().unwrap_or(60).clamp(1, 240);
        self.pending_subagent_waits.push(PendingWait { request: req.clone(), until: (now() + seconds).min(req.deadline - 1) });
        true
    }

    pub(super) fn settle_subagent_waits(&mut self, cx: &App) {
        for pending in std::mem::take(&mut self.pending_subagent_waits) {
            if pending.request.deadline <= now() {
                continue;
            }
            let status = self.subagent_status(&pending.request, cx);
            let busy = self.owned_subagent(&pending.request).is_ok_and(|child| self.subagent_state(&child.id, cx).busy());
            if busy && pending.until > now() {
                self.pending_subagent_waits.push(pending);
            } else {
                let ok = status.is_ok();
                bridge::respond(&pending.request.id, ok, status.map(|status| status.to_string()).unwrap_or_else(|error| error));
            }
        }
    }

    pub(super) fn run_subagent_request(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) -> Result<String, String> {
        let parent = self.store.session(requester(req)).cloned().ok_or("The requesting session no longer exists.")?;
        match req.tool.as_str() {
            "spawn_subagent" => {
                if !self.is_running(&parent.id, cx) {
                    return Err("The parent session is no longer running.".into());
                }
                let harness: Harness = serde_json::from_value(req.args["harness"].clone()).map_err(|_| "Choose claude, codex or opencode.")?;
                if harness == Harness::Antigravity {
                    return Err("Shared Antigravity subagents are not enabled yet.".into());
                }
                let settings = &self.store.data.settings;
                let access = settings.subagent_access(harness);
                if !settings.harness_enabled(harness) || !access.on {
                    return Err(format!("{} subagents are disabled in Settings.", harness.name()));
                }
                let parent_read_only = self.live_view(&parent.id).and_then(|view| view.thread()).is_some_and(|view| {
                    matches!(view.read(cx).thread.mode.as_deref(), Some("plan" | "read_only" | "readOnly" | "read-only"))
                });
                let edits = !parent_read_only && access.edits
                    && parent.agent.as_ref().is_none_or(|m| m.parent_session.is_none() || m.subagent_edits);
                let prompt = message(req)?;
                let prefix = format!("{}:", harness.key());
                let offered = settings.aggregated_models();
                let default = offered.iter().find(|model| model.harness == harness && Some(model.id.as_str()) == settings.new_session.model.as_deref())
                    .or_else(|| offered.iter().find(|model| model.harness == harness));
                let model = req.args["model"].as_str().filter(|model| !model.is_empty()).map(|model| model.strip_prefix(&prefix).unwrap_or(model))
                    .or_else(|| default.map(|model| model.id.as_str()).filter(|model| !model.is_empty()));
                if let Some(effort) = req.args["effort"].as_str()
                    && !matches!(effort, "low" | "medium" | "high" | "xhigh" | "max")
                {
                    return Err("Choose low, medium, high, xhigh or max effort.".into());
                }
                if let Some(model) = model
                    && !offered.iter().any(|m| m.harness == harness && m.id == model)
                {
                    return Err("Choose a model listed by list_models for this harness.".into());
                }
                let id = new_id();
                let meta = AgentMeta {
                    harness, parent_session: Some(parent.id.clone()), subagent_edits: edits,
                    chosen_model: model.map(String::from),
                    chosen_effort: req.args["effort"].as_str().map(String::from),
                    ..Default::default()
                };
                let mut start = self.thread_start(&id, &parent.cwd, &meta, None, Some(prompt), Starter::User, cx).ok_or("The subagent could not start.")?;
                for (category, value) in [("model", &meta.chosen_model), ("thought_level", &meta.chosen_effort)] {
                    if let Some(value) = value {
                        start.options.retain(|(existing, _)| *existing != category);
                        start.options.push((category, value.clone()));
                    }
                }
                let record = SessionRecord {
                    id: id.clone(), kind: SessionKind::Agent, title: format!("{} subagent", harness.name()),
                    custom_title: None, auto_title: None, project_id: parent.project_id,
                    cwd: parent.cwd, created_at: now(), last_active: now(), exit_code: None,
                    hidden: false, agent: Some(meta),
                };
                self.start_thread_with_focus(record, start, true, false, window, cx);
                self.subagent_status(&Request { args: json!({"id": id}), ..req.clone() }, cx).map(|status| status.to_string())
            }
            "list_subagents" => {
                let children = self.store.data.sessions.iter()
                    .filter(|rec| rec.agent.as_ref().is_some_and(|m| m.parent_session.as_deref() == Some(parent.id.as_str())))
                    .map(|rec| self.subagent_status(&Request { args: json!({"id": rec.id}), ..req.clone() }, cx))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(json!(children).to_string())
            }
            "wait_subagent" => self.subagent_status(req, cx).map(|status| status.to_string()),
            "send_subagent_message" => {
                let record = self.owned_subagent(req)?;
                let prompt = message(req)?;
                if let Some(view) = self.live_view(&record.id).and_then(|v| v.thread()).cloned() {
                    view.update(cx, |v, cx| v.send(prompt, window, cx));
                } else {
                    let meta = record.agent.as_ref().ok_or("The subagent has no session metadata.")?;
                    let start = self.thread_start(&record.id, &record.cwd, meta, None, Some(prompt), Starter::User, cx).ok_or("The subagent could not resume.")?;
                    self.start_thread_with_focus(record, start, false, false, window, cx);
                }
                Ok("Message queued in the same subagent session.".into())
            }
            "stop_subagent" => {
                let child = self.owned_subagent(req)?;
                self.end_live(&child.id, window, cx);
                Ok("Stopped the subagent and its children; its conversation is kept.".into())
            }
            _ => Err("Unknown subagent operation.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SubagentState::*;

    #[test]
    fn a_native_subagent_runs_only_while_its_parents_turn_does() {
        use super::SubagentState;
        use crate::harness::thread::ToolStatus;
        assert_eq!(SubagentState::of_step(ToolStatus::Running, true), Running);
        assert_eq!(SubagentState::of_step(ToolStatus::Pending, true), Running);
        assert_eq!(SubagentState::of_step(ToolStatus::Running, false), Stopped, "an interrupted turn left it behind");
        assert_eq!(SubagentState::of_step(ToolStatus::Done, false), Completed);
        assert_eq!(SubagentState::of_step(ToolStatus::Failed, true), Failed);
    }

    #[test]
    fn only_subagents_with_nothing_left_to_do_fold_away() {
        for state in [Completed, Failed, Stopped] {
            assert!(state.finished(), "{state:?}");
        }
        for state in [Starting, Running, WaitingForUser] {
            assert!(!state.finished(), "{state:?} still needs watching");
        }
    }
}
