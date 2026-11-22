//! Sessions reaching other sessions: a Claude Code session can list, search and read the others and open a new one.
//! The MCP helper files the request (`claude/bridge.rs`) and m4code answers it on its next tick, without asking the
//! user: the tools' instructions tell the model to call them only when the user asked for what they do.

use super::*;
use crate::claude::bridge::{self, Request};
use crate::harness::CatalogModel;
use serde_json::Value;

/// Longest first message a session may hand over: it travels on a Windows command line, which is limited to about
/// 32,000 characters.
const MAX_PROMPT: usize = 2000;

/// A worktree switch a session asked for, waiting for the turn in which it asked to end: the switch restarts the
/// process that made the call.
pub struct PendingSwitch {
    id: Id,
    folder: PathBuf,
    base: crate::git::worktree::Base,
    /// Unix seconds when the session asked for it.
    asked: i64,
}

/// How long a switch waits for the turn to end before it is given up.
const SWITCH_PATIENCE_SECS: i64 = 300;

/// `2026-10-22 09:10 UTC` for a Unix time (days since 1970 turned into a calendar date, no time crate needed).
fn utc(ts: i64) -> String {
    let (days, secs) = (ts.div_euclid(86_400), ts.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", secs / 3600, secs % 3600 / 60)
}

/// What the model reads to tell how old a session is: when it was created and last updated, and how long ago.
fn stamps(created: i64, updated: i64) -> String {
    let now = crate::store::now();
    let old = |t: i64| match crate::panels::widgets::ago(t, now).as_str() {
        "now" => "just now".to_string(),
        a => format!("{a} ago"),
    };
    format!("created {} ({}) · updated {} ({})", utc(created), old(created), utc(updated), old(updated))
}

/// Answer a request, unless the asker has given up already: nobody would read it and the file would linger.
fn answer(req: &Request, ok: bool, text: impl Into<String>) {
    if req.deadline > crate::store::now() {
        bridge::respond(&req.id, ok, text);
    }
}

impl Workspace {
    fn project_names(&self) -> String {
        self.store.data.projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
    }

    /// The project or folder a request names; without one, the requester's own project, if it has one.
    fn requested_folder(&self, req: &Request) -> Result<Option<PathBuf>, String> {
        let id = req.from.split_once(':').map_or(req.from.as_str(), |(_, id)| id);
        if let Some(name) = req.args["folder"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
            let by_name = self.store.data.projects.iter().find(|p| p.name.eq_ignore_ascii_case(name));
            let path = PathBuf::from(name);
            return match (by_name, path.is_dir()) {
                (Some(p), _) => Ok(Some(p.path.clone())),
                (None, true) => Ok(Some(path)),
                _ => Err(format!("No project or folder “{name}”. Projects: {}.", self.project_names())),
            };
        }
        Ok(self.store.session(id).map(|s| s.cwd.clone()))
    }

    /// Answer the requests that arrived since the last tick (not those whose asker has given up). One that can't
    /// work is refused with the reason.
    pub(super) fn serve_requests(&mut self, requests: Vec<Request>, window: &mut Window, cx: &mut Context<Self>) {
        for mut req in requests.into_iter().filter(|r| r.deadline > crate::store::now()) {
            let prepared = self.prepare(&mut req, cx);
            if prepared.is_ok() && req.tool == "ask_user" {
                if let Err(why) = self.ask_user_in_thread(&req, cx) {
                    answer(&req, false, why);
                }
                continue;
            }
            if prepared.is_ok() && req.tool == "wait_subagent" && self.defer_subagent_wait(&req, cx) {
                continue;
            }
            let result = prepared.and_then(|()| self.run_request(&req, window, cx));
            let ok = result.is_ok();
            answer(&req, ok, result.unwrap_or_else(|e| e));
        }
        self.settle_subagent_waits(cx);
    }

    /// Check what the request needs. A new session's folder, or the worktree to switch to, is resolved into `req.args["folder"]`.
    fn prepare(&self, req: &mut Request, cx: &App) -> Result<(), String> {
        if req.tool == "switch_worktree" {
            return self.prepare_switch(req);
        }
        if req.tool == "set_model" {
            return self.prepare_model(req, cx);
        }
        if req.tool != "start_code_session" {
            return Ok(());
        }
        let folder = self.requested_folder(req)?;
        if folder.is_none() {
            return Err(format!("Say which project or folder to open it in (the folder argument). Projects: {}.", self.project_names()));
        }
        req.args["folder"] = folder.map_or(Value::Null, |f| f.to_string_lossy().into_owned().into());
        if req.args["prompt"].as_str().is_some_and(|p| p.trim().chars().count() > MAX_PROMPT) {
            return Err(format!("The first message is too long ({MAX_PROMPT} characters at most). Shorten it, or write the details to a file and point to it."));
        }
        if let Some(wanted) = req.args["model"].as_str().map(str::trim).filter(|m| !m.is_empty()) {
            let model = find_model(&self.store.data.settings.aggregated_models(), wanted)?;
            req.args["harness"] = model.harness.key().into();
            req.args["model_id"] = model.id.into();
        }
        if let Some(effort) = req.args["effort"].as_str().filter(|e| !e.is_empty())
            && !crate::claude::defaults::Effort::ALL.iter().any(|known| known.name() == effort)
        {
            return Err("Choose low, medium, high, xhigh or max effort.".into());
        }
        Ok(())
    }

    /// Show an `ask_user` MCP tool call in the requester's chat view (and sidebar hover popover); `ThreadView::answer`
    /// sends the bridge reply once the user clicks an option or declines.
    fn ask_user_in_thread(&self, req: &Request, cx: &mut Context<Self>) -> Result<(), String> {
        let thread = self.requester_thread(req)?;
        let title = req.args["question"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or("Give a non-empty `question`.")?.to_string();
        let raw_options = req.args["options"].as_array().ok_or("Give at least one option in `options`.")?;
        let options: Vec<crate::thread::QuestionOption> = raw_options
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                let name = o["label"].as_str().or_else(|| o["name"].as_str()).or_else(|| o.as_str())?.trim();
                if name.is_empty() {
                    return None;
                }
                let description = o["description"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
                Some(crate::thread::QuestionOption {
                    id: format!("opt-{i}"),
                    name: name.to_string(),
                    description,
                    kind: "allow_once".into(),
                })
            })
            .collect();
        if options.is_empty() {
            return Err("Give at least one option with a non-empty `label`.".into());
        }
        let req_id = req.id.clone();
        thread.update(cx, |v, cx| v.ask_bridge_question(req_id, title, options, cx));
        Ok(())
    }

    /// The requester's chat view, for the model tools.
    fn requester_thread(&self, req: &Request) -> Result<Entity<crate::thread::ThreadView>, String> {
        let id = req.from.split_once(':').map_or(req.from.as_str(), |(_, id)| id);
        self.live_view(id).and_then(|v| v.thread()).cloned().ok_or_else(|| "This session isn't open in m4code's chat view.".to_string())
    }

    /// Resolve `set_model`'s model into `harness`, `model_id` and `name`, and mark a switch onto another harness
    /// (`cross`), which hands the conversation over once the turn ends.
    fn prepare_model(&self, req: &mut Request, cx: &App) -> Result<(), String> {
        let thread = self.requester_thread(req)?.read(cx);
        let wanted = req.args["model"].as_str().map(str::trim).filter(|m| !m.is_empty());
        if wanted.is_none() && req.args["effort"].as_str().is_none() {
            return Err("Give a model (as list_models prints it), an effort, or both.".into());
        }
        if let Some(wanted) = wanted {
            let model = find_model(&thread.unified_models(), wanted)?;
            let cross = model.harness != thread.harness;
            if cross && req.args["message"].as_str().is_some_and(|m| m.trim().chars().count() > MAX_PROMPT) {
                return Err(format!("The message is too long ({MAX_PROMPT} characters at most)."));
            }
            req.args["harness"] = model.harness.key().into();
            req.args["model_id"] = model.id.clone().into();
            req.args["name"] = format!("{} ({})", model.name, model.harness.name()).into();
            req.args["cross"] = cross.into();
        }
        Ok(())
    }

    fn prepare_switch(&self, req: &mut Request) -> Result<(), String> {
        let id = req.from.split_once(':').map_or(req.from.as_str(), |(_, id)| id);
        let rec = self.store.session(id).ok_or("m4code doesn't know this session.")?;
        let git = rec.project_id.as_ref().and_then(|p| self.store.project(p)).and_then(|p| p.git.as_ref()).ok_or("This session's project is not a git repository.")?;
        let wanted = req.args["worktree"].as_str().unwrap_or_default();
        let listed = crate::git::worktree::folders(git).into_iter().skip(1).filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned())).collect::<Vec<_>>();
        let target = crate::git::worktree::resolve(git, &rec.cwd, wanted).map_err(|why| format!("{why} Worktrees: main{}{}.", if listed.is_empty() { "" } else { ", " }, listed.join(", ")))?;
        req.args["folder"] = target.folder.to_string_lossy().into_owned().into();
        req.args["new"] = target.new.into();
        Ok(())
    }

    /// Start the switches whose session has ended its turn. Looks at Claude Code's own status, as a switch from
    /// the menu does, so the call that asked is finished and answered first.
    pub(super) fn run_pending_switches(&mut self, statuses: &std::collections::HashMap<String, String>, window: &mut Window, cx: &mut Context<Self>) {
        let now = crate::store::now();
        let pending = std::mem::take(&mut self.pending_switches);
        for p in pending {
            let claude_id = self.store.session(&p.id).and_then(|r| r.claude_id());
            // Without a registry entry (older Claude Code) the terminal's own activity decides, as in `switch::inspect`.
            let idle = match claude_id.and_then(|c| statuses.get(c)) {
                Some(status) => status == "idle",
                None => self.live_view(&p.id).is_some_and(|v| !v.is_working(cx)),
            };
            if now - p.asked > SWITCH_PATIENCE_SECS {
                self.toast("The worktree switch was dropped: the session's turn did not end.", cx);
            } else if !self.is_running(&p.id, cx) {
                // Closed in the meantime: nothing to move.
            } else if idle {
                self.switch_session(p.id, p.folder, p.base, false, window, cx);
            } else {
                self.pending_switches.push(p);
            }
        }
    }

    /// Do what was asked.
    pub(super) fn run_request(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) -> Result<String, String> {
        if crate::claude::mcp::SUBAGENT_TOOLS.contains(&req.tool.as_str()) {
            return self.run_subagent_request(req, window, cx);
        }
        let id = req.args["id"].as_str().unwrap_or_default().to_string();
        let count = |key: &str, default: u64, max: u64| req.args[key].as_u64().unwrap_or(default).clamp(1, max) as usize;
        let requester_id = req.from.split_once(':').map_or(req.from.as_str(), |(_, id)| id);
        let folder = req.args["folder"].as_str().map(PathBuf::from);
        let prompt = req.args["prompt"].as_str().map(str::trim).filter(|p| !p.is_empty()).map(String::from);
        match req.tool.as_str() {
            "list_code_sessions" => {
                let mut sessions: Vec<&SessionRecord> = self.store.data.sessions.iter().filter(|s| s.kind == SessionKind::Claude && !s.hidden).collect();
                sessions.sort_by_key(|s| std::cmp::Reverse(s.last_active));
                let lines: Vec<String> = sessions
                    .iter()
                    .take(50)
                    .map(|s| {
                        let project = self.store.project_for_cwd(&s.cwd).and_then(|p| self.store.project(&p)).map_or_else(|| s.cwd.to_string_lossy().into_owned(), |p| p.name.clone());
                        let state = if self.is_running(&s.id, cx) { "running" } else { "ended" };
                        let me = req.from == format!("code:{}", s.id);
                        format!("- id {} · {} · {project} · {state} · {}{}", s.id, s.display_title(), stamps(s.created_at, s.last_active), if me { " · this session" } else { "" })
                    })
                    .collect();
                Ok(if lines.is_empty() { "No coding sessions.".into() } else { lines.join("\n") })
            }
            "search_sessions" => {
                let words: Vec<String> = req.args["query"].as_str().unwrap_or_default().to_lowercase().split_whitespace().map(String::from).collect();
                let project_of = |cwd: &std::path::Path| self.store.project_for_cwd(cwd).and_then(|p| self.store.project(&p)).map(|p| p.name.clone());
                let mut found: Vec<(i64, String)> = vec![];
                for s in self.store.data.sessions.iter().filter(|s| s.kind == SessionKind::Claude && !s.hidden) {
                    let project = project_of(&s.cwd);
                    let hay = format!("{} {} {}", s.display_title(), project.clone().unwrap_or_default(), s.cwd.to_string_lossy()).to_lowercase();
                    if words.iter().all(|w| hay.contains(w)) {
                        let me = req.from == format!("code:{}", s.id);
                        found.push((s.last_active, format!("- code session id {} · {} · {} · {}{}", s.id, s.display_title(), project.unwrap_or_else(|| s.cwd.to_string_lossy().into_owned()), stamps(s.created_at, s.last_active), if me { " · this session" } else { "" })));
                    }
                }
                found.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
                let lines: Vec<String> = found.into_iter().take(15).map(|(_, l)| l).collect();
                Ok(if lines.is_empty() { "Nothing matches.".into() } else { format!("{}\nLink one with link_session.", lines.join("\n")) })
            }
            "search_projects" => {
                let words: Vec<String> = req.args["query"].as_str().unwrap_or_default().to_lowercase().split_whitespace().map(String::from).collect();
                let lines: Vec<String> = self
                    .store
                    .data
                    .projects
                    .iter()
                    .filter(|p| {
                        let hay = format!("{} {}", p.name, p.path.to_string_lossy()).to_lowercase();
                        words.iter().all(|w| hay.contains(w))
                    })
                    .take(30)
                    .map(|p| {
                        let branch = p.git.as_ref().and_then(|g| g.branch.as_deref()).map_or(String::new(), |b| format!(" · branch {b}"));
                        format!("- {} · {}{branch}", p.name, p.path.to_string_lossy())
                    })
                    .collect();
                Ok(if lines.is_empty() { "No project matches.".into() } else { lines.join("\n") })
            }
            "read_code_session" => {
                let rec = self.store.session(&id).filter(|s| s.kind == SessionKind::Claude).ok_or("No session with that id.")?;
                let view = self.live_view(&id).filter(|v| v.is_running(cx)).ok_or_else(|| format!("The session “{}” has ended; only running sessions can be read.", rec.display_title()))?;
                let text = view.tail_text(count("lines", 120, 400), cx);
                Ok(format!("Session “{}” ({}), latest conversation:\n\n{text}", rec.display_title(), stamps(rec.created_at, rec.last_active)))
            }
            "start_code_session" => {
                let cwd = folder.ok_or("No folder.")?;
                let harness = req.args["harness"].as_str().and_then(|key| crate::harness::Harness::ALL.into_iter().find(|h| h.key() == key)).unwrap_or(self.store.data.settings.harness);
                // The model may pick the model and effort; the permission mode stays the user's default for new
                // sessions (`Starter::OtherSession` never bypasses permissions).
                let mut options = vec![];
                if let Some(model) = req.args["model_id"].as_str() {
                    options.push(("model", model.to_string()));
                }
                if let Some(effort) = req.args["effort"].as_str().filter(|e| !e.is_empty()) {
                    options.push(("thought_level", effort.to_string()));
                }
                let before = self.selected.clone();
                self.new_session_with(harness, cwd.clone(), prompt, options, Starter::OtherSession, window, cx);
                match self.selected.clone().filter(|s| Some(s) != before.as_ref()) {
                    Some(id) => {
                        // A session started from another one is linked with it from the start.
                        self.link(requester_id, &id);
                        Ok(format!("Opened a {} session (id {id}) in {}. It is running in the user's sidebar.", harness.name(), cwd.to_string_lossy()))
                    }
                    None => Err(format!("The {} session could not be started.", harness.name())),
                }
            }
            "switch_worktree" => {
                let folder = folder.ok_or("No folder.")?;
                let base = if req.args["base"].as_str() == Some("current") { crate::git::worktree::Base::Current } else { crate::git::worktree::Base::Main };
                let name = folder.file_name().map_or_else(|| "the main checkout".into(), |n| n.to_string_lossy().into_owned());
                self.pending_switches.push(PendingSwitch { id: requester_id.to_string(), folder, base, asked: crate::store::now() });
                Ok(format!("m4code switches this session to {name} as soon as your turn ends, so end your turn now: reply in one line and make no more tool calls. The user's next message continues the same conversation there."))
            }
            "list_models" => {
                let thread = self.requester_thread(req)?.read(cx);
                Ok(model_lines(&thread.unified_models(), thread.current_model().as_ref()))
            }
            "set_model" => {
                let thread = self.requester_thread(req)?;
                let model = req.args["harness"].as_str().and_then(|key| crate::harness::Harness::ALL.into_iter().find(|h| h.key() == key)).map(|harness| CatalogModel {
                    harness,
                    id: req.args["model_id"].as_str().unwrap_or_default().to_string(),
                    name: req.args["name"].as_str().unwrap_or_default().to_string(),
                });
                let effort = effort_for(req.args["effort"].as_str(), model.is_some());
                match model {
                    Some(model) if req.args["cross"].as_bool() == Some(true) => {
                        let message = req.args["message"].as_str().map(str::trim).filter(|m| !m.is_empty()).map_or_else(|| "Carry on with the task from where the conversation left off.".into(), String::from);
                        let name = model.name.clone();
                        thread.update(cx, |t, cx| t.hand_off_after_turn(model, effort, message, cx));
                        Ok(format!("m4code hands this conversation to {name} as soon as your turn ends, so end your turn now: reply in one line and make no more tool calls."))
                    }
                    model => {
                        let done = thread.update(cx, |t, cx| t.switch_model(model.as_ref(), effort.as_deref(), cx))?;
                        Ok(format!("Switched to {done} from your next request. Your system prompt may still name the previous model; ignore that."))
                    }
                }
            }
            other => Err(format!("unknown tool {other}")),
        }
    }
}

