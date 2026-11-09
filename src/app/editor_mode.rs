//! Editor mode: the shown session's terminal narrows to the left, and its folder's files and an editor fill the rest.
//! Each session keeps its own pane while it runs; listing and reading files happen off the UI thread (see `files`).
//! Saving and following changes on disk are in `editing.rs`; the pane is drawn by `panels/editor_pane.rs`.

use std::path::Path;
use std::time::{Duration, Instant};

use super::editing::Conflict;
use super::*;
use crate::editor::Editor;
use crate::files::{self, Change, Contents, Node, Stamp};
use crate::platform::path_key;

/// Most entries listed in the tree, so a session started in a huge plain folder (or the home folder) stays usable.
const LIST_LIMIT: usize = 20_000;
/// The shortest time between two listings the folder watcher asks for (see `EditorPane::listed_at`).
const RELIST_GAP: Duration = Duration::from_secs(2);
/// A watched folder is listed again this often even when its watcher reports nothing.
const WATCHED_RELIST: Duration = Duration::from_secs(30);

/// One row of the file tree as it is shown: the open folders' contents under them.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub folder: bool,
    pub open: bool,
    pub change: Option<Change>,
}

/// Where the keyboard is in the pane when the editor doesn't have it: the file tree, so the pane's shortcuts work.
pub fn tree_key(id: &str) -> String {
    format!("files:{id}")
}

/// An entry of the editor's right-click menu (`Workspace::open_editor_menu`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EditorCommand {
    GoToDefinition,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Ask(super::Ask),
}

/// A session's files and editor. Never persisted; it goes away with the session.
pub struct EditorPane {
    /// Open files, in the order they were opened, and the one shown.
    pub tabs: Vec<Tab>,
    pub active: Option<usize>,
    /// The listing as last read, and the folder it lists.
    pub nodes: Vec<Node>,
    pub root: PathBuf,
    /// The tree's rows with the open folders' contents, worked out when the listing or an open folder changes.
    pub rows: Vec<TreeRow>,
    /// Bumped whenever `rows` changes, so the tree's list knows its keys moved.
    pub rows_version: u64,
    /// The listing's files, and each one's path under `root` as quick open shows it; worked out once per listing
    /// rather than on every keystroke.
    pub files: std::sync::Arc<Vec<PathBuf>>,
    pub relative: std::sync::Arc<Vec<String>>,
    /// Git's changes by `path_key`, see `files::Listing`.
    pub changed: HashMap<String, Change>,
    pub truncated: bool,
    /// Places the editor left (file and caret line), latest last, for Back; and the ones Back left, for Forward.
    pub back: Vec<(PathBuf, usize)>,
    pub forward: Vec<(PathBuf, usize)>,
    /// Files shown lately, latest first, which quick open lists first.
    pub recent: Vec<PathBuf>,
    /// Folders the user opened, kept open across refreshes.
    pub expanded: HashSet<PathBuf>,
    /// Quick open or find in files, while it is up.
    pub picker: Option<super::picker::Picker>,
    /// A listing is on its way. A refresh asked for meanwhile sets `relist`, because the walk under way may have
    /// passed the change already.
    pub listing: bool,
    pub relist: bool,
    /// Tells the pane when its folder changes on disk (see `watch_folder`); while there is none, every tick lists
    /// the folder again.
    pub watcher: Option<notify::RecommendedWatcher>,
    /// The repository's own folder, whose index and HEAD say when git's view of the files moved (a commit, a checkout).
    pub git_dir: Option<PathBuf>,
    /// The same for the repositories nested in the folder, as the last listing found them.
    pub nested_git_dirs: Vec<PathBuf>,
    /// The folders the tree lists; a change anywhere else (an ignored build folder) changes nothing it shows.
    pub folders: HashSet<PathBuf>,
    /// The watcher saw a change that the listing doesn't have yet.
    pub stale: bool,
    /// When the last listing arrived, so a stream of changes (Claude writing file after file) lists the folder at
    /// most every `RELIST_GAP` rather than after each one.
    pub listed_at: Option<std::time::Instant>,
    /// A listing is set to follow once `RELIST_GAP` has passed.
    pub relist_due: bool,
    /// The language servers this session started, by program (see `lsp::KINDS`).
    pub servers: HashMap<&'static str, super::language_servers::ServerState>,
}

