//! Sessions reaching other sessions: a chat can open a Claude Code session or read a terminal, a terminal can
//! open a chat or read one. The MCP helper files the request (`bridge.rs`); nothing happens until the user says
//! yes in a dialog, and a Dismiss, Escape or click outside means no.

use super::*;
use crate::bridge::{self, Request};
use serde_json::Value;

/// Longest first message a session may hand over. The dialog shows all of it, and a Windows command line (where it
/// travels) is limited to about 32,000 characters.
const MAX_PROMPT: usize = 2000;

/// Answer a request, unless the asker has given up already: nobody would read it and the file would linger.
fn answer(req: &Request, ok: bool, text: impl Into<String>) {
    if req.deadline > crate::store::now() {
        bridge::respond(&req.id, ok, text);
    }
}

impl Workspace {
    /// Show a dialog. One that was open is closed first, and a pending request behind it is declined (never left hanging).
    pub(super) fn open_modal(&mut self, modal: Modal) {
        self.deny_pending_request();
        self.modal = Some(modal);
    }

    /// Decline the request whose dialog is open, if there is one.
    pub(super) fn deny_pending_request(&mut self) {
        if let Some(Modal::Bridge { req, .. }) = self.modal.take_if(|m| matches!(m, Modal::Bridge { .. })) {
            answer(&req, false, "The user declined.");
        }
    }

    /// The user allowed a request: do it, unless the asker has stopped waiting (then it is not done at all).
    pub(super) fn finish_request(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) {
        if req.deadline <= crate::store::now() {
            self.toast("That request had expired, so nothing was done.", false, cx);
            return;
        }
        let result = self.run_request(req, window, cx);
        answer(req, result.is_ok(), result.unwrap_or_else(|e| e));
    }

    fn project_names(&self) -> String {
        self.store.data.projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
    }

    /// Who is asking, for the dialog: the session's title.
    fn requester(&self, from: &str) -> String {
        let (kind, id) = from.split_once(':').unwrap_or(("", from));
        let title = match kind {
            "chat" => self.chat_store.get(id).map(|r| r.title.clone()),
            _ => self.store.session(id).map(|s| s.display_title().to_string()),
        };
        match (kind, title) {
            ("chat", Some(t)) => format!("The chat “{t}”"),
            (_, Some(t)) => format!("The Claude Code session “{t}”"),
            ("chat", None) => "A chat".into(),
            _ => "A Claude Code session".into(),
        }
    }

    /// The project or folder a request names; without one, the requester's own project, if it has one.
    fn requested_folder(&self, req: &Request) -> Result<Option<PathBuf>, String> {
        let (kind, id) = req.from.split_once(':').unwrap_or(("", &req.from));
        if let Some(name) = req.args["folder"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
            let by_name = self.store.data.projects.iter().find(|p| p.name.eq_ignore_ascii_case(name));
            let path = PathBuf::from(name);
            return match (by_name, path.is_dir()) {
                (Some(p), _) => Ok(Some(p.path.clone())),
                (None, true) => Ok(Some(path)),
                _ => Err(format!("No project or folder “{name}”. Projects: {}.", self.project_names())),
            };
        }
        Ok(match kind {
            "chat" => self.chat_store.get(id).map(|r| r.cwd.clone()).filter(|c| is_tied(c)),
            _ => self.store.session(id).map(|s| s.cwd.clone()),
        })
    }