/// The effort set_model asks for: the one the agent named, or medium with a new model, since agents are told to name an
/// effort only when the user did.
fn effort_for(named: Option<&str>, new_model: bool) -> Option<String> {
    match named.map(str::trim).filter(|e| !e.is_empty()) {
        Some(effort) => Some(effort.to_string()),
        None => new_model.then(|| "medium".to_string()),
    }
}

/// What `list_models` answers: one line per model a session can switch to, `harness:model-id · name`, the current one
/// marked.
fn model_lines(models: &[CatalogModel], current: Option<&CatalogModel>) -> String {
    let lines: Vec<String> = models
        .iter()
        .filter(|m| !m.id.is_empty())
        .map(|m| {
            let here = current.is_some_and(|c| c.harness == m.harness && c.id == m.id);
            format!("- {}:{} · {} ({}){}", m.harness.key(), m.id, m.name, m.harness.name(), if here { " · current" } else { "" })
        })
        .collect();
    if lines.is_empty() { "No models are listed yet.".into() } else { lines.join("\n") }
}

/// The model `wanted` names, as `list_models` prints it (`codex:gpt-6`). A bare id is accepted when only one harness
/// offers it, so `opus` still works when it is unambiguous.
fn find_model(models: &[CatalogModel], wanted: &str) -> Result<CatalogModel, String> {
    let wanted = wanted.trim();
    let named = |m: &&CatalogModel| match wanted.split_once(':') {
        Some((key, id)) if crate::harness::Harness::ALL.iter().any(|h| h.key() == key) => m.harness.key() == key && m.id == id,
        _ => m.id == wanted,
    };
    let found: Vec<&CatalogModel> = models.iter().filter(|m| !m.id.is_empty()).filter(named).collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("No model “{wanted}”. Use one of these exactly as written:\n{}", model_lines(models, None))),
        many => Err(format!("“{wanted}” is offered by {}. Say which, as harness:model-id.", many.iter().map(|m| m.harness.name()).collect::<Vec<_>>().join(" and "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models() -> Vec<CatalogModel> {
        use crate::harness::Harness;
        let m = |harness, id: &str, name: &str| CatalogModel { harness, id: id.into(), name: name.into() };
        vec![m(Harness::Claude, "opus", "Opus"), m(Harness::Codex, "gpt-6", "GPT-6"), m(Harness::OpenCode, "gpt-6", "GPT-6 (OpenCode)"), m(Harness::Antigravity, "", "Antigravity (default)")]
    }

    #[test]
    fn set_model_finds_a_model_by_harness_and_id_or_by_an_unambiguous_id() {
        assert_eq!(find_model(&models(), "codex:gpt-6").unwrap().harness, crate::harness::Harness::Codex);
        assert_eq!(find_model(&models(), " opus ").unwrap().id, "opus");
        let both = find_model(&models(), "gpt-6").unwrap_err();
        assert!(both.contains("Codex and OpenCode"), "{both}");
        let none = find_model(&models(), "claude:sonnet").unwrap_err();
        assert!(none.contains("- codex:gpt-6 · GPT-6 (Codex)"), "an unknown name lists the choices: {none}");
        assert!(find_model(&models(), "antigravity:").is_err(), "a harness's placeholder default is not a model to switch to");
    }

    #[test]
    fn set_model_uses_medium_effort_unless_one_is_named() {
        assert_eq!(effort_for(None, true).as_deref(), Some("medium"));
        assert_eq!(effort_for(Some(" "), true).as_deref(), Some("medium"));
        assert_eq!(effort_for(Some("high"), true).as_deref(), Some("high"));
        assert_eq!(effort_for(Some("low"), false).as_deref(), Some("low"));
        assert_eq!(effort_for(None, false), None, "an effort-only call always names one");
    }

    #[test]
    fn list_models_marks_the_current_model() {
        let listed = model_lines(&models(), models().get(1));
        assert_eq!(listed, "- claude:opus · Opus (Claude Code)\n- codex:gpt-6 · GPT-6 (Codex) · current\n- opencode:gpt-6 · GPT-6 (OpenCode) (OpenCode)");
    }

    #[test]
    fn unix_times_become_utc_dates() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_782_119_400), "2026-06-22 09:10 UTC");
        assert_eq!(utc(951_782_400 + 3600), "2000-02-29 01:00 UTC");
    }
}