impl EditorPane {
    pub fn shown(&self) -> Option<&Tab> {
        self.tabs.get(self.active?)
    }

    pub fn shown_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active?)
    }

    /// The tree's rows again, after the listing or an open folder changed.
    fn rebuild_rows(&mut self) {
        let mut rows = vec![];
        flatten(&self.nodes, 0, &self.expanded, &self.changed, &mut rows);
        if rows != self.rows {
            self.rows = rows;
            self.rows_version += 1;
        }
    }
}

fn flatten(nodes: &[Node], depth: usize, expanded: &HashSet<PathBuf>, changed: &HashMap<String, Change>, out: &mut Vec<TreeRow>) {
    for n in nodes {
        let open = n.children.is_some() && expanded.contains(&n.path);
        out.push(TreeRow { path: n.path.clone(), name: n.name.clone(), depth, folder: n.children.is_some(), open, change: changed.get(&path_key(&n.path)).copied() });
        if let (true, Some(children)) = (open, &n.children) {
            flatten(children, depth + 1, expanded, changed, out);
        }
    }
}

/// An open file. Its editor keeps the caret, scroll and undo history while other tabs are shown.
pub struct Tab {
    pub path: PathBuf,
    pub editor: Editor,
    /// Why the file's text isn't shown (too large, not text, unreadable); the editor stays empty then.
    pub note: Option<String>,
    /// How the file differs from the editor's text (line ends, tab indents), which saving puts back (see `files::read`).
    pub format: files::Format,
    /// The file on disk as last read or saved; `None` when the user chose to keep their version over a change.
    pub stamp: Option<Stamp>,
    /// Edited since it was last read or saved.
    pub dirty: bool,
    /// A save is being written; the disk check leaves the tab alone until it lands, so the save isn't taken for a
    /// change from outside.
    pub saving: bool,
    pub conflict: Option<Conflict>,
    /// The highlighter's name for the file's language (see `files::language`), which also picks its language server.
    pub language: Option<&'static str>,
    /// The text's version as its language server last heard it (see `language_servers::sync_with_server`).
    pub version: i32,
    /// The language server that has this file open, once it is running.
    pub server: Option<std::sync::Arc<crate::lsp::Server>>,
    /// The search bar over it (`find.rs`).
    pub find: super::find::FindBar,
}

impl Workspace {
    /// Turn editor mode on or off for the shown session.
    pub fn toggle_editor(&mut self) {
        if !self.editor_mode && !self.active_id().is_some_and(|id| self.is_running(id)) {
            self.toast("Open a Claude session to use the editor.", false);
            return;
        }
        self.editor_mode = !self.editor_mode;
        if self.editor_mode {
            self.ensure_pane();
            self.refresh_files();
            self.focus_pane();
        } else {
            self.focus_active();
        }
    }

    /// Editor mode is on and has a session to show; the home page keeps its sidebars.
    pub fn editing(&self) -> bool {
        self.editor_mode && self.main == Main::Terminals && self.active_id().is_some()
    }

    /// The shown session's pane while editor mode is on.
    pub fn shown_pane(&self) -> Option<(&Id, &EditorPane)> {
        let id = self.active_id().filter(|_| self.editing())?;
        Some((id, self.panes.get(id)?))
    }

    /// The pane for the shown session, made on first use.
    fn ensure_pane(&mut self) {
        let Some(id) = self.active_id().filter(|_| self.editing()).cloned() else { return };
        if self.panes.contains_key(&id) {
            return;
        }
        let pane = EditorPane {
            tabs: vec![],
            active: None,
            nodes: vec![],
            root: PathBuf::new(),
            rows: vec![],
            rows_version: 0,
            files: Default::default(),
            relative: Default::default(),
            changed: HashMap::new(),
            truncated: false,
            back: vec![],
            forward: vec![],
            recent: vec![],
            expanded: HashSet::new(),
            picker: None,
            listing: false,
            relist: false,
            watcher: None,
            git_dir: None,
            nested_git_dirs: vec![],
            folders: HashSet::new(),
            stale: false,
            listed_at: None,
            relist_due: false,
            servers: HashMap::new(),
        };
        self.panes.insert(id.clone(), pane);
        self.watch_folder(&id);
    }

