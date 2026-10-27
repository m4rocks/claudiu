//! Editor mode: the shown session's terminal narrows to the left, and its folder's files and an editor fill the rest.
//! Each session keeps its own pane while it runs; listing and reading files happen off the UI thread (see `files`).

use gpui_kit::component::input::{EditorState, InputEvent, Position, TabSize};
use gpui_kit::component::tree::{TreeEvent, TreeItem, TreeState};

use std::path::Path;

use futures::StreamExt;

use super::*;
use super::editing::Conflict;
use crate::files::{self, Contents, Node, Stamp};

/// Most entries listed in the tree, so a session started in a huge plain folder (or the home folder) stays usable.
const LIST_LIMIT: usize = 20_000;
/// The shortest time between two listings the folder watcher asks for (see `EditorPane::listed_at`).
const RELIST_GAP: Duration = Duration::from_secs(2);
/// A watched folder is listed again this often even when its watcher reports nothing.
const WATCHED_RELIST: Duration = Duration::from_secs(30);

/// A session's files and editor. Never persisted; it goes away with the session.
pub struct EditorPane {
    /// The pane as a whole (its file tree): the keyboard needs to be inside the pane for its shortcuts, such as Ctrl+P,
    /// which otherwise belong to Claude Code.
    pub focus: FocusHandle,
    pub tree: Entity<TreeState>,
    /// Open files, in the order they were opened, and the one shown.
    pub tabs: Vec<Tab>,
    /// Smooth wheel scrolling of the tab bar.
    pub tabs_scroll: crate::panels::glide::Smooth,
    /// Smooth wheel scrolling of the file tree.
    pub tree_scroll: crate::panels::glide::Smooth,
    pub active: Option<usize>,
    /// The listing the tree was last given, to skip redrawing it when nothing changed, and the folder it lists.
    pub nodes: Vec<Node>,
    pub root: PathBuf,
    /// The listing's files, and each one's path under `root` as quick open shows it; worked out once per listing
    /// rather than on every keystroke.
    pub files: std::sync::Arc<Vec<PathBuf>>,
    pub relative: std::sync::Arc<Vec<String>>,
    /// Git's changes by `path_key`, see `files::Listing`.
    pub changed: std::sync::Arc<HashMap<String, files::Change>>,
    pub truncated: bool,
    /// Places the editor left (file and cursor line), latest last, for Back; and the ones Back left, for Forward.
    pub back: Vec<(PathBuf, u32)>,
    pub forward: Vec<(PathBuf, u32)>,
    /// Files shown lately, latest first, which quick open lists first.
    pub recent: Vec<PathBuf>,
    /// Folders the user opened, kept open across refreshes.
    pub expanded: HashSet<String>,
    /// Tree ids of the listed folders with nothing in them, which the tree would otherwise take for files, since it
    /// counts an item with no children as one.
    pub empty_folders: std::sync::Arc<HashSet<String>>,
    /// A listing is on its way. A refresh asked for meanwhile sets `relist`, because the walk under way may have
    /// passed the change already.
    pub listing: bool,
    pub relist: bool,
    /// Quick open or find in files, while it is up.
    pub picker: Option<super::picker::Picker>,
    /// Language servers by program name; dropping the pane (the session ended) stops them.
    pub servers: HashMap<&'static str, super::language_servers::ServerState>,
    /// Tells the pane when its folder changes on disk (see `watch_folder`); while there is none, every tick lists
    /// the folder again.
    pub watcher: Option<notify::RecommendedWatcher>,
    /// The repository's own folder, whose index and HEAD say when git's view of the files moved (a commit, a checkout).
    pub git_dir: Option<PathBuf>,
    /// The folders the tree lists; a change anywhere else (an ignored build folder) changes nothing it shows.
    pub folders: HashSet<PathBuf>,
    /// The watcher saw a change that the listing doesn't have yet.
    pub stale: bool,
    /// When the last listing arrived, so a stream of changes (Claude writing file after file) lists the folder at
    /// most every `RELIST_GAP` rather than after each one.
    pub listed_at: Option<std::time::Instant>,
    /// A listing is set to follow once `RELIST_GAP` has passed.
    pub relist_due: bool,
    pub _sub: Subscription,
}

