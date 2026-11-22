use super::*;
use crate::claude::bridge::{self, Request};
use crate::harness::thread::Entry;
use serde_json::{Value, json};

pub struct PendingWait {
    request: Request,
    until: i64,
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
            self.store.data.sessions.iter().filter(|rec| rec.claude.as_ref().is_some_and(|m| m.parent_session.as_deref() == Some(parent))).collect();
        children.sort_by_key(|rec| rec.created_at);
        children.into_iter().map(|rec| (rec.id.clone(), rec.display_title().to_string())).collect()
    }

    fn owned_subagent(&self, req: &Request) -> Result<SessionRecord, String> {
        let id = req.args["id"].as_str().ok_or("Give the subagent id.")?;
        self.store.session(id)
            .filter(|rec| rec.claude.as_ref().is_some_and(|m| m.parent_session.as_deref() == Some(requester(req))))
            .cloned()
            .ok_or_else(|| "This subagent does not belong to the requesting session.".into())
    }

    fn subagent_status(&self, req: &Request, cx: &App) -> Result<Value, String> {
        let record = self.owned_subagent(req)?;
        if let Some(view) = self.live_view(&record.id).and_then(|v| v.thread()) {
            let view = view.read(cx);
            let state = if view.is_loading() { "starting" } else if view.is_waiting() { "waiting_for_user" } else if view.is_working() { "running" } else { "completed" };
            let report = view.thread.entries.iter().rev().find_map(|entry| match entry { Entry::Agent(text) => Some(text.clone()), _ => None });
            return Ok(json!({"id": record.id, "state": state, "report": report, "error": view.start_error()}));
        }
        let state = if record.exit_code.is_some_and(|code| code != 0) { "failed" } else { "stopped" };
        let entries = crate::harness::thread::load_entries(&record.id);
        let report = entries.iter().rev().find_map(|entry| match entry {
            Entry::Agent(text) => Some(text.as_str()),
            _ => None,
        });
        Ok(json!({"id": record.id, "state": state, "report": report, "error": null}))
    }

    pub(super) fn defer_subagent_wait(&mut self, req: &Request, cx: &App) -> bool {
        if !self.subagent_status(req, cx).is_ok_and(|status| matches!(status["state"].as_str(), Some("starting" | "running"))) {
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
            let busy = status.as_ref().is_ok_and(|status| matches!(status["state"].as_str(), Some("starting" | "running")));
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
        if parent.claude.as_ref().is_some_and(|m| m.harness == Harness::Antigravity) {
            return Err("Shared subagents are not enabled for Antigravity yet.".into());
        }
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
                    && parent.claude.as_ref().is_none_or(|m| m.parent_session.is_none() || m.subagent_edits);
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
                let meta = ClaudeMeta {
                    harness, parent_session: Some(parent.id.clone()), subagent_edits: edits,
                    chosen_model: model.map(String::from),
                    chosen_effort: req.args["effort"].as_str().map(String::from),
                    account: parent.claude.as_ref().and_then(|m| m.account.clone()),
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
                    id: id.clone(), kind: SessionKind::Claude, title: format!("{} subagent", harness.name()),
                    custom_title: None, auto_title: None, project_id: parent.project_id,
                    cwd: parent.cwd, created_at: now(), last_active: now(), exit_code: None,
                    hidden: false, claude: Some(meta),
                };
                self.start_thread_with_focus(record, start, true, false, window, cx);
                self.subagent_status(&Request { args: json!({"id": id}), ..req.clone() }, cx).map(|status| status.to_string())
            }
            "list_subagents" => {
                let children = self.store.data.sessions.iter()
                    .filter(|rec| rec.claude.as_ref().is_some_and(|m| m.parent_session.as_deref() == Some(parent.id.as_str())))
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
                    let meta = record.claude.as_ref().ok_or("The subagent has no session metadata.")?;
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