    /// The keyboard goes to the shown file's editor, or to the tree when no file is open.
    pub fn focus_pane(&mut self) {
        let Some((id, pane)) = self.shown_pane() else { return };
        let key = pane.shown().filter(|t| t.note.is_none()).map_or_else(|| tree_key(id), |t| t.editor.key.clone());
        self.focus_terminal(&key);
    }

    /// The editor with this key, in any session's pane.
    pub fn editor_mut(&mut self, key: &str) -> Option<&mut Editor> {
        self.panes.values_mut().flat_map(|p| p.tabs.iter_mut()).map(|t| &mut t.editor).find(|e| e.key == key)
    }

    /// The editor's right-click menu at window point `x`, `y`: the usual editing entries, then requests to Claude about
    /// the selection (or the caret's line) and about the language server's problem there.
    pub fn open_editor_menu(&mut self, x: f32, y: f32) {
        use EditorCommand::*;
        use super::Ask;
        let entry = |label: &str, command| MenuItem::new(label, Act::EditorMenu(command));
        let mut items = vec![
            entry("Go to Definition", GoToDefinition),
            entry("Cut", Cut),
            entry("Copy", Copy),
            entry("Paste", Paste),
            entry("Select All", SelectAll),
            entry("Ask Claude to explain this", Ask(Ask::Explain)),
            entry("Ask Claude to fix this", Ask(Ask::Fix)),
            entry("Ask Claude to write tests for this", Ask(Ask::Tests)),
            entry("Ask Claude to fix the problem on this line", Ask(Ask::Problem)),
        ];
        for ix in [1, 5] {
            items[ix].separator_before = true;
        }
        self.open_menu(MenuAnchor::Point(x, y), 280.0, items);
    }

    /// Run a right-click menu entry on the shown file's editor, which the click gave the keyboard.
    pub(super) fn run_editor_command(&mut self, command: EditorCommand) {
        if let EditorCommand::Ask(ask) = command {
            return self.ask_claude(ask);
        }
        let Some(e) = self.active_id().cloned().and_then(|id| self.panes.get_mut(&id)).and_then(|p| p.shown_mut()).map(|t| &mut t.editor) else { return };
        match command {
            EditorCommand::GoToDefinition => e.requests.push(crate::editor::assist::Request::Definition(e.sel.head)),
            EditorCommand::Cut | EditorCommand::Copy => {
                let text = e.take_for_clipboard(command == EditorCommand::Cut);
                self.rt.with_tx(move |tx| tx.copy(&text));
            }
            EditorCommand::Paste => {
                let key = e.key.clone();
                self.rt.with_tx(move |tx| {
                    tx.read_clipboard(move |ws: &mut Workspace, content, _| {
                        if let Some(e) = ws.editor_mut(&key) {
                            e.paste_text(content.text());
                        }
                    })
                });
            }
            EditorCommand::SelectAll => e.select_all(),
            EditorCommand::Ask(_) => {}
        }
    }

    /// Give the keyboard to this editor: text fields let go of it.
    pub fn focus_editor(&mut self, key: &str) {
        self.focus_terminal(key);
    }

    /// Whether the keyboard is in the shown pane (its editor or its tree), where the pane's shortcuts apply.
    pub fn keys_in_pane(&self) -> bool {
        let Some((id, pane)) = self.shown_pane() else { return false };
        let Some(key) = self.keys_to.as_deref() else { return false };
        key == tree_key(id) || pane.tabs.iter().any(|t| t.editor.key == key)
    }