/// An open file. Its buffer keeps the cursor, scroll and undo history while other tabs are shown.
pub struct Tab {
    pub path: PathBuf,
    pub buffer: Entity<EditorState>,
    /// Why the file's text isn't shown (too large, not text, unreadable); the buffer stays empty then.
    pub note: Option<String>,
    /// How the file differs from the buffer (line ends, tab indents), which saving puts back (see `files::read`).
    pub format: files::Format,
    /// The file on disk as last read or saved; `None` when the user chose to keep their version over a change.
    pub stamp: Option<Stamp>,
    /// Edited since it was last read or saved.
    pub dirty: bool,
    /// The lines that differ from the last commit, as of the last read or save of the file (`git::lines`).
    pub marks: std::sync::Arc<Vec<crate::git::lines::Change>>,
    /// The smooth wheel scrolling of the text (see `panels::editor_pane::change_marks`, which also hosts it).
    pub follow: std::rc::Rc<std::cell::RefCell<crate::panels::glide::Follow>>,
    /// A save is being written; the disk check leaves the tab alone until it lands, so the save isn't taken for a
    /// change from outside.
    pub saving: bool,
    pub conflict: Option<Conflict>,
    /// The highlighter's name for the file's language (see `files::language`), which also picks its language server.
    pub language: Option<&'static str>,
    /// The document version the language server last heard of, and the server, once it has the file.
    pub version: i32,
    pub server: Option<std::rc::Rc<crate::lsp::Server>>,
    pub _sub: Option<Subscription>,
}

impl EditorPane {
    pub fn shown(&self) -> Option<&Tab> {
        self.tabs.get(self.active?)
    }
}

/// Add the grammars gpui-kit doesn't ship to the highlighter (see `files::language` for the names). Once, at startup.
pub fn register_languages() {
    use gpui_kit::component::highlighter::{LanguageConfig, LanguageRegistry};
    let gleam = LanguageConfig::new(
        "gleam",
        tree_sitter_gleam::LANGUAGE.into(),
        vec![],
        tree_sitter_gleam::HIGHLIGHT_QUERY,
        "",
        tree_sitter_gleam::LOCALS_QUERY,
    );
    let registry = LanguageRegistry::singleton();
    registry.register("gleam", &gleam);
    // gpui-kit's TSX takes tree-sitter-typescript's highlights, which only add TypeScript's extras to JavaScript's, so
    // alone they leave most of a .tsx file uncoloured; and neither TSX nor JavaScript (which .jsx files use) colours
    // JSX. TSX gets TypeScript's full query instead (its grammar is TypeScript's plus JSX), and both get JSX's.
    let (Some(typescript), Some(tsx), Some(javascript)) = (registry.language("typescript"), registry.language("tsx"), registry.language("javascript")) else {
        return;
    };
    let with_jsx = |config: LanguageConfig, highlights: &str| LanguageConfig { highlights: format!("{highlights}\n{JSX_HIGHLIGHTS}").into(), ..config };
    registry.register("tsx", &with_jsx(LanguageConfig { injection_languages: typescript.injection_languages.clone(), injections: typescript.injections.clone(), ..tsx }, &typescript.highlights));
    if !javascript.highlights.contains(JSX_HIGHLIGHTS) {
        registry.register("javascript", &with_jsx(javascript.clone(), &javascript.highlights));
    }
}

