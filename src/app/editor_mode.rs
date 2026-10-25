//! Editor mode: the shown session's terminal narrows to the left, and its folder's files and an editor fill the rest.
//! Each session keeps its own pane while it runs; listing and reading files happen off the UI thread (see `files`).

use gpui_kit::component::input::EditorState;
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState};

use super::*;
use crate::files::{self, Contents, Node};

/// Most entries listed in the tree, so a session started in a huge plain folder (or the home folder) stays usable.
const LIST_LIMIT: usize = 20_000;

/// A session's files and editor. Never persisted; it goes away with the session.
pub struct EditorPane {
    pub tree: Entity<TreeState>,
    pub buffer: Entity<EditorState>,
    /// The listing the tree was last given, to skip redrawing it when nothing changed.
    pub nodes: Vec<Node>,
    /// `path_key`s of changed files and their folders, see `files::Listing`.
    pub changed: std::sync::Arc<HashSet<String>>,
    pub truncated: bool,
    /// Folders the user opened, kept open across refreshes.
    pub expanded: HashSet<String>,
    /// The file shown, and why its text isn't shown when it can't be.
    pub file: Option<PathBuf>,
    pub note: Option<String>,
    /// A listing is on its way; refreshes wait for it.
    pub listing: bool,
    pub _sub: Subscription,
}

impl Workspace {
    pub fn toggle_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editor_mode && !self.active_id().is_some_and(|id| self.is_running(id, cx)) {
            self.toast("Open a Claude session to use the editor.", false, cx);
            return;
        }
        self.editor_mode = !self.editor_mode;
        if self.editor_mode {
            self.refresh_files(cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Editor mode is on and has a session to show; the home page keeps its sidebars.
    pub fn editing(&self) -> bool {
        self.editor_mode && self.active_id().is_some()
    }

    /// The pane of the shown session while editor mode is on, created on first use.
    pub fn shown_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<(Id, &EditorPane)> {
        let id = self.active_id().filter(|_| self.editing()).cloned()?;
        if !self.panes.contains_key(&id) {
            let tree = cx.new(|cx| TreeState::new(cx));
            let buffer = cx.new(|cx| EditorState::new(window, cx));
            let owner = id.clone();
            let sub = cx.subscribe(&tree, move |ws: &mut Workspace, _, ev: &TreeEvent, _| {
                if let Some(p) = ws.panes.get_mut(&owner) {
                    match ev {
                        TreeEvent::Expanded(path) => p.expanded.insert(path.to_string()),
                        TreeEvent::Collapsed(path) => p.expanded.remove(path.as_ref()),
                    };
                }
            });
            let pane = EditorPane {
                tree,
                buffer,
                nodes: vec![],
                changed: Default::default(),
                truncated: false,
                expanded: HashSet::new(),
                file: None,
                note: None,
                listing: false,
                _sub: sub,
            };
            self.panes.insert(id.clone(), pane);
            self.refresh_files(cx);
        }
        let pane = self.panes.get(&id)?;
        Some((id, pane))
    }

    /// List the shown session's folder again (files Claude added, changed marks), off the UI thread.
    pub(super) fn refresh_files(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().filter(|_| self.editor_mode).cloned() else { return };
        let Some(root) = self.store.session(&id).map(|s| s.cwd.clone()) else { return };
        let Some(pane) = self.panes.get_mut(&id).filter(|p| !p.listing) else { return };
        pane.listing = true;
        cx.spawn(async move |this, cx| {
            let listing = cx.background_executor().spawn(async move { files::list(&root, LIST_LIMIT) }).await;
            let _ = this.update(cx, |ws, cx| {
                let Some(pane) = ws.panes.get_mut(&id) else { return };
                pane.listing = false;
                pane.changed = std::sync::Arc::new(listing.changed);
                pane.truncated = listing.truncated;
                if pane.nodes != listing.nodes {
                    let items = tree_items(&listing.nodes, &pane.expanded);
                    let selected = pane.file.as_ref().map(|f| f.to_string_lossy().to_string());
                    pane.tree.update(cx, |t, cx| {
                        t.set_items(items, cx);
                        let ix = selected.and_then(|s| t.index_of(&s.into()));
                        t.set_selected_index(ix, cx);
                    });
                    pane.nodes = listing.nodes;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Show `path` in the session's editor, or a note saying why it can't be shown.
    pub fn open_file(&mut self, id: &Id, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.clone();
        let read = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let contents = cx.background_executor().spawn(async move { files::read(&read) }).await;
            let _ = this.update_in(cx, |ws, window, cx| {
                let Some(pane) = ws.panes.get_mut(&id) else { return };
                pane.note = match contents {
                    Ok(Contents::Text(text)) => {
                        pane.buffer.update(cx, |b, cx| b.set_value(text, window, cx));
                        None
                    }
                    Ok(Contents::TooLarge) => Some(format!(
                        "This file is too large for the editor (over {} lines or {} MB).",
                        files::MAX_LINES,
                        files::MAX_BYTES / (1024 * 1024)
                    )),
                    Ok(Contents::NotText) => Some("This file isn't UTF-8 text.".into()),
                    Err(e) => Some(format!("Could not read this file: {e}")),
                };
                pane.file = Some(path);
                cx.notify();
            });
        })
        .detach();
    }
}

fn tree_items(nodes: &[Node], expanded: &HashSet<String>) -> Vec<TreeItem> {
    nodes
        .iter()
        .map(|n| {
            let id = n.path.to_string_lossy().to_string();
            let open = expanded.contains(&id);
            let item = TreeItem::new(id, n.name.clone());
            match &n.children {
                Some(children) => item.children(tree_items(children, expanded)).expanded(open),
                None => item,
            }
        })
        .collect()
}
