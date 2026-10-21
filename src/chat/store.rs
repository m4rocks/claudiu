//! Chat history, kept in its own `chats.json` next to `state.json`: an older Claudiu sharing `state.json` would
//! not understand a new session kind, and (like `accounts.json`) it must not erase what it does not know.
//! A record only points at Claude Code's own session; the conversation itself stays in Claude Code's transcript.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::Id;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatRecord {
    pub id: Id,
    /// Claude's session id, known once the first `session/new` answered.
    pub session_id: Option<String>,
    pub cwd: PathBuf,
    pub title: String,
    pub created_at: i64,
    pub last_active: i64,
    /// Removed from the list by the user. The record stays so its Claude session is still known to be a chat's
    /// and is not imported into the Code list on the next launch.
    #[serde(default)]
    pub removed: bool,
}

pub const DEFAULT_TITLE: &str = "New chat";

/// The folder a chat runs in while it is not tied to a project (a neutral, empty folder of its own).
pub fn scratch_dir() -> PathBuf {
    crate::statusline::data_dir().join("chats")
}

/// A chat is tied to a project exactly when it does not run in the scratch folder.
pub fn is_tied(cwd: &Path) -> bool {
    crate::platform::path_key(cwd) != crate::platform::path_key(&scratch_dir())
}

#[derive(Default)]
pub struct ChatStore {
    path: PathBuf,
    pub records: Vec<ChatRecord>,
    dirty: bool,
}

impl ChatStore {
    /// Next to the given `state.json`.
    pub fn path_for(state: &Path) -> PathBuf {
        state.with_file_name("chats.json")
    }

    /// A corrupt file is moved aside, never overwritten.
    pub fn load(path: PathBuf) -> Self {
        let mut records = vec![];
        if let Ok(text) = fs::read_to_string(&path) {
            match serde_json::from_str(&text) {
                Ok(r) => records = r,
                Err(_) => {
                    let _ = fs::rename(&path, path.with_extension("json.corrupt"));
                }
            }
        }
        Self { path, records, dirty: false }
    }

    pub fn get(&self, id: &str) -> Option<&ChatRecord> {
        self.records.iter().find(|r| r.id == id)
    }

    /// Edit a record; saved on the next `save_if_dirty`.
    pub fn update(&mut self, id: &str, f: impl FnOnce(&mut ChatRecord)) {
        if let Some(r) = self.records.iter_mut().find(|r| r.id == id) {
            f(r);
            self.dirty = true;
        }
    }

    pub fn add(&mut self, r: ChatRecord) {
        self.records.push(r);
        self.dirty = true;
    }

    /// Whether this Claude session is a chat's, removed from the list or not.
    pub fn owns(&self, session_id: &str) -> bool {
        self.records.iter().any(|r| r.session_id.as_deref() == Some(session_id))
    }

    /// The chats the sidebar lists.
    pub fn shown(&self) -> impl Iterator<Item = &ChatRecord> {
        self.records.iter().filter(|r| !r.removed)
    }

    /// The user removed a chat from the list. Claude Code's transcript is not touched.
    pub fn hide(&mut self, id: &str) {
        self.update(id, |r| r.removed = true);
    }

    /// Forget a chat that never produced a conversation.
    pub fn remove(&mut self, id: &str) {
        self.records.retain(|r| r.id != id);
        self.dirty = true;
    }

    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension("json.tmp");
        let ok = serde_json::to_string_pretty(&self.records).is_ok_and(|t| fs::write(&tmp, t).is_ok() && fs::rename(&tmp, &self.path).is_ok());
        self.dirty = !ok;
    }
}

/// The sidebar title for a chat that has none yet: the start of the first message.
pub fn title_from(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default();
    let mut t: String = line.chars().take(48).collect();
    if line.chars().count() > 48 {
        t.push('…');
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_chat_leaves_the_list_but_its_session_stays_known() {
        let mut store = ChatStore::default();
        store.add(ChatRecord { id: "c".into(), session_id: Some("s1".into()), title: "Hello".into(), ..Default::default() });
        store.hide("c");
        assert_eq!(store.shown().count(), 0);
        assert!(store.owns("s1"), "the conversation must not be imported as a Code session");
        assert!(!store.owns("other"));
    }

    #[test]
    fn records_roundtrip_and_a_corrupt_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chats.json");
        let mut s = ChatStore::load(path.clone());
        s.add(ChatRecord { id: "a".into(), title: "Hi".into(), ..Default::default() });
        s.update("a", |r| r.session_id = Some("sid".into()));
        s.save_if_dirty();
        let again = ChatStore::load(path.clone());
        assert_eq!(again.get("a").and_then(|r| r.session_id.as_deref()), Some("sid"));
        fs::write(&path, "{nope").unwrap();
        assert!(ChatStore::load(path.clone()).records.is_empty());
        assert!(path.with_extension("json.corrupt").exists());
    }

    #[test]
    fn titles_are_one_short_line() {
        assert_eq!(title_from("  fix the build\nplease"), "fix the build");
        assert_eq!(title_from(&"x".repeat(60)).chars().count(), 49);
    }
}
