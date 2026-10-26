//! Links between sessions, and the GitHub issues and pull requests attached to them. Both live on the session's own
//! record; the model sets them through the MCP tools and the user edits them in the right sidebar.

use super::*;

impl Workspace {
    /// Show two sessions together: each lists the other. True when something changed (false when either does not
    /// exist or they were linked already).
    pub fn link(&mut self, a: &str, b: &str) -> bool {
        let exists = |ws: &Self, id: &str| ws.store.session(id).is_some_and(|s| s.claude.is_some());
        if a == b || !exists(self, a) || !exists(self, b) {
            return false;
        }
        let a_new = self.edit_links(a, |l| add_once(l, b));
        let b_new = self.edit_links(b, |l| add_once(l, a));
        a_new | b_new
    }

    /// Run `f` on the attached items of a session; true when it changed anything.
    pub(super) fn edit_attached(&mut self, id: &str, f: impl FnOnce(&mut Vec<Attached>) -> bool) -> bool {
        self.store.session_mut(id).and_then(|s| s.claude.as_mut()).is_some_and(|meta| f(&mut meta.attached))
    }

    pub fn attached_of(&self, id: &str) -> Vec<Attached> {
        self.store.session(id).and_then(|s| s.claude.as_ref()).map(|m| m.attached.clone()).unwrap_or_default()
    }

    /// Take an attached item away from a session.
    pub fn detach(&mut self, id: &str, pull: bool, number: u32) {
        self.edit_attached(id, |l| {
            l.retain(|x| !(x.pull == pull && x.number == number));
            true
        });
        self.store.mark_dirty();
    }

    /// What a session asked for through its MCP tools (`stem` is its record id as a file name).
    pub(super) fn apply_attachment(&mut self, stem: &str, item: crate::claude::mcp::Attachment) -> bool {
        use crate::claude::mcp::Attachment::*;
        let me = self.store.data.sessions.iter().map(|s| &s.id).find(|id| datadir::safe_file_stem(id) == stem).cloned();
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

    /// Run `f` on the link list of a session; true when it changed anything.
    fn edit_links(&mut self, id: &str, f: impl FnOnce(&mut Vec<Id>) -> bool) -> bool {
        self.store.session_mut(id).and_then(|s| s.claude.as_mut()).is_some_and(|meta| f(&mut meta.linked))
    }

    /// The sessions linked with `id`, as `(id, title)`; a linked one that has gone is left out.
    pub fn links_of(&self, id: &str) -> Vec<(Id, String)> {
        let ids = self.store.session(id).and_then(|s| s.claude.as_ref()).map(|m| m.linked.clone()).unwrap_or_default();
        ids.into_iter().filter_map(|o| Some((o.clone(), self.store.session(&o)?.display_title().to_string()))).collect()
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

/// What GitHub says about an attached item. True when the title or state differs from what is kept.
pub(super) fn set_status(list: &mut [Attached], pull: bool, number: u32, title: &str, state: crate::github::model::State) -> bool {
    let Some(have) = list.iter_mut().find(|a| a.pull == pull && a.number == number) else { return false };
    if have.title.as_deref() == Some(title) && have.state == Some(state) {
        return false;
    }
    have.title = Some(title.to_string());
    have.state = Some(state);
    true
}

/// Most items one session keeps; the oldest go first.
const MAX_ATTACHED: usize = 20;

/// Add an attached item unless it is there already. True when anything changed.
pub(super) fn attach(list: &mut Vec<Attached>, item: Attached) -> bool {
    if list.iter().any(|a| a.pull == item.pull && a.number == item.number) {
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

    fn item(pull: bool, number: u32) -> Attached {
        Attached { pull, number, title: None, state: None }
    }

    #[test]
    fn an_item_is_attached_once() {
        let mut list = vec![];
        assert!(attach(&mut list, item(false, 5)));
        assert!(!attach(&mut list, item(false, 5)), "same item again");
        assert!(attach(&mut list, item(true, 5)), "a pull request is not the issue with that number");
        assert_eq!(list, [item(false, 5), item(true, 5)]);
    }

    #[test]
    fn github_status_updates_only_what_changed() {
        use crate::github::model::State;
        let mut list = vec![item(false, 5)];
        assert!(set_status(&mut list, false, 5, "Fix it", State::Open));
        assert!(!set_status(&mut list, false, 5, "Fix it", State::Open), "same answer again");
        assert!(set_status(&mut list, false, 5, "Fix it properly", State::Open), "renamed on GitHub");
        assert!(set_status(&mut list, false, 5, "Fix it properly", State::Closed), "closed on GitHub");
        assert!(!set_status(&mut list, true, 5, "Other", State::Open), "no such pull request attached");
        assert_eq!(list[0].title.as_deref(), Some("Fix it properly"));
        assert_eq!(list[0].state, Some(State::Closed));
    }

    #[test]
    fn the_oldest_items_drop_off() {
        let mut list = vec![];
        for n in 1..=(MAX_ATTACHED as u32 + 3) {
            attach(&mut list, item(false, n));
        }
        assert_eq!(list.len(), MAX_ATTACHED);
        assert_eq!(list[0].number, 4);
    }
}
