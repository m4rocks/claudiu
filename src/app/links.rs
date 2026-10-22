//! Links between sessions and chats, and the GitHub issues and pull requests attached to them. Both live on the
//! session's or chat's own record; the model sets them through the MCP tools and the user edits them in the right sidebar.

use super::*;

impl Workspace {
    /// Show two sessions or chats together: each lists the other. True when something changed (false when either
    /// does not exist or they were linked already).
    pub fn link(&mut self, a: &str, b: &str) -> bool {
        let exists = |ws: &Self, id: &str| ws.store.session(id).is_some_and(|s| s.claude.is_some()) || ws.chat_store.get(id).is_some();
        if a == b || !exists(self, a) || !exists(self, b) {
            return false;
        }
        let a_new = self.edit_links(a, |l| add_once(l, b));
        let b_new = self.edit_links(b, |l| add_once(l, a));
        a_new | b_new
    }

    /// Run `f` on the attached items of a session or a chat; true when it changed anything.
    fn edit_attached(&mut self, id: &str, f: impl FnOnce(&mut Vec<Attached>) -> bool) -> bool {
        if let Some(meta) = self.store.session_mut(id).and_then(|s| s.claude.as_mut()) {
            return f(&mut meta.attached);
        }
        let mut changed = false;
        self.chat_store.update(id, |r| changed = f(&mut r.attached));
        changed
    }

    pub fn attached_of(&self, id: &str) -> Vec<Attached> {
        match self.store.session(id).and_then(|s| s.claude.as_ref()) {
            Some(m) => m.attached.clone(),
            None => self.chat_store.get(id).map(|c| c.attached.clone()).unwrap_or_default(),
        }
    }

    /// Take an attached item away from a session or chat.
    pub fn detach(&mut self, id: &str, pull: bool, number: u32) {
        self.edit_attached(id, |l| {
            l.retain(|x| !(x.pull == pull && x.number == number));
            true
        });
        self.store.mark_dirty();
    }

    /// What a session or chat asked for through its MCP tools (`stem` is its record id as a file name).
    pub(super) fn apply_attachment(&mut self, stem: &str, item: crate::mcp::Attachment) -> bool {
        use crate::mcp::Attachment::*;
        let me = self.store.data.sessions.iter().map(|s| &s.id).chain(self.chat_store.shown().map(|c| &c.id)).find(|id| datadir::safe_file_stem(id) == stem).cloned();
        let Some(me) = me else { return false };
        match item {
            Github(a) => self.edit_attached(&me, |list| attach(list, a)),
            Link(other) => self.link(&me, &other),
            Unlink(other) => {
                self.unlink(&me, &other);
                true
            }
            Detach { pull, number } => {
                self.detach(&me, pull, number);
                true
            }
        }
    }

    /// Take the link between two away on both sides.
    pub fn unlink(&mut self, a: &str, b: &str) {
        self.edit_links(a, |l| remove_id(l, b));
        self.edit_links(b, |l| remove_id(l, a));
        self.store.mark_dirty();
    }

    /// Run `f` on the link list of a session or a chat; true when it changed anything.
    fn edit_links(&mut self, id: &str, f: impl FnOnce(&mut Vec<Id>) -> bool) -> bool {
        if let Some(meta) = self.store.session_mut(id).and_then(|s| s.claude.as_mut()) {
            return f(&mut meta.linked);
        }
        let mut changed = false;
        self.chat_store.update(id, |r| changed = f(&mut r.links));
        changed
    }

    /// The sessions and chats linked with `id`, as `(id, title)`; a linked one that has gone is left out.
    pub fn links_of(&self, id: &str) -> Vec<(Id, String)> {
        let ids = match self.store.session(id).and_then(|s| s.claude.as_ref()) {
            Some(m) => m.linked.clone(),
            None => self.chat_store.get(id).map(|c| c.links.clone()).unwrap_or_default(),
        };
        ids.into_iter()
            .filter_map(|o| {
                let title = self.store.session(&o).map(|s| s.display_title().to_string()).or_else(|| self.chat_store.get(&o).map(|c| c.title.clone()))?;
                Some((o, title))
            })
            .collect()
    }
}

fn add_once(list: &mut Vec<Id>, id: &str) -> bool {
    let missing = !list.iter().any(|x| x == id);
    if missing {
        list.push(id.to_string());
    }
    missing
}

fn remove_id(list: &mut Vec<Id>, id: &str) -> bool {
    let before = list.len();
    list.retain(|x| x != id);
    list.len() != before
}

/// Most items one session keeps; the oldest go first.
const MAX_ATTACHED: usize = 20;

/// Add an attached item unless it is there already (a later title fills in a missing one). True when anything changed.
pub(super) fn attach(list: &mut Vec<Attached>, item: Attached) -> bool {
    if let Some(have) = list.iter_mut().find(|a| a.pull == item.pull && a.number == item.number) {
        if have.title.is_none() && item.title.is_some() {
            have.title = item.title;
            return true;
        }
        return false;
    }
    list.push(item);
    if list.len() > MAX_ATTACHED {
        list.remove(0);
    }
    true
}

#[cfg(test)]
mod attach_tests {
    use super::*;

    #[test]
    fn link_lists_add_and_remove_once() {
        let mut l = vec![];
        assert!(add_once(&mut l, "a") && !add_once(&mut l, "a") && add_once(&mut l, "b"));
        assert!(remove_id(&mut l, "a") && !remove_id(&mut l, "a"));
        assert_eq!(l, ["b"]);
    }

    fn item(pull: bool, number: u32, title: Option<&str>) -> Attached {
        Attached { pull, number, title: title.map(String::from) }
    }

    #[test]
    fn an_item_is_attached_once_and_may_gain_a_title() {
        let mut list = vec![];
        assert!(attach(&mut list, item(false, 5, None)));
        assert!(!attach(&mut list, item(false, 5, None)), "same item again");
        assert!(attach(&mut list, item(true, 5, None)), "a pull request is not the issue with that number");
        assert!(attach(&mut list, item(false, 5, Some("Fix it"))), "title filled in");
        assert!(!attach(&mut list, item(false, 5, Some("Other"))), "an existing title stays");
        assert_eq!(list, [item(false, 5, Some("Fix it")), item(true, 5, None)]);
    }

    #[test]
    fn the_oldest_items_drop_off() {
        let mut list = vec![];
        for n in 1..=(MAX_ATTACHED as u32 + 3) {
            attach(&mut list, item(false, n, None));
        }
        assert_eq!(list.len(), MAX_ATTACHED);
        assert_eq!(list[0].number, 4);
    }
}
