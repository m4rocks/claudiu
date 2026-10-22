//! Sessions reaching other sessions: a chat can open a Claude Code session or read a terminal, a terminal can
//! open a chat or read one. The MCP helper files the request (`bridge.rs`) and Claudiu answers it on its next tick,
//! with no dialog (product owner decision).

use super::*;
use crate::bridge::{self, Request};
use serde_json::Value;

/// Longest first message a session may hand over: it travels on a Windows command line, which is limited to about
/// 32,000 characters.
const MAX_PROMPT: usize = 2000;

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
    let old = |t: i64| match crate::widgets::ago(t, now).as_str() {
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
        let cwd = match kind {
            "chat" => self.chat_store.get(id).map(|r| r.cwd.clone()).filter(|c| is_tied(c)),
            _ => self.store.session(id).map(|s| s.cwd.clone()),
        };
        // A chat belongs to the project, not to whichever subfolder the asker happens to be in.
        let root = |c: &PathBuf| self.store.project_for_cwd(c).and_then(|p| self.store.project(&p)).map(|p| p.path.clone());
        Ok(match cwd {
            Some(c) if req.tool == "start_chat" => Some(root(&c).unwrap_or(c)),
            other => other,
        })
    }

    /// Answer the requests that arrived since the last tick (not those whose asker has given up). One that can't
    /// work is refused with the reason.
    pub(super) fn serve_requests(&mut self, requests: Vec<Request>, window: &mut Window, cx: &mut Context<Self>) {
        for mut req in requests.into_iter().filter(|r| r.deadline > crate::store::now()) {
            let result = self.prepare(&mut req).and_then(|()| self.run_request(&req, window, cx));
            let ok = result.is_ok();
            answer(&req, ok, result.unwrap_or_else(|e| e));
        }
    }

    /// Check what starting a session or chat needs and resolve the folder into `req.args["folder"]`.
    fn prepare(&self, req: &mut Request) -> Result<(), String> {
        if !matches!(req.tool.as_str(), "start_code_session" | "start_chat") {
            return Ok(());
        }
        let folder = self.requested_folder(req)?;
        if req.tool == "start_code_session" && folder.is_none() {
            return Err(format!("Say which project or folder to open it in (the folder argument). Projects: {}.", self.project_names()));
        }
        req.args["folder"] = folder.map_or(Value::Null, |f| f.to_string_lossy().into_owned().into());
        if req.args["prompt"].as_str().is_some_and(|p| p.trim().chars().count() > MAX_PROMPT) {
            return Err(format!("The first message is too long ({MAX_PROMPT} characters at most). Shorten it, or write the details to a file and point to it."));
        }
        Ok(())
    }

    /// Do what was asked.
    pub(super) fn run_request(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) -> Result<String, String> {
        let id = req.args["id"].as_str().unwrap_or_default().to_string();
        let count = |key: &str, default: u64, max: u64| req.args[key].as_u64().unwrap_or(default).clamp(1, max) as usize;
        let requester_id = req.from.split_once(':').map_or(req.from.as_str(), |(_, id)| id);
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
                        format!("- id {} · {} · {project} · {state} · {}{}", r.id, r.title, stamps(r.created_at, r.last_active), if req.from == format!("chat:{}", r.id) { " · this chat" } else { "" })
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
                Ok(format!("Chat “{}” ({}), latest messages:\n\n{}", rec.title, stamps(rec.created_at, rec.last_active), if text.is_empty() { "(no messages yet)" } else { &text }))
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
                        format!("- id {} · {} · {project} · {state} · {}{}", s.id, s.display_title(), stamps(s.created_at, s.last_active), if me { " · this session" } else { "" })
                    })
                    .collect();
                Ok(if lines.is_empty() { "No Claude Code sessions.".into() } else { lines.join("\n") })
            }
            "search_sessions" => {
                let words: Vec<String> = req.args["query"].as_str().unwrap_or_default().to_lowercase().split_whitespace().map(String::from).collect();
                let project_of = |cwd: &std::path::Path| self.store.project_for_cwd(cwd).and_then(|p| self.store.project(&p)).map(|p| p.name.clone());
                let mut found: Vec<(i64, String)> = vec![];
                for r in self.chat_store.shown().filter(|r| r.title != DEFAULT_TITLE) {
                    let project = if is_tied(&r.cwd) { project_of(&r.cwd) } else { None };
                    let hay = format!("{} {}", r.title, project.clone().unwrap_or_default()).to_lowercase();
                    if words.iter().all(|w| hay.contains(w)) {
                        found.push((r.last_active, format!("- chat id {} · {} · {} · {}{}", r.id, r.title, project.unwrap_or_else(|| "no project".into()), stamps(r.created_at, r.last_active), if req.from == format!("chat:{}", r.id) { " · this chat" } else { "" })));
                    }
                }
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
            "set_chat_project" => {
                let name = req.args["project"].as_str().map(str::trim).unwrap_or_default();
                let chat = req.from.strip_prefix("chat:").ok_or("Only a chat can be tied to a project.")?;
                let project = self.store.data.projects.iter().find(|p| p.name.eq_ignore_ascii_case(name)).ok_or_else(|| format!("No project “{name}”. Projects: {}.", self.project_names()))?;
                let (path, name) = (project.path.clone(), project.name.clone());
                let pane = self.chat_pane(chat).ok_or("This chat is not open.")?;
                pane.view.update(cx, |v, _| v.tie_after_turn(path));
                Ok(format!("This chat will work in {name} as soon as this reply is finished."))
            }
            "read_code_session" => {
                let rec = self.store.session(&id).filter(|s| s.kind == SessionKind::Claude).ok_or("No session with that id.")?;
                let view = self.live_view(&id).filter(|v| v.read(cx).is_running()).ok_or_else(|| format!("The session “{}” has ended; only running sessions can be read.", rec.display_title()))?;
                let text = view.read(cx).tail_text(count("lines", 120, 400));
                Ok(format!("Session “{}” ({}), latest terminal output:\n\n{text}", rec.display_title(), stamps(rec.created_at, rec.last_active)))
            }
            "start_code_session" => {
                let cwd = folder.ok_or("No folder.")?;
                let before = self.selected.clone();
                // A prompt that looks like a flag must not become one.
                let leading = prompt.map(|p| if p.starts_with('-') { format!(" {p}") } else { p }).into_iter().collect();
                self.new_claude_with(cwd.clone(), leading, window, cx);
                match self.selected.clone().filter(|s| Some(s) != before.as_ref()) {
                    Some(id) => {
                        // A session started from another one is linked with it from the start.
                        self.link(requester_id, &id);
                        Ok(format!("Opened a Claude Code session (id {id}) in {}. It is running in the user's sidebar.", cwd.to_string_lossy()))
                    }
                    None => Err("The session could not be started (is the `claude` CLI installed?).".into()),
                }
            }
            "start_chat" => {
                let id = self.new_chat(folder, prompt, window, cx);
                self.link(requester_id, &id);
                Ok(format!("Opened a new chat (id {id}) in the user's sidebar."))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_times_become_utc_dates() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_782_119_400), "2026-06-22 09:10 UTC");
        assert_eq!(utc(951_782_400 + 3600), "2000-02-29 01:00 UTC");
    }
}