    /// Open or close a folder in the tree.
    pub fn toggle_folder(&mut self, path: &Path) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        if !pane.expanded.remove(path) {
            pane.expanded.insert(path.to_path_buf());
        }
        pane.rebuild_rows();
        self.focus_terminal(&tree_key(&id));
    }

    /// List the shown session's folder again (files Claude added, changed marks), off the UI thread.
    pub(super) fn refresh_files(&mut self) {
        let Some(id) = self.active_id().filter(|_| self.editor_mode).cloned() else { return };
        // A worktree that Claude Code is still making has no folder to list yet.
        let Some(root) = self.store.session(&id).map(|s| s.cwd.clone()).filter(|root| root.is_dir()) else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        if pane.listing {
            pane.relist = true;
            return;
        }
        pane.listing = true;
        self.rt.spawn(move || files::list(&root, LIST_LIMIT), move |ws: &mut Workspace, listing| ws.on_listing(&id, listing));
    }

    fn on_listing(&mut self, id: &Id, listing: files::Listing) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        pane.listing = false;
        pane.listed_at = Some(std::time::Instant::now());
        let relist = std::mem::take(&mut pane.relist);
        pane.changed = listing.changed;
        pane.truncated = listing.truncated;
        pane.nested_git_dirs = listing.nested_git_dirs;
        let moved = !pane.root.as_os_str().is_empty() && pane.root != listing.root;
        pane.root = listing.root;
        if pane.nodes != listing.nodes {
            pane.folders = folders_of(&listing.nodes);
            let all = files::files_of(&listing.nodes);
            pane.relative = std::sync::Arc::new(all.iter().map(|f| relative(f, &pane.root)).collect());
            pane.files = std::sync::Arc::new(all);
            pane.nodes = listing.nodes;
        }
        pane.rebuild_rows();
        if moved {
            // The session went to another folder: the old watcher watches the wrong one.
            pane.watcher = None;
            self.watch_folder(id);
        }
        if relist {
            self.refresh_files();
        }
    }

    /// The tick's look at the shown pane's folder: listed again when its watcher saw a change, every time when it has
    /// no watcher, and now and then regardless, since a watcher can go quiet without a word (network folders).
    pub(super) fn refresh_if_stale(&mut self) {
        let Some(id) = self.active_id().filter(|_| self.editor_mode).cloned() else { return };
        let Some(pane) = self.panes.get_mut(&id) else { return };
        let overdue = pane.listed_at.is_none_or(|t| t.elapsed() >= WATCHED_RELIST);
        if pane.watcher.is_some() && !pane.stale && !overdue {
            return;
        }
        pane.stale = false;
        self.refresh_files();
    }

    /// Watch the session's folder, so the tree and its change marks follow the disk as soon as it changes rather than
    /// on the next tick, and an unchanged folder is never walked again.
    fn watch_folder(&mut self, id: &Id) {
        let Some(root) = self.store.session(id).map(|s| s.cwd.clone()) else { return };
        let mailbox = self.rt.mailbox();
        let owner = id.clone();
        // Starting a watch can take a while on a big tree (Linux adds one per folder), so it happens off the UI thread.
        self.rt.spawn(
            move || start_watch(&root, owner, mailbox),
            {
                let id = id.clone();
                move |ws: &mut Workspace, started| {
                    let Some((watcher, git_dir)) = started else { return };
                    if let Some(p) = ws.panes.get_mut(&id) {
                        // The first listing ran while the watch was starting, so what changed in between is caught by one more.
                        (p.watcher, p.git_dir, p.stale) = (Some(watcher), git_dir, true);
                    }
                }
            },
        );
    }

    fn on_folder_change(&mut self, id: &Id, paths: &[PathBuf]) {
        let shown = self.active_id() == Some(id);
        let Some(pane) = self.panes.get_mut(id) else { return };
        // No paths: the watcher lost track (its buffer overflowed) and anything may have changed.
        let git_dirs: Vec<&Path> = pane.git_dir.iter().chain(&pane.nested_git_dirs).map(PathBuf::as_path).collect();
        if !paths.is_empty() && !paths.iter().any(|p| change_matters(&pane.root, &git_dirs, &pane.folders, pane.truncated, p)) {
            return;
        }
        pane.stale = true;
        if !shown {
            return;
        }
        // Listed a moment ago: listed again once `RELIST_GAP` has passed, with whatever else changed by then.
        let wait = pane.listed_at.map_or(Duration::ZERO, |t| RELIST_GAP.saturating_sub(t.elapsed()));
        if wait.is_zero() {
            self.refresh_if_stale();
        } else if !pane.relist_due {
            pane.relist_due = true;
            let id = id.clone();
            self.rt.after(wait, move |ws: &mut Workspace| {
                if let Some(p) = ws.panes.get_mut(&id) {
                    p.relist_due = false;
                }
                ws.refresh_if_stale();
            });
        }
        self.refresh_marks();
    }

    /// Show `path` in the session's editor at `line` (from 0), as a jump that Back returns from.
    pub fn open_file(&mut self, id: &Id, path: PathBuf, line: Option<usize>) {
        self.remember_place(id, &path, line);
        self.note_recent(id, &path);
        self.show_file(id, path, line);
    }

    /// Show `path` in the session's editor: its tab when it is open, otherwise a new tab once the file is read off the
    /// UI thread. `line` (from 0) puts the caret there.
    pub(super) fn show_file(&mut self, id: &Id, path: PathBuf, line: Option<usize>) {
        let Some(pane) = self.panes.get_mut(id) else { return };
        if let Some(ix) = pane.tabs.iter().position(|t| t.path == path) {
            pane.active = Some(ix);
            if let Some(line) = line {
                pane.tabs[ix].editor.go_to_line(line);
            }
            self.focus_pane();
            self.refresh_marks();
            return;
        }
        let id = id.clone();
        let read = path.clone();
        self.rt.spawn(move || files::read(&read), move |ws: &mut Workspace, contents| ws.on_read(&id, path, line, contents));
    }

    fn on_read(&mut self, id: &Id, path: PathBuf, line: Option<usize>, contents: std::io::Result<Contents>) {
        self.editor_serial += 1;
        let mut editor = Editor::new(format!("editor:{}", self.editor_serial), "");
        let (mut format, mut stamp) = (files::Format::default(), None);
        let note = match contents {
            Ok(Contents::Text { text, format: f, stamp: s }) => {
                editor.set_text(&text);
                if f.tabs.as_ref().is_some_and(|t| t.hard) {
                    editor.indent_unit = crate::editor::TAB_WIDTH;
                }
                (format, stamp) = (f, Some(s));
                None
            }
            Ok(Contents::TooLarge) => Some(format!("This file is too large for the editor (over {} lines or {} MB).", files::MAX_LINES, files::MAX_BYTES / (1024 * 1024))),
            Ok(Contents::NotText) => Some("This file isn't UTF-8 text.".into()),
            Err(e) => Some(format!("Could not read this file: {e}")),
        };
        editor.edited = false;
        if let Some(line) = line {
            editor.go_to_line(line);
        }
        let Some(pane) = self.panes.get_mut(id) else { return };
        // A second click while the file was being read: keep the tab that is already there.
        let opened = pane.tabs.iter().position(|t| t.path == path);
        let ix = opened.unwrap_or_else(|| {
            let language = files::language(&path).filter(|_| note.is_none());
            pane.tabs.push(Tab { path: path.clone(), editor, note, format, stamp, dirty: false, saving: false, conflict: None, language, version: 0, server: None, find: Default::default() });
            pane.tabs.len() - 1
        });
        pane.active = Some(ix);
        if self.active_id() == Some(id) {
            self.focus_pane();
        }
        if opened.is_none() {
            self.attach_server(id, &path);
            self.refresh_marks();
        }
    }

    /// Close tab `ix`; the tab to its right is shown next, or the last one.
    pub fn close_tab(&mut self, id: &Id, ix: usize) {
        let Some(pane) = self.panes.get_mut(id).filter(|p| ix < p.tabs.len()) else { return };
        Self::tell_server_closed(&pane.tabs.remove(ix));
        pane.active = active_after_close(pane.active, ix, pane.tabs.len());
        if self.active_id() == Some(id) && self.editing() {
            self.focus_pane();
        }
    }

    pub fn show_tab(&mut self, id: &Id, ix: usize) {
        if let Some(path) = self.panes.get(id).and_then(|p| p.tabs.get(ix)).map(|t| t.path.clone()) {
            self.remember_place(id, &path, None);
            self.note_recent(id, &path);
        }
        if let Some(pane) = self.panes.get_mut(id).filter(|p| ix < p.tabs.len()) {
            pane.active = Some(ix);
            self.focus_pane();
            self.refresh_marks();
        }
    }

    /// After every handler: an edited tab is marked unsaved (or clean again, undone back to what is saved).
    /// Its language server hears of the change once typing pauses.
    pub(super) fn note_edits(&mut self) {
        let mut edited = vec![];
        for (id, pane) in &mut self.panes {
            for tab in &mut pane.tabs {
                if std::mem::take(&mut tab.editor.edited) {
                    // A tab whose user kept their version over a change on disk stays unsaved until it is saved.
                    tab.dirty = tab.editor.buffer.is_dirty() || tab.stamp.is_none();
                    edited.push((id.clone(), tab.path.clone()));
                }
            }
        }
        for (id, path) in edited {
            self.sync_with_server(&id, &path, false);
        }
    }

    /// Each frame: the shown editor's easing scroll moves on and its caret blinks. Whether that changed what shows,
    /// and when the editor next needs a frame.
    pub(super) fn poll_editors(&mut self) -> (bool, Option<Instant>) {
        let mut changed = false;
        let mut next = None;
        let window_active = self.window_active;
        let keys_to = self.keys_to.clone();
        if let Some(id) = self.active_id().filter(|_| self.editing()).cloned()
            && let Some(tab) = self.panes.get_mut(&id).and_then(|p| p.shown_mut())
        {
            let now = Instant::now();
            if tab.editor.tick() {
                next = Some(now + Duration::from_millis(16));
            }
            let blinking = window_active && keys_to.as_deref() == Some(tab.editor.key.as_str());
            if let Some(at) = tab.editor.blink(now, blinking) {
                next = Some(next.map_or(at, |n: Instant| n.min(at)));
            }
            changed |= std::mem::take(&mut tab.editor.dirty);
            self.colour_shown();
        }
        (changed, next)
    }

    /// Work out the shown file's colours and foldable blocks off the UI thread when its text changed since they were, one run at a time:
    /// edits made meanwhile are coloured by the next run.
    fn colour_shown(&mut self) {
        let Some(id) = self.active_id().cloned() else { return };
        let Some(tab) = self.panes.get_mut(&id).and_then(|p| p.shown_mut()) else { return };
        let Some(language) = tab.language else { return };
        let e = &mut tab.editor;
        let version = e.buffer.version();
        if e.colouring || e.coloured == Some(version) {
            return;
        }
        e.colouring = true;
        let (key, text) = (e.key.clone(), e.buffer.text());
        self.rt.spawn(
            move || (crate::editor::highlight::highlight(language, &text), crate::editor::highlight::fold_spots(language, &text)),
            move |ws: &mut Workspace, (spans, folds)| {
                let Some(e) = ws.editor_mut(&key) else { return };
                e.colouring = false;
                // Colours and folds for older text would be a few letters or lines off; the next frame asks again.
                if e.buffer.version() == version {
                    e.spans = spans.unwrap_or_default();
                    e.set_fold_spots(folds.unwrap_or_default());
                    e.coloured = Some(version);
                    e.dirty = true;
                }
            },
        );
    }

    /// Ctrl+click on a path in a session's terminal: open it in editor mode, at its line (from 1) when one was given.
    /// Text that only looks like a path, naming no file, does nothing.
    pub fn open_in_editor_mode(&mut self, id: &Id, path: &str, line: Option<usize>) -> bool {
        let Some(cwd) = self.store.session(id).map(|s| s.cwd.clone()) else { return false };
        let Some(file) = dunce::canonicalize(cwd.join(path)).ok().filter(|f| f.is_file()) else { return false };
        if self.active_id() != Some(id) {
            return false;
        }
        if !self.editor_mode {
            self.toggle_editor();
        }
        if !self.editing() {
            return false;
        }
        self.open_file(id, file, line.map(|l| l.saturating_sub(1)));
        true
    }
}

