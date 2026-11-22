//! Picking up a session that was started outside m4code, such as in the agent's own terminal UI: the agent lists the
//! sessions it has for a folder (ACP `session/list`), and the one chosen opens here as a m4code session. A session
//! still open in a terminal keeps running there; the user is told to quit it first, since two programs writing one
//! conversation mix it up.

use super::*;
use serde_json::{Value, json};

use crate::harness::acp::{self, Connection, Launch};

/// One session an agent listed.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub id: String,
    pub title: Option<String>,
    /// As the agent wrote it (ISO 8601), shown as is.
    pub updated: Option<String>,
}

/// The sessions in a `session/list` result. Ones without an id are left out.
pub fn listed(result: &Value) -> Vec<Listed> {
    let sessions = result["sessions"].as_array().map(Vec::as_slice).unwrap_or_default();
    sessions
        .iter()
        .filter_map(|s| {
            let id = s["sessionId"].as_str().filter(|id| !id.is_empty())?.to_string();
            let title = s["title"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(String::from);
            Some(Listed { id, title, updated: s["updatedAt"].as_str().map(String::from) })
        })
        .collect()
}

/// Start the agent, ask it for its sessions in `cwd`, and end it again.
async fn list(launch: Launch, cwd: PathBuf, cx: &mut gpui::AsyncApp) -> Result<Vec<Listed>, String> {
    let (events, _incoming) = futures::channel::mpsc::unbounded();
    let connection = cx.background_executor().spawn(async move { Connection::start(&launch, events) }).await?;
    connection.request("initialize", acp::initialize_params()).await?;
    let result = connection.request("session/list", json!({ "cwd": cwd })).await?;
    Ok(listed(&result))
}

impl Workspace {
    /// The project menu's "Pick up a session here…": list the default agent's sessions in `cwd` in a menu at `at`.
    pub(super) fn list_sessions(&mut self, cwd: PathBuf, at: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let harness = self.store.data.settings.harness;
        let meta = ClaudeMeta { harness, ..Default::default() };
        // The same program and environment a new session gets, so Claude Code lists the active account's sessions.
        let Some(start) = self.thread_start(&new_id(), &cwd, &meta, None, None, Starter::User, cx) else { return };
        self.toast(format!("Looking for {} sessions…", harness.name()), false, cx);
        cx.spawn(async move |this, cx| {
            let server = match start.server.clone() {
                Some(s) => Ok(s),
                None => cx.background_executor().spawn(async move { (harness.adapter().server)() }).await,
            };
            let found = match server {
                Ok((program, args)) => list(Launch { program, args, cwd: cwd.clone(), env: start.env.clone() }, cwd.clone(), cx).await,
                Err(why) => Err(why),
            };
            let _ = this.update(cx, |ws, cx| ws.show_listed(harness, cwd, found, at, cx));
        })
        .detach();
    }

    fn show_listed(&mut self, harness: Harness, cwd: PathBuf, found: Result<Vec<Listed>, String>, at: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let sessions = match found {
            Ok(s) => s,
            Err(why) => return self.toast(format!("{} couldn't list its sessions: {why}", harness.name()), true, cx),
        };
        if sessions.is_empty() {
            return self.toast(format!("{} has no sessions in this folder.", harness.name()), false, cx);
        }
        self.toast("Quit a session in its terminal before picking it up here.", false, cx);
        let items = sessions
            .into_iter()
            .map(|s| {
                let label = s.title.clone().unwrap_or_else(|| format!("Session {}", s.id));
                let act = Act::PickUp { harness, cwd: cwd.clone(), session_id: s.id, title: s.title };
                MenuItem { hint: s.updated.map(|u| u.chars().take(10).collect()), ..MenuItem::new(label, act) }
            })
            .collect();
        self.open_menu(at, items, cx);
    }

    /// Open a listed session here: the record m4code already has for it, or a new one.
    pub(super) fn pick_up(&mut self, harness: Harness, cwd: PathBuf, session_id: String, title: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let known = self.store.data.sessions.iter().find(|s| s.claude.as_ref().is_some_and(|m| m.harness == harness) && s.claude_id() == Some(session_id.as_str())).map(|s| s.id.clone());
        let id = match known {
            Some(id) => id,
            None => {
                let record = SessionRecord {
                    id: new_id(),
                    kind: SessionKind::Claude,
                    title: title.unwrap_or_else(|| format!("{} session", harness.name())),
                    custom_title: None,
                    auto_title: None,
                    project_id: self.store.project_for_cwd(&cwd),
                    cwd,
                    created_at: now(),
                    last_active: now(),
                    exit_code: None,
                    hidden: false,
                    claude: Some(ClaudeMeta { session_id, harness, chrome: self.store.data.settings.chrome_default, ..Default::default() }),
                };
                let id = record.id.clone();
                self.store.data.sessions.push(record);
                self.store.mark_dirty();
                id
            }
        };
        self.resume(&id, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_list_keeps_the_sessions_that_have_an_id() {
        let result = json!({"sessions": [
            {"sessionId": "a", "cwd": "/w", "title": " Fix the build ", "updatedAt": "2026-11-20T10:00:00Z"},
            {"sessionId": "b", "cwd": "/w", "title": ""},
            {"cwd": "/w", "title": "no id"},
        ]});
        assert_eq!(
            listed(&result),
            [
                Listed { id: "a".into(), title: Some("Fix the build".into()), updated: Some("2026-11-20T10:00:00Z".into()) },
                Listed { id: "b".into(), title: None, updated: None },
            ]
        );
        assert!(listed(&json!({})).is_empty());
    }
}