/// tree-sitter-javascript's `highlights-jsx.scm`, which gpui-kit leaves out.
const JSX_HIGHLIGHTS: &str = r#"
(jsx_opening_element (identifier) @tag (#match? @tag "^[a-z][^.]*$"))
(jsx_closing_element (identifier) @tag (#match? @tag "^[a-z][^.]*$"))
(jsx_self_closing_element (identifier) @tag (#match? @tag "^[a-z][^.]*$"))

(jsx_attribute (property_identifier) @attribute)
(jsx_opening_element (["<" ">"]) @punctuation.bracket)
(jsx_closing_element (["</" ">"]) @punctuation.bracket)
(jsx_self_closing_element (["<" "/>"]) @punctuation.bracket)
"#;

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

    /// A path the user Ctrl+clicked in the session's terminal: open it in editor mode, at its line (1-based) when one was
    /// given. Text that only looks like a path, naming no file, does nothing.
    pub fn open_from_terminal(&mut self, id: &Id, path: &str, line: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.store.session(id).map(|s| s.cwd.clone()) else { return };
        let Some(file) = dunce::canonicalize(cwd.join(path)).ok().filter(|f| f.is_file()) else { return };
        if self.active_id() != Some(id) {
            return;
        }
        if !self.editor_mode {
            self.toggle_editor(window, cx);
        }
        if self.shown_pane(cx).is_none() {
            return;
        }
        self.open_file(id, file, line.map(|l| l.saturating_sub(1)), window, cx);
    }

    /// Editor mode is on and has a session to show; the home page keeps its sidebars.
    pub fn editing(&self) -> bool {
        self.editor_mode && self.active_id().is_some()
    }

    /// The pane of the shown session while editor mode is on, created on first use.
    pub fn shown_pane(&mut self, cx: &mut Context<Self>) -> Option<(Id, &EditorPane)> {
        let id = self.active_id().filter(|_| self.editing()).cloned()?;
        if !self.panes.contains_key(&id) {
            let tree = cx.new(|cx| TreeState::new(cx));
            let owner = id.clone();
            let sub = cx.subscribe(&tree, move |ws: &mut Workspace, _, ev: &TreeEvent, _| {
                if let Some(p) = ws.panes.get_mut(&owner) {
                    match ev {
                        TreeEvent::Expanded(path) => p.expanded.insert(path.to_string()),
                        TreeEvent::Collapsed(path) => p.expanded.remove(path.as_ref()),
                    };
                }
            });
            let tree_scroll = crate::panels::glide::Smooth::around(tree.read(cx).scroll_handle().0.borrow().base_handle.clone());
            let pane = EditorPane {
                focus: cx.focus_handle(),
                tree,
                tabs: vec![],
                tabs_scroll: crate::panels::glide::Smooth::horizontal(),
                tree_scroll,
                active: None,
                nodes: vec![],
                root: PathBuf::new(),
                files: Default::default(),
                relative: Default::default(),
                changed: Default::default(),
                truncated: false,
                back: vec![],
                forward: vec![],
                recent: vec![],
                expanded: HashSet::new(),
                empty_folders: Default::default(),
                listing: false,
                relist: false,
                picker: None,
                servers: HashMap::new(),
                watcher: None,
                git_dir: None,
                folders: HashSet::new(),
                stale: false,
                listed_at: None,
                relist_due: false,
                _sub: sub,
            };
            self.panes.insert(id.clone(), pane);
            self.watch_folder(&id, cx);
            self.refresh_files(cx);
        }
        let pane = self.panes.get(&id)?;
        Some((id, pane))
    }

    /// List the shown session's folder again (files Claude added, changed marks), off the UI thread.
    pub(super) fn refresh_files(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().filter(|_| self.editor_mode).cloned() else { return };
        // A worktree that Claude Code is still making has no folder to list yet.
        let Some(root) = self.store.session(&id).map(|s| s.cwd.clone()).filter(|root| root.is_dir()) else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        if pane.listing {
            pane.relist = true;
            return;
        }
        pane.listing = true;
        cx.spawn(async move |this, cx| {
            let listing = cx.background_executor().spawn(async move { files::list(&root, LIST_LIMIT) }).await;
            let _ = this.update(cx, |ws, cx| {
                let Some(pane) = ws.panes.get_mut(&id) else { return };
                pane.listing = false;
                pane.listed_at = Some(std::time::Instant::now());
                let relist = std::mem::take(&mut pane.relist);
                pane.changed = std::sync::Arc::new(listing.changed);
                pane.truncated = listing.truncated;
                let moved = !pane.root.as_os_str().is_empty() && pane.root != listing.root;
                pane.root = listing.root;
                if pane.nodes != listing.nodes {
                    let items = tree_items(&listing.nodes, &pane.expanded);
                    let selected = pane.shown().map(|t| t.path.to_string_lossy().to_string());
                    pane.tree.update(cx, |t, cx| {
                        t.set_items(items, cx);
                        let ix = selected.and_then(|s| t.index_of(&s.into()));
                        t.set_selected_index(ix, cx);
                    });
                    pane.empty_folders = std::sync::Arc::new(empty_folders(&listing.nodes));
                    pane.folders = folders_of(&listing.nodes);
                    let all = files::files_of(&listing.nodes);
                    pane.relative = std::sync::Arc::new(all.iter().map(|f| super::picker::relative(f, &pane.root)).collect());
                    pane.files = std::sync::Arc::new(all);
                    pane.nodes = listing.nodes;
                }
                if moved {
                    // The session went to another folder: the old watcher watches the wrong one.
                    if let Some(pane) = ws.panes.get_mut(&id) {
                        pane.watcher = None;
                    }
                    ws.watch_folder(&id, cx);
                }
                if relist {
                    ws.refresh_files(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The tick's look at the shown pane's folder: listed again when its watcher saw a change, every time when it has
    /// no watcher, and now and then regardless, since a watcher can go quiet without a word (network folders).
    pub(super) fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().filter(|_| self.editor_mode).cloned() else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        let overdue = pane.listed_at.is_none_or(|t| t.elapsed() >= WATCHED_RELIST);
        if pane.watcher.is_some() && !pane.stale && !overdue {
            return;
        }
        pane.stale = false;
        self.refresh_files(cx);
    }

    /// Watch the session's folder, so the tree and its change marks follow the disk as soon as it changes rather than
    /// on the next tick, and an unchanged folder is never walked again.
    fn watch_folder(&mut self, id: &Id, cx: &mut Context<Self>) {
        let Some(root) = self.store.session(id).map(|s| s.cwd.clone()) else { return };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
        let id = id.clone();
        cx.spawn(async move |this, cx| {
            // Starting a watch can take a while on a big tree (Linux adds one per folder), so it happens off the UI thread.
            let started = cx
                .background_executor()
                .spawn(async move {
                    use notify::Watcher;
                    let root = dunce::canonicalize(&root).unwrap_or(root);
                    let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
                        // An error says nothing of what changed, so it counts as any change (no paths).
                        let paths = match ev {
                            Ok(ev) if matches!(ev.kind, notify::EventKind::Access(_)) => return,
                            Ok(ev) => ev.paths,
                            Err(_) => vec![],
                        };
                        let _ = tx.unbounded_send(paths);
                    })
                    .ok()?;
                    watcher.watch(&root, notify::RecursiveMode::Recursive).ok()?;
                    // A session started below the repository's top doesn't contain `.git`, so it is watched on its own.
                    let git_dir = git2::Repository::discover(&root).ok().and_then(|r| dunce::canonicalize(r.path()).ok());
                    if let Some(dir) = git_dir.as_ref().filter(|d| !d.starts_with(&root)) {
                        let _ = watcher.watch(dir, notify::RecursiveMode::NonRecursive);
                        let _ = watcher.watch(&dir.join("refs"), notify::RecursiveMode::Recursive);
                    }
                    Some((watcher, git_dir))
                })
                .await;
            let Some((watcher, git_dir)) = started else { return };
            // The first listing ran while the watch was starting, so what changed in between is caught by one more.
            let kept = this.update(cx, |ws, _| ws.panes.get_mut(&id).map(|p| (p.watcher, p.git_dir, p.stale) = (Some(watcher), git_dir, true)));
            if !matches!(kept, Ok(Some(()))) {
                return;
            }
            // Ends when the pane, and with it the watcher, goes away.
            while let Some(mut paths) = rx.next().await {
                // A save or a build touches many files at once: let the burst settle and take it as one change.
                cx.background_executor().timer(Duration::from_millis(150)).await;
                while let Ok(more) = rx.try_recv() {
                    paths.extend(more);
                }
                if this.update(cx, |ws, cx| ws.on_folder_change(&id, &paths, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_folder_change(&mut self, id: &Id, paths: &[PathBuf], cx: &mut Context<Self>) {
        let shown = self.active_id() == Some(id);
        let Some(pane) = self.panes.get_mut(id) else { return };
        // No paths: the watcher lost track (its buffer overflowed) and anything may have changed.
        if !paths.is_empty() && !paths.iter().any(|p| change_matters(&pane.root, pane.git_dir.as_deref(), &pane.folders, pane.truncated, p)) {
            return;
        }
        pane.stale = true;
        if !shown {
            return;
        }
        // Listed a moment ago: listed again once `RELIST_GAP` has passed, with whatever else changed by then.
        let wait = pane.listed_at.map_or(Duration::ZERO, |t| RELIST_GAP.saturating_sub(t.elapsed()));
        if wait.is_zero() {
            self.refresh_if_stale(cx);
        } else if !pane.relist_due {
            pane.relist_due = true;
            let id = id.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |ws, cx| {
                    if let Some(p) = ws.panes.get_mut(&id) {
                        p.relist_due = false;
                    }
                    ws.refresh_if_stale(cx);
                });
            })
            .detach();
        }
        self.refresh_marks(cx);
    }

    /// Re-read, off the UI thread, which lines of the shown file differ from the last commit.
    pub(super) fn refresh_marks(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_id().filter(|_| self.editor_mode).cloned() else { return };
        let Some(path) = self.panes.get(&id).and_then(|p| p.shown()).filter(|t| t.note.is_none()).map(|t| t.path.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let read = path.clone();
            let marks = cx.background_executor().spawn(async move { crate::git::lines::changed_lines(&read) }).await;
            let _ = this.update(cx, |ws, cx| {
                let Some(tab) = ws.panes.get_mut(&id).and_then(|p| p.tabs.iter_mut().find(|t| t.path == path)) else { return };
                if *tab.marks != marks {
                    tab.marks = std::sync::Arc::new(marks);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Show `path` in the session's editor at `line` (0-based), as a jump that Back returns from.
    pub fn open_file(&mut self, id: &Id, path: PathBuf, line: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_place(id, &path, line, cx);
        self.note_recent(id, &path);
        self.show_file(id, path, line, window, cx);
    }

    /// Show `path` in the session's editor: its tab when it is open, otherwise a new tab once the file is read off the UI
    /// thread. `line` (0-based) puts the cursor there.
    pub(super) fn show_file(&mut self, id: &Id, path: PathBuf, line: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        if let Some(ix) = pane.tabs.iter().position(|t| t.path == path) {
            pane.active = Some(ix);
            let buffer = pane.tabs[ix].buffer.clone();
            buffer.update(cx, |b, cx| match line {
                Some(line) => b.set_cursor_position(Position::new(line as u32, 0), window, cx),
                None => b.focus(window, cx),
            });
            cx.notify();
            return;
        }
        let id = id.clone();
        let read = path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let contents = cx.background_executor().spawn(async move { files::read(&read) }).await;
            let _ = this.update_in(cx, |ws, window, cx| {
                let language = files::language(&path);
                let buffer = cx.new(|cx| {
                    let state = EditorState::new(window, cx);
                    match language {
                        Some(language) => state.language(language),
                        None => state,
                    }
                });
                let (mut format, mut stamp, mut sub) = (files::Format::default(), None, None);
                let note = match contents {
                    Ok(Contents::Text { text, format: f, stamp: s }) => {
                        buffer.update(cx, |b, cx| {
                            b.set_tab_size(tab_size(&f), cx);
                            b.set_value(text, window, cx);
                            b.set_cursor_position(Position::new(line.unwrap_or(0) as u32, 0), window, cx);
                        });
                        // `set_value` (opening, reloading) sends no Change, so only the user's edits mark the tab.
                        sub = Some(cx.subscribe(&buffer, |ws: &mut Workspace, buffer, ev: &InputEvent, cx| {
                            if !matches!(ev, InputEvent::Change) {
                                return;
                            }
                            let edited = ws.panes.iter_mut().find_map(|(owner, p)| {
                                let tab = p.tabs.iter_mut().find(|t| t.buffer == buffer)?;
                                tab.dirty = true;
                                Some((owner.clone(), tab.path.clone()))
                            });
                            if let Some((owner, path)) = edited {
                                ws.sync_with_server(&owner, &path, false, cx);
                                cx.notify();
                            }
                        }));
                        (format, stamp) = (f, Some(s));
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
                let Some(pane) = ws.panes.get_mut(&id) else { return };
                // A second click while the file was being read: keep the tab that is already there.
                let opened = pane.tabs.iter().position(|t| t.path == path);
                let ix = opened.unwrap_or_else(|| {
                    let language = language.filter(|_| note.is_none());
                    pane.tabs.push(Tab { path: path.clone(), buffer, note, format, stamp, dirty: false, marks: Default::default(), follow: Default::default(), saving: false, conflict: None, language, version: 0, server: None, _sub: sub });
                    pane.tabs.len() - 1
                });
                pane.active = Some(ix);
                let shown = pane.tabs[ix].buffer.clone();
                shown.update(cx, |b, cx| b.focus(window, cx));
                if opened.is_none() {
                    ws.attach_server(&id, &path, cx);
                    ws.refresh_marks(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Close tab `ix`; the tab to its right is shown next, or the last one.
    pub fn close_tab(&mut self, id: &Id, ix: usize, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get_mut(id).filter(|p| ix < p.tabs.len()) else { return };
        Self::tell_server_closed(&pane.tabs.remove(ix));
        pane.active = active_after_close(pane.active, ix, pane.tabs.len());
        cx.notify();
    }

    pub fn show_tab(&mut self, id: &Id, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.panes.get(id).and_then(|p| p.tabs.get(ix)).map(|t| t.path.clone()) {
            self.remember_place(id, &path, None, cx);
            self.note_recent(id, &path);
        }
        if let Some(pane) = self.panes.get_mut(id).filter(|p| ix < p.tabs.len()) {
            pane.active = Some(ix);
            pane.tabs[ix].buffer.update(cx, |b, cx| b.focus(window, cx));
            cx.notify();
        }
    }
}

/// What an entry of the editor's right-click menu does.
#[derive(Clone, Copy, Debug)]
pub enum EditorCommand {
    GoToDefinition,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Ask(Ask),
}

impl Workspace {
    /// Run a menu entry on the shown file's editor, which has the keyboard again after the menu closes.
    pub(super) fn run_editor_command(&mut self, command: EditorCommand, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::{Copy, Cut, GoToDefinition, Paste, SelectAll};
        if let EditorCommand::Ask(ask) = command {
            return self.ask_claude(ask, window, cx);
        }
        let Some(buffer) = self.active_id().and_then(|id| self.panes.get(id)).and_then(|p| p.shown()).map(|t| t.buffer.clone()) else { return };
        buffer.update(cx, |b, cx| b.focus(window, cx));
        let action: Box<dyn gpui::Action> = match command {
            EditorCommand::GoToDefinition => Box::new(GoToDefinition),
            EditorCommand::Cut => Box::new(Cut),
            EditorCommand::Copy => Box::new(Copy),
            EditorCommand::Paste => Box::new(Paste),
            EditorCommand::SelectAll => Box::new(SelectAll),
            EditorCommand::Ask(_) => return,
        };
        window.dispatch_action(action, cx);
    }
}

/// A file indented with tabs is shown with `files::TAB_WIDTH` spaces for each tab, so the Tab key indents by as many.
pub(super) fn tab_size(format: &files::Format) -> TabSize {
    match &format.tabs {
        Some(indents) if indents.hard => TabSize { tab_size: files::TAB_WIDTH, hard_tabs: false },
        _ => TabSize::default(),
    }
}

fn folders_of(nodes: &[Node]) -> HashSet<PathBuf> {
    let mut folders = HashSet::new();
    for n in nodes {
        if let Some(children) = &n.children {
            folders.insert(n.path.clone());
            folders.extend(folders_of(children));
        }
    }
    folders
}

/// Whether a change at `path` can change what a pane listing `root` shows: something in a folder the tree lists, or
/// in git's index or HEAD (the tree's change marks). A build writing into an ignored folder (`target`,
/// `node_modules`) is neither, and must not set off a walk of the whole folder every moment it runs. With the
/// listing incomplete or not there yet, every change counts.
fn change_matters(root: &Path, git_dir: Option<&Path>, folders: &HashSet<PathBuf>, truncated: bool, path: &Path) -> bool {
    if truncated || root.as_os_str().is_empty() {
        return true;
    }
    let parent = path.parent();
    if let Some(git) = git_dir.filter(|g| path.starts_with(g)) {
        // The index and the branch the work tree is on; objects and logs churn without changing either.
        let named = |names: &[&str]| path.file_name().is_some_and(|n| names.iter().any(|m| n == *m));
        return (parent == Some(git) && named(&["index", "HEAD", "packed-refs"])) || path.starts_with(git.join("refs"));
    }
    parent.is_some_and(|dir| dir == root || folders.contains(dir))
}

fn empty_folders(nodes: &[Node]) -> HashSet<String> {
    let mut empty = HashSet::new();
    for n in nodes {
        match &n.children {
            Some(children) if children.is_empty() => {
                empty.insert(n.path.to_string_lossy().to_string());
            }
            Some(children) => empty.extend(empty_folders(children)),
            None => {}
        }
    }
    empty
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

/// The tab shown after tab `closed` goes, given the one shown before and how many are left: the same file when another
/// tab closed, otherwise the tab that took the closed one's place, or the new last one.
fn active_after_close(active: Option<usize>, closed: usize, left: usize) -> Option<usize> {
    let a = active?;
    match a.cmp(&closed) {
        std::cmp::Ordering::Less => Some(a),
        std::cmp::Ordering::Greater => Some(a - 1),
        std::cmp::Ordering::Equal => (left > 0).then(|| closed.min(left - 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::{active_after_close, change_matters};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    #[test]
    fn only_changes_the_tree_or_its_marks_could_show_set_off_a_listing() {
        let root = Path::new("/w");
        let git = Path::new("/w/.git");
        let folders: HashSet<PathBuf> = [PathBuf::from("/w/src")].into();
        let matters = |p: &str| change_matters(root, Some(git), &folders, false, Path::new(p));
        assert!(matters("/w/README.md"));
        assert!(matters("/w/src/new.rs"));
        assert!(matters("/w/src/sub"), "a folder made in a listed one");
        assert!(!matters("/w/target/debug/build.o"), "an ignored folder, which the tree doesn't list");
        assert!(matters("/w/.git/index"));
        assert!(matters("/w/.git/HEAD"));
        assert!(matters("/w/.git/refs/heads/main"), "a branch moved (a reset, a commit from another worktree)");
        assert!(matters("/w/.git/packed-refs"));
        assert!(!matters("/w/.git/objects/ab/cdef"));
        assert!(!matters("/w/.git/logs/HEAD"));
        assert!(change_matters(root, Some(git), &folders, true, Path::new("/w/target/x")), "a cut-short listing may miss any folder");
        assert!(change_matters(Path::new(""), None, &folders, false, Path::new("/w/target/x")), "nothing listed yet");
    }

    /// The highlighter falls back to plain text when a grammar or its query fails to load, so check that each language
    /// `files::language` names really colours code.
    #[test]
    fn every_named_language_highlights() {
        use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
        use gpui_kit::component::input::Rope;
        super::register_languages();
        let samples = [
            ("rust", "fn main() { let x = 1; }"),
            ("typescript", "const x: number = 1;"),
            ("tsx", "const a = <div>{x}</div>;"),
            ("javascript", "function f() { return 1; }"),
            ("json", "{\"a\": 1}"),
            ("toml", "[package]\nname = \"x\""),
            ("yaml", "a: 1"),
            ("markdown", "# Title\n\nSome *text*."),
            ("html", "<p class=\"x\">hi</p>"),
            ("css", "a { color: red; }"),
            ("bash", "echo \"$HOME\""),
            ("gleam", "pub fn main() { let x = 1 }"),
        ];
        let theme = HighlightTheme::default_dark();
        for (lang, code) in samples {
            let mut h = SyntaxHighlighter::new(lang);
            assert_eq!(h.language().as_ref(), lang, "{lang} fell back to plain text");
            let rope = Rope::from(code);
            h.update(None, &rope, None);
            assert!(!h.styles(&(0..code.len()), &*theme).is_empty(), "{lang} has no highlights");
        }
    }

    /// Whether some highlight colours exactly `part` of `code`.
    fn coloured(lang: &str, code: &str, part: &str) -> bool {
        use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
        use gpui_kit::component::input::Rope;
        super::register_languages();
        let mut h = SyntaxHighlighter::new(lang);
        h.update(None, &Rope::from(code), None);
        let start = code.find(part).unwrap();
        let theme = HighlightTheme::default_dark();
        h.styles(&(0..code.len()), &*theme).iter().any(|(range, style)| *range == (start..start + part.len()) && style.color.is_some())
    }

    #[test]
    fn tsx_and_jsx_colour_code_and_tags() {
        let code = "function App() { const s = \"hi\"; return <div className={s}>{s}</div>; }";
        for lang in ["tsx", "javascript"] {
            for part in ["function", "App", "\"hi\"", "div", "className"] {
                assert!(coloured(lang, code, part), "{lang}: {part} has no colour");
            }
        }
    }

    #[test]
    fn closing_a_tab_keeps_the_shown_file_or_moves_to_a_neighbour() {
        // Tabs a b c, b shown.
        assert_eq!(active_after_close(Some(1), 0, 2), Some(0), "closing a: b is still shown");
        assert_eq!(active_after_close(Some(1), 2, 2), Some(1), "closing c: b is still shown");
        assert_eq!(active_after_close(Some(1), 1, 2), Some(1), "closing b: c takes its place");
        assert_eq!(active_after_close(Some(2), 2, 2), Some(1), "closing the last shown tab: the one before it");
        assert_eq!(active_after_close(Some(0), 0, 0), None, "closing the only tab");
        assert_eq!(active_after_close(None, 0, 1), None);
    }
}