/// Start watching `root`; each burst of changes (a save, a build touching many files) reaches the pane of `id` once
/// it settles. Ends when the watcher, kept by the pane, is dropped.
fn start_watch(root: &Path, id: Id, mailbox: rt::Mailbox<Workspace>) -> Option<(notify::RecommendedWatcher, Option<PathBuf>)> {
    use notify::Watcher;
    let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let (tx, rx) = std::sync::mpsc::channel::<Vec<PathBuf>>();
    let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
        // An error says nothing of what changed, so it counts as any change (no paths).
        let paths = match ev {
            Ok(ev) if matches!(ev.kind, notify::EventKind::Access(_)) => return,
            Ok(ev) => ev.paths,
            Err(_) => vec![],
        };
        let _ = tx.send(paths);
    })
    .ok()?;
    watcher.watch(&root, notify::RecursiveMode::Recursive).ok()?;
    // A session started below the repository's top doesn't contain `.git`, so it is watched on its own.
    let git_dir = git2::Repository::discover(&root).ok().and_then(|r| dunce::canonicalize(r.path()).ok());
    if let Some(dir) = git_dir.as_ref().filter(|d| !d.starts_with(&root)) {
        let _ = watcher.watch(dir, notify::RecursiveMode::NonRecursive);
        let _ = watcher.watch(&dir.join("refs"), notify::RecursiveMode::Recursive);
    }
    std::thread::spawn(move || {
        // Ends when the watcher, and with it the sender, goes away.
        while let Ok(mut paths) = rx.recv() {
            std::thread::sleep(Duration::from_millis(150));
            while let Ok(more) = rx.try_recv() {
                paths.extend(more);
            }
            let id = id.clone();
            if !mailbox.send(move |ws: &mut Workspace| ws.on_folder_change(&id, &paths)) {
                break;
            }
        }
    });
    Some((watcher, git_dir))
}