    /// Work out what a request would do and put the question to the user. A request that can't work is answered
    /// at once, without bothering them.
    pub(super) fn ask_about_next_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.is_some() {
            return;
        }
        while let Some(mut req) = self.bridge_queue.pop_front() {
            if req.deadline <= crate::store::now() {
                continue; // waited too long in the queue: the asker has given up
            }
            match self.describe(&mut req) {
                Err(e) => answer(&req, false, e),
                Ok((title, body, confirm)) => {
                    self.modal = Some(Modal::Bridge { req, title, body, confirm });
                    window.focus(&self.modal_focus, cx);
                    cx.notify();
                    return;
                }
            }
        }
    }

    /// (title, body, confirm label) of the question; resolves the folder into `req.args["folder"]`.
    fn describe(&self, req: &mut Request) -> Result<(String, String, String), String> {
        let who = self.requester(&req.from);
        let id = req.args["id"].as_str().unwrap_or_default().to_string();
        Ok(match req.tool.as_str() {
            "list_chats" => ("Share your chats?".into(), format!("{who} wants to see the titles and projects of your chats."), "Allow".into()),
            "list_code_sessions" => ("Share your sessions?".into(), format!("{who} wants to see the titles and folders of your Claude Code sessions."), "Allow".into()),
            "read_chat" => {
                let title = self.chat_store.get(&id).map(|r| r.title.clone()).ok_or("No chat with that id. Use list_chats.")?;
                ("Let it read a chat?".into(), format!("{who} wants to read the latest messages of the chat “{title}”."), "Allow".into())
            }
            "read_code_session" => {
                let title = self.store.session(&id).filter(|s| s.kind == SessionKind::Claude).map(|s| s.display_title().to_string()).ok_or("No session with that id. Use list_code_sessions.")?;
                ("Let it read a session?".into(), format!("{who} wants to read the latest terminal output of the Claude Code session “{title}”."), "Allow".into())
            }
            "start_code_session" | "start_chat" => {
                let code = req.tool == "start_code_session";
                let folder = self.requested_folder(req)?;
                if code && folder.is_none() {
                    return Err(format!("Say which project or folder to open it in (the folder argument). Projects: {}.", self.project_names()));
                }
                req.args["folder"] = folder.as_ref().map_or(Value::Null, |f| f.to_string_lossy().into_owned().into());
                let place = folder.map_or("without a project".to_string(), |f| format!("in {}", f.to_string_lossy()));
                let prompt = req.args["prompt"].as_str().map(str::trim).filter(|p| !p.is_empty());
                if prompt.is_some_and(|p| p.chars().count() > MAX_PROMPT) {
                    return Err(format!("The first message is too long ({MAX_PROMPT} characters at most). Shorten it, or write the details to a file and point to it."));
                }
                let first = prompt.map(|p| format!("\n\nFirst message:\n{p}")).unwrap_or_default();
                if code {
                    ("Open a Claude Code session?".into(), format!("{who} wants to open a new Claude Code session {place}.{first}"), "Open session".into())
                } else {
                    ("Open a new chat?".into(), format!("{who} wants to open a new chat {place}.{first}"), "Open chat".into())
                }
            }
            other => return Err(format!("unknown tool {other}")),
        })
    }

    /// Do what the user allowed.
    pub(super) fn run_request(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) -> Result<String, String> {
        let id = req.args["id"].as_str().unwrap_or_default().to_string();
        let count = |key: &str, default: u64, max: u64| req.args[key].as_u64().unwrap_or(default).clamp(1, max) as usize;
        let folder = req.args["folder"].as_str().map(PathBuf::from);
        let prompt = req.args["prompt"].as_str().map(str::trim).filter(|p| !p.is_empty()).map(String::from);
        match req.tool.as_str() {
            "list_chats" => {
                let mut chats: Vec<&ChatRecord> = self.chat_store.shown().filter(|r| r.title != DEFAULT_TITLE || self.chat_pane(&r.id).is_some()).collect();
                chats.sort_by_key(|r| std::cmp::Reverse(r.last_active));
                let lines: Vec<String> = chats
                    .iter()
                    .take(50)
                    .map(|r| {
                        let project = if is_tied(&r.cwd) {
                            self.store.project_for_cwd(&r.cwd).and_then(|p| self.store.project(&p)).map_or_else(|| r.cwd.to_string_lossy().into_owned(), |p| p.name.clone())
                        } else {
                            "no project".into()
                        };
                        let state = if self.chat_pane(&r.id).is_some() { "open" } else { "closed" };
                        format!("- id {} · {} · {project} · {state}{}", r.id, r.title, if req.from == format!("chat:{}", r.id) { " · this chat" } else { "" })
                    })
                    .collect();
                Ok(if lines.is_empty() { "No chats.".into() } else { lines.join("\n") })
            }
            "read_chat" => {
                let rec = self.chat_store.get(&id).ok_or("No chat with that id.")?;
                let Some(pane) = self.chat_pane(&id) else {
                    return Err(format!("The chat “{}” is not open in Claudiu, so its messages can't be read. Ask the user to open it.", rec.title));
                };
                let text = pane.view.read(cx).transcript(count("messages", 30, 200));
                Ok(format!("Chat “{}”, latest messages:\n\n{}", rec.title, if text.is_empty() { "(no messages yet)" } else { &text }))
            }
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
                        format!("- id {} · {} · {project} · {state}{}", s.id, s.display_title(), if me { " · this session" } else { "" })
                    })
                    .collect();
                Ok(if lines.is_empty() { "No Claude Code sessions.".into() } else { lines.join("\n") })
            }
            "read_code_session" => {
                let rec = self.store.session(&id).filter(|s| s.kind == SessionKind::Claude).ok_or("No session with that id.")?;
                let view = self.live_view(&id).filter(|v| v.read(cx).is_running()).ok_or_else(|| format!("The session “{}” has ended; only running sessions can be read.", rec.display_title()))?;
                let text = view.read(cx).tail_text(count("lines", 120, 400));
                Ok(format!("Session “{}”, latest terminal output:\n\n{text}", rec.display_title()))
            }
            "start_code_session" => {
                let cwd = folder.ok_or("No folder.")?;
                let before = self.selected.clone();
                // A prompt that looks like a flag must not become one.
                let leading = prompt.map(|p| if p.starts_with('-') { format!(" {p}") } else { p }).into_iter().collect();
                self.set_side(Side::Sessions);
                self.new_claude_with(cwd.clone(), leading, window, cx);
                match self.selected.clone().filter(|s| Some(s) != before.as_ref()) {
                    Some(id) => Ok(format!("Opened a Claude Code session (id {id}) in {}. It is running in the user's Code tab.", cwd.to_string_lossy())),
                    None => Err("The session could not be started (is the `claude` CLI installed?).".into()),
                }
            }
            "start_chat" => {
                let id = self.new_chat(folder, prompt, false, window, cx);
                Ok(format!("Opened a new chat (id {id}) in the user's Chat tab."))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }
}