/// `path` under `root`, with `/` between folders, as quick open and tooltips show it.
pub fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
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
/// in the index or HEAD of one of `git_dirs` (the tree's change marks). A build writing into an ignored folder (`target`,
/// `node_modules`) is neither, and must not set off a walk of the whole folder every moment it runs. With the
/// listing incomplete or not there yet, every change counts.
fn change_matters(root: &Path, git_dirs: &[&Path], folders: &HashSet<PathBuf>, truncated: bool, path: &Path) -> bool {
    if truncated || root.as_os_str().is_empty() {
        return true;
    }
    let parent = path.parent();
    if let Some(git) = git_dirs.iter().find(|g| path.starts_with(g)) {
        // The index and the branch the work tree is on; objects and logs churn without changing either.
        let named = |names: &[&str]| path.file_name().is_some_and(|n| names.iter().any(|m| n == *m));
        return (parent == Some(git) && named(&["index", "HEAD", "packed-refs"])) || path.starts_with(git.join("refs"));
    }
    parent.is_some_and(|dir| dir == root || folders.contains(dir))
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
        let nested = Path::new("/w/lib/.git");
        let matters = |p: &str| change_matters(root, &[git, nested], &folders, false, Path::new(p));
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
        assert!(matters("/w/lib/.git/index"), "a commit in a repository nested in the folder");
        assert!(!matters("/w/lib/.git/objects/ab/cdef"));
        assert!(change_matters(root, &[git], &folders, true, Path::new("/w/target/x")), "a cut-short listing may miss any folder");
        assert!(change_matters(Path::new(""), &[], &folders, false, Path::new("/w/target/x")), "nothing listed yet");
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
