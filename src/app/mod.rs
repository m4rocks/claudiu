//! Workspace: the app's state, the `Act` table that menus, shortcuts and clicks all go through, and `new`. The methods
//! are split by topic: `sessions` (start, show, close), `background` (timers and polls), `layout` (panels),
//! `modal` (toasts, menus, modals), `repo` (git), `github_pane`, `alt_accounts`, `commands` (shortcuts and the menu
//! bar), `input` (where keys and frames go). Rendering lives in `panels/`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use rune::{Icon, TextEdit, Tx};

use crate::claude::accounts;
use crate::claude;
use crate::panels::sidebar::{SIDEBAR_MAX, SIDEBAR_MIN, SIDEBAR_W};
use crate::github::view::{GhEvent, GithubView};
use crate::claude::defaults::{Effort, PermissionMode, Starter};
use crate::claude::mcp;
use crate::git::worktree::Base;
use crate::platform::{self, Editor};
use crate::{claude::usage, datadir};
use crate::store::{
    Account, Attached, ClaudeMeta, ExternalState, Id, MAIN_ACCOUNT, SessionKind, SessionRecord, Store, new_id, now,
};
use crate::terminal::SpawnSpec;
use crate::terminal::view::{TerminalEvent, TerminalView};
use crate::updater::UpdateState;

pub use rt::{HasRt, Rt};

mod alt_accounts;
mod attached_sync;
pub mod commands;
mod config_sync;
mod background;
mod cross;
mod github_pane;
mod handoff;
#[cfg(test)]
mod headless;
mod ingest;
mod input;
mod layout;
mod lifecycle;
mod links;
mod modal;
mod repo;
pub mod rt;
mod send;
mod sessions;
mod settings;
mod setup;
mod titles;
mod visuals;
mod switch;
mod worktrees;

pub use commands::Command;
pub use lifecycle::{saved_window_geom, set_main_window, show_main_window};
pub use settings::SettingsPage;
pub use setup::open_first_setup;

/// Every user-triggerable operation, so buttons, menus and shortcuts share one code path.
#[derive(Clone, Debug)]
pub enum Act {
    NewClaude {
        cwd: PathBuf,
    },
    /// The worktree dialog for a repository's main checkout (see `worktrees.rs`).
    NewWorktree(PathBuf),
    PickBase(Base),
    /// Move the shown session into this working tree of its project (`base` is for a worktree that does not exist yet).
    SwitchWorktree {
        folder: PathBuf,
        base: Base,
    },
    /// `SwitchWorktree`, after the user agreed to leave uncommitted files behind.
    ConfirmSwitch {
        id: Id,
        folder: PathBuf,
        base: Base,
    },
    /// Ask before removing a worktree. A running session in it refuses.
    RemoveWorktree {
        main_checkout: PathBuf,
        folder: PathBuf,
    },
    ConfirmRemoveWorktree {
        main_checkout: PathBuf,
        folder: PathBuf,
        discard: bool,
    },
    /// Ask before deleting a folder in Claudiu's worktrees folder that git no longer lists.
    AskDeleteUnlisted(PathBuf),
    ConfirmDeleteUnlisted(PathBuf),
    /// Show or hide the shell under a Claude session's terminal (started on first use, in the session's folder).
    ToggleShell(Id),
    /// Click on a session row: live -> show it, history -> resume it.
    Select(Id),
    Resume(Id),
    CloseLive(Id),
    Rename(Id),
    /// Hide (true) or restore (false) a session in the sidebar. Archived sessions still turn up in search.
    Archive(Id, bool),
    CopyText(String),
    Reveal(PathBuf),
    OpenEditor(Editor, PathBuf),
    ImportProject,
    OpenSettings,
    /// The setup window again, from Settings or the missing-`claude` toast (see `setup.rs`).
    OpenSetup,
    /// A choice in the Settings dialog's effort or permission-mode picker (None = Claude Code's default); kept in the
    /// dialog until Save.
    PickEffort(Option<Effort>),
    PickPermission(Option<PermissionMode>),
    /// The project's GitHub issues and pull requests.
    OpenGithub {
        cwd: PathBuf,
    },
    /// Have the running session's own mod (Haiku) write a new title from what was said in it.
    RegenTitle(Id),
    /// Change a setting of a running session through its mod (see `usage::send_ctl`).
    SetAutoCompact(Id, bool),
    SetPrecomputeCompact(Id, bool),
    SetAdvisor(Id, String),
    /// Turn Claude in Chrome on or off; the session restarts to apply it.
    SetChrome(Id, bool),
    /// Ask for an advisor model by name.
    AskAdvisor(Id),
    /// Filter the sidebar list to a project (`None`: all).
    FilterProject(Option<Id>),
    /// Stage everything in this folder's repository, commit it with a message the running session's mod (Haiku) writes, push.
    CommitPush(Id, PathBuf),
    Pull(PathBuf),
    SwitchBranch(PathBuf, String),
    /// Read what deleting the local branch would lose, then ask.
    DeleteBranch(PathBuf, String),
    ConfirmDeleteBranch(PathBuf, String),
    /// Ask for a name, then `CreateBranch`.
    NewBranch(PathBuf),
    CreateBranch(PathBuf, String),
    /// New Claude session (Sonnet 5.5, medium effort) that resolves a failed pull / push in this repository.
    FixGitWithClaude(PathBuf),
    AddProject(PathBuf),
    RemoveProject(Id),
    ToggleOlder,
    ConfirmQuit,
    ConfirmInstallUpdate,
    Dismiss,
    InstallUpdate,
    /// New sessions start with this account; running ones keep theirs.
    SwitchAccount(Id),
    /// The shown session runs under another account: restart it under the active one (asks first, see `handoff.rs`).
    Handoff,
    ConfirmHandoff(Id),
    /// A session's request to open another one with a first message, by request id (see `cross.rs`).
    ConfirmStartSession(String),
    /// Shows the sharing warning, then `ConfirmAddAccount`.
    AddAccount,
    ConfirmAddAccount,
    /// Run `claude auth login` for an alt account in a terminal tab.
    SignIn(Id),
    RemoveAccount(Id),
    ConfirmRemoveAccount(Id),
}


impl MenuItem {
    /// A plain entry: not dangerous, enabled, no hint, no separator above.
    pub fn new(label: impl Into<String>, act: Act) -> Self {
        Self { label: label.into(), hint: None, detail: None, act, danger: false, disabled: false, separator_before: false, remove: None, icon: None }
    }
}

pub struct MenuItem {
    pub label: String,
    /// Faint text on the right that is never cut, so a long `label` is shortened with "…" instead.
    pub hint: Option<String>,
    /// Faint second line under the label, for text too long to sit beside it (a project path).
    pub detail: Option<String>,
    pub act: Act,
    pub danger: bool,
    /// Shown greyed out and does nothing when clicked.
    pub disabled: bool,
    pub separator_before: bool,
    /// Shows an x on the row while the pointer is over it; clicking it runs this instead of `act`.
    pub remove: Option<Act>,
    /// Drawn before the hint.
    pub icon: Option<&'static Icon>,
}

/// Where a menu opens: under the element with this key (a button), or at a point in the window (a right click).
#[derive(Clone, Debug, PartialEq)]
pub enum MenuAnchor {
    Key(String),
    Point(f32, f32),
}

pub struct Menu {
    pub anchor: MenuAnchor,
    pub min_w: f32,
    pub items: Vec<MenuItem>,
    /// Bumped for every menu opened, so a new menu pops in again instead of morphing from the last one.
    pub serial: u64,
}

// One modal is open at a time, so the Settings variant's size costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum Modal {
    Confirm {
        title: String,
        body: String,
        confirm: String,
        danger: bool,
        act: Act,
    },
    Rename {
        id: Id,
        input: TextEdit,
    },
    NewBranch {
        cwd: PathBuf,
        input: TextEdit,
    },
    /// Empty name = a generated one.
    NewWorktree {
        main_checkout: PathBuf,
        input: TextEdit,
        base: Base,
    },
    Advisor {
        id: Id,
        input: TextEdit,
    },
    /// Empty text = use the default.
    Settings {
        shell: TextEdit,
        claude: TextEdit,
        /// Model for new sessions (Sessions page); empty = Claude Code's default.
        model: TextEdit,
        effort: Option<Effort>,
        permission: Option<PermissionMode>,
        updates: bool,
        chrome: bool,
        scale: u32,
        /// Claude Code features and tools turned off (Claude Code tools page).
        trimmed: Vec<crate::claude::trim::Trim>,
        /// Claudiu tools turned off (Claudiu tools page).
        claudiu_off: Vec<crate::claude::mcp::Feature>,
        /// Skills to turn off, separated by commas (Claude Code tools page).
        skills_field: TextEdit,
        /// Settings found at open time that make Claude Code cost more (Claude Code tools page).
        costly: Vec<crate::claude::costly::Finding>,
        /// Placeholders: what each field means when left empty.
        hints: [String; 4],
    },
}

/// A field of the open modal, by its place: the dialog's only field is 0; Settings' are shell, claude, model, skills.
pub fn modal_field(modal: &mut Option<Modal>, n: usize) -> Option<&mut TextEdit> {
    match (modal.as_mut()?, n) {
        (Modal::Rename { input, .. } | Modal::NewBranch { input, .. } | Modal::NewWorktree { input, .. } | Modal::Advisor { input, .. }, 0) => Some(input),
        (Modal::Settings { shell, .. }, 0) => Some(shell),
        (Modal::Settings { claude, .. }, 1) => Some(claude),
        (Modal::Settings { model, .. }, 2) => Some(model),
        (Modal::Settings { skills_field, .. }, 3) => Some(skills_field),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Main {
    Home,
    Terminals,
}

/// What a running Claude session is doing, as shown by its sidebar dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Activity {
    /// Output is streaming (spinning icon).
    Working,
    /// Claude Code stopped to wait for the user, a question or a permission (question mark).
    Waiting,
    /// Finished working while the user was looking elsewhere (check mark).
    Attention,
}

/// The git operation running in the background, if any (one at a time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitOp {
    CommitPush,
    Pull,
    Branch,
    Worktree,
}

pub struct Live {
    pub id: Id,
    pub view: TerminalView,
    /// The session name Claude Code was last given (`--name` or `/rename`), so a changed title is sent once.
    pub claude_name: Option<String>,
    /// When Claude last called the title tool. A later call (e.g. after `/clear`, which starts a nameless
    /// Claude session) clears `claude_name` so the title is sent again.
    pub title_stamp: Option<std::time::SystemTime>,
    /// The account this process was started under; a session on another account than the active one can be handed off.
    pub account: Id,
}

/// A project's GitHub view, shown in the right sidebar. Kept while Claudiu runs so each project keeps its place;
/// never written to `state.json`.
pub struct GhPane {
    pub id: Id,
    /// The project's root folder: the pane's repository is whatever `gh` resolves from it.
    pub cwd: PathBuf,
    pub project: String,
    pub view: GithubView,
}

/// The shell under a Claude session's terminal. It belongs to that session (never listed, never persisted), keeps
/// its height while the session lives, and goes away with it.
pub struct ShellPanel {
    pub view: TerminalView,
    pub open: bool,
    pub height: f32,
}

/// The key of the shell terminal under session `id`; a Claude session's terminal is keyed by its id.
pub fn shell_key(id: &str) -> String {
    format!("shell:{id}")
}

pub const SHELL_HEIGHT: f32 = 260.0;
pub const SHELL_MIN: f32 = 120.0;

/// The right sidebar: the menu or the wider GitHub view, each with its own remembered width.
pub const RIGHT_MENU_W: f32 = 272.0;
pub const RIGHT_MENU_MIN: f32 = 240.0;
pub const RIGHT_MENU_MAX: f32 = 480.0;
pub const RIGHT_GH_W: f32 = 560.0;
pub const RIGHT_GH_MIN: f32 = 380.0;

/// A drag of a sidebar's inner edge or the shell divider: the size when it started.
#[derive(Clone, Copy)]
pub struct SideDrag {
    pub size: f32,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub serial: u64,
    /// Button label and what it does; the toast stays up longer so there is time to answer.
    pub action: Option<(String, Act)>,
}

/// A running row being dragged in the sidebar: its id, and how far the pointer has moved since the last swap.
pub struct RowDrag {
    pub id: Id,
    pub dy: f32,
}

pub struct Workspace {
    pub rt: Rt<Workspace>,
    pub store: Store,
    pub claude_exe: Option<PathBuf>,
    pub shell: (String, Vec<String>),
    pub live: Vec<Live>,
    pub gh: Vec<GhPane>,
    /// The GitHub view the right sidebar shows; `None` is its menu.
    pub right_gh: Option<Id>,
    pub right_drag: Option<SideDrag>,
    pub left_drag: Option<SideDrag>,
    pub shell_drag: Option<SideDrag>,
    /// The session shown in the main area.
    pub active: Option<Id>,
    /// Shells under Claude sessions, by the session they belong to.
    pub shells: HashMap<Id, ShellPanel>,
    /// The terminal that gets the keyboard (a session id or `shell_key`) while no text field has it.
    pub keys_to: Option<String>,
    /// A text field should let go of the keyboard after this handler (see `after_input`).
    pub(crate) blur: bool,
    /// The window has the system's keyboard focus.
    pub window_active: bool,
    /// The window's size in points, as last laid out.
    pub window_size: (f32, f32),
    /// Open sessions in the order they were last shown (most recent last); closing one goes back to the previous.
    recent: Vec<Id>,
    pub main: Main,
    pub selected: Option<Id>,
    pub search: TextEdit,
    pub modal: Option<Modal>,
    /// Bumped for every modal opened.
    pub modal_serial: u64,
    /// The Settings category last shown. Kept here because opening Settings builds the modal anew; not saved to disk.
    pub settings_page: SettingsPage,
    /// What the Worktrees page of Settings shows. Not saved to disk.
    pub worktrees_page: worktrees::WorktreesPage,
    pub menu: Option<Menu>,
    pub menu_serial: u64,
    pub toast: Option<Toast>,
    pub toast_serial: u64,
    /// Sessions that leave no history once they end (the "fix with Claude" helper).
    ephemeral: HashSet<Id>,
    /// `claude auth login` tabs; account details are re-read when one ends.
    logins: HashSet<Id>,
    /// Sessions being handed to another account: compacting, then resumed under it (see `handoff.rs`).
    handoffs: HashMap<Id, handoff::Handoff>,
    pub editors: Vec<(Editor, bool)>,
    pub transcript_mtimes: HashMap<Id, SystemTime>,
    pub window_title: String,
    /// The running row being dragged in the sidebar.
    pub dragging: Option<RowDrag>,
    /// Ctrl (Cmd on macOS) has been held a moment, so the running rows show their Ctrl+number shortcut.
    pub shortcuts_shown: bool,
    /// Bumped whenever the modifier keys change, so a pending reveal of the shortcuts is dropped.
    pub shortcuts_wait: u64,
    /// The minute (Unix time / 60) of the last tick's redraw, see `tick`.
    pub tick_minute: i64,
    /// Attached issues and pull requests are asked about on GitHub from time to time, see `attached_sync.rs`.
    pub attached_sync: attached_sync::Sync,
    /// The shared Claude Code settings are kept equal across accounts, see `config_sync.rs`.
    pub config_sync: config_sync::Runner,
    /// Sessions asked to open another with a first message; each waits for the user's yes (`cross.rs`).
    pub pending_starts: Vec<cross::PendingStart>,
    /// Worktree switches the user approved that a session asked for; each waits for the session's turn to end (`cross.rs`).
    pub pending_switches: Vec<cross::PendingSwitch>,
    pub update: UpdateState,
    pub scanning: bool,
    pub quitting: bool,
    /// What `running.json` holds, and whether the saved list has been brought back yet (nothing is written before).
    pub remembered: Vec<Id>,
    pub restored: bool,
    /// Sessions that are working or waiting for the user; absent = idle and already seen.
    pub activity: HashMap<Id, Activity>,
    pub git_busy: Option<GitOp>,
    /// What Commit & Push would do in the shown session's folder, as last read; `None` until it has been.
    pub pending: Option<(PathBuf, crate::git::commit::Pending)>,
    /// Claude Code's session status files as last read (see `start_activity_poll`).
    pub status_files: claude::StatusFiles,
    /// The setup window while it is open, so a second request shows it instead of opening another.
    setup_window: Option<rune::WindowHandle>,
    /// Counts the setup windows opened, for their keys.
    setup_serial: u64,
}

impl HasRt for Workspace {
    fn rt(&mut self) -> &mut Rt<Self> {
        &mut self.rt
    }
}

impl Workspace {
    /// The workspace for the main window. `start` begins the timers and restores the sessions; tests leave it off.
    pub fn new(initial_projects: Vec<PathBuf>) -> Self {
        Self::with_store(initial_projects, Store::load(Store::default_path()))
    }

    /// The workspace over `store`; tests give each workspace a state file of its own.
    fn with_store(initial_projects: Vec<PathBuf>, store: Store) -> Self {
        crate::panels::scale::set(store.data.settings.scale.unwrap_or(100));
        let claude_exe = claude::discover(store.data.settings.claude_path.as_deref());
        let mut ws = Self {
            rt: Rt::default(),
            claude_exe,
            shell: platform::default_shell(store.data.settings.shell.as_deref()),
            live: vec![],
            gh: vec![],
            right_gh: None,
            right_drag: None,
            left_drag: None,
            shell_drag: None,
            active: None,
            shells: HashMap::new(),
            keys_to: None,
            blur: false,
            window_active: true,
            window_size: (1280.0, 800.0),
            recent: vec![],
            main: Main::Home,
            selected: store.data.layout.selected_session.clone(),
            search: TextEdit::new(""),
            modal: None,
            modal_serial: 0,
            settings_page: SettingsPage::default(),
            worktrees_page: Default::default(),
            menu: None,
            menu_serial: 0,
            toast: None,
            toast_serial: 0,
            ephemeral: HashSet::new(),
            logins: HashSet::new(),
            handoffs: HashMap::new(),
            editors: Editor::ALL.iter().map(|e| (*e, e.locate().is_some())).collect(),
            transcript_mtimes: HashMap::new(),
            window_title: String::new(),
            dragging: None,
            shortcuts_shown: false,
            shortcuts_wait: 0,
            tick_minute: 0,
            attached_sync: Default::default(),
            config_sync: Default::default(),
            pending_starts: vec![],
            pending_switches: vec![],
            update: UpdateState::default(),
            scanning: false,
            quitting: false,
            remembered: vec![],
            restored: false,
            activity: HashMap::new(),
            git_busy: None,
            pending: None,
            status_files: Default::default(),
            setup_window: None,
            setup_serial: 0,
            store,
        };
        // Everything from a previous run is history until clicked. Keep the highlight only if it still exists.
        if ws.selected.as_ref().is_some_and(|id| ws.store.session(id).is_none()) {
            ws.selected = None;
        }
        for path in initial_projects {
            ws.store.add_project(&path);
        }
        ws.store.refresh_git();
        // Alt folders the list doesn't know (e.g. lost to an older Claudiu rewriting its state) are adopted again.
        for dir in accounts::folders() {
            if !ws.store.data.accounts.iter().any(|a| a.dir.as_ref() == Some(&dir)) {
                ws.store.data.accounts.push(Account { id: new_id(), dir: Some(dir), ..Default::default() });
                ws.store.mark_dirty();
            }
        }
        ws
    }

    /// Start the background work and bring back the sessions that were open: once the window exists.
    pub fn start(&mut self) {
        self.start_scan();
        self.rt.background(lifecycle::tidy_leftovers);
        self.ingest_usage();
        self.refresh_accounts();
        // After the first frame: starting every saved session is the slow part of coming back.
        self.rt.after(Duration::ZERO, |ws: &mut Workspace| ws.restore_open());
        self.start_ticker();
        self.start_visual_poll();
        self.start_fetch_loop();
        self.start_git_watch();
        self.start_activity_poll();
        self.check_updates();
        self.take_quit_key(40);
    }

    pub fn act(&mut self, act: Act) {
        self.menu = None;
        match act {
            Act::NewClaude { cwd } => self.new_claude(cwd),
            Act::NewWorktree(main_checkout) => self.ask_worktree(main_checkout),
            Act::PickBase(choice) => {
                if let Some(Modal::NewWorktree { base, .. }) = self.modal.as_mut() {
                    *base = choice;
                }
            }
            Act::SwitchWorktree { folder, base } => self.switch_shown(folder, base, false),
            Act::ConfirmSwitch { id, folder, base } => {
                self.modal = None;
                self.switch_session(id, folder, base, true)
            }
            Act::RemoveWorktree { main_checkout, folder } => self.ask_remove_worktree(main_checkout, folder),
            Act::AskDeleteUnlisted(folder) => self.ask_delete_unlisted(folder),
            Act::ConfirmDeleteUnlisted(folder) => {
                self.worktrees_page.asking = None;
                self.run_git(GitOp::Worktree, move || Ok(crate::git::worktree::delete_unlisted(&folder)?));
            }
            Act::ConfirmRemoveWorktree { main_checkout, folder, discard } => {
                self.worktrees_page.asking = None;
                self.run_git(
                    GitOp::Worktree,
                    move || Ok(crate::git::worktree::remove(&main_checkout, &folder, discard, Duration::from_secs(10))?));
            }
            Act::ToggleShell(id) => self.toggle_shell(&id),
            Act::Select(id) => self.show_session(&id),
            Act::Resume(id) => self.resume(&id),
            Act::CloseLive(id) => self.close_live(&id),
            Act::Rename(id) => {
                let text = self
                    .store
                    .session(&id)
                    .map(|s| s.display_title().to_string())
                    .unwrap_or_default();
                let input = modal::field(&text, true);
                self.modal = Some(Modal::Rename { id, input });
            }
            Act::Archive(id, archive) => {
                if let Some(rec) = self.store.session_mut(&id) {
                    rec.hidden = archive;
                }
                if archive && self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = None;
                }
                if archive {
                    self.toast("Archived. Search still finds it; the Claude transcript was not touched.", false);
                }
            }
            Act::CopyText(text) => {
                self.rt.with_tx(move |tx| tx.copy(&text));
                self.toast("Copied", false);
            }
            Act::Reveal(path) => platform::reveal(&path),
            Act::OpenEditor(editor, path) => {
                // The last editor used becomes the "Open in" default.
                if self.store.data.settings.editor != Some(editor) {
                    self.store.data.settings.editor = Some(editor);
                    self.store.mark_dirty();
                }
                if let Err(e) = editor.open(&path) {
                    self.toast(e, true);
                }
            }
            Act::ImportProject => self.import_project(),
            Act::OpenGithub { cwd } => {
                self.open_github(cwd)
            }
            Act::RegenTitle(id) => self.regen_title(&id),
            Act::CommitPush(id, cwd) => {
                if self.is_running(&id) {
                    self.run_git(
                        GitOp::CommitPush,
                        move || crate::git::commit::commit_and_push(&|system, prompt| crate::claude::ask::complete(&id, system, prompt), &cwd).map_err(|e| crate::git::commit::classify(&cwd, e)))
                } else {
                    self.toast("Start the session first: the message is written through the running session.", true)
                }
            }
            Act::Pull(cwd) => self.run_git(
                GitOp::Pull,
                move || crate::git::commit::pull(&cwd).map_err(|e| crate::git::commit::classify(&cwd, e))),
            Act::SwitchBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::git::commit::switch_branch(&cwd, &name)?))
            }
            Act::DeleteBranch(cwd, name) => self.ask_delete_branch(cwd, name),
            Act::ConfirmDeleteBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::git::commit::delete_branch(&cwd, &name)?))
            }
            Act::FixGitWithClaude(cwd) => {
                let prompt = "Claudiu's Pull or Commit & Push just failed in this repository because of a merge conflict, a diverged branch or a rejected push. Run git status, fix it (resolve conflicts keeping the intent of both sides, finish the merge or rebase, then push if the push was rejected). Ask me whenever a conflict is ambiguous or you are unsure which side to keep.";
                match self.running_claude_in(&cwd) {
                    Some(id) => self.send_to_claude(&id, prompt),
                    None => {
                        // No session to ask: a short-lived one on a mid-sized model.
                        let args = ["--model", "claude-sonnet-5-5", "--effort", "medium"];
                        let leading = std::iter::once(prompt).chain(args).map(String::from).collect();
                        let before = self.selected.clone();
                        self.new_claude_with(cwd, leading, Starter::FixGit);
                        if self.selected != before
                            && let Some(id) = self.selected.clone() {
                                self.ephemeral.insert(id);
                            }
                    }
                }
            }
            Act::OpenSetup => self.open_setup(),
            Act::OpenSettings => {
                let s = &self.store.data.settings;
                let (shell_t, claude_t) = (
                    s.shell.clone().unwrap_or_default(),
                    s.claude_path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                );
                let updates = s.update_url.as_deref() != Some("");
                let chrome = s.chrome_default;
                let claude_hint = self.claude_exe.as_ref().map_or("not found".to_string(), |p| p.to_string_lossy().into_owned());
                let defaults = s.new_session.clone();
                // The category list has the keyboard when the dialog opens, so no field is focused.
                let claude = modal::field(&claude_t, false);
                let model = modal::field(defaults.model.as_deref().unwrap_or_default(), false);
                let shell = modal::field(&shell_t, false);
                let skills_field = modal::field(&self.store.data.settings.limits.text(), false);
                let hints = [platform::default_shell(None).0, claude_hint, "Claude Code default".into(), "none, e.g. dataviz, claude-api".into()];
                self.blur = true;
                self.set_settings_page(self.settings_page);
                self.modal = Some(Modal::Settings { shell, claude, model, effort: defaults.effort, permission: defaults.permission_mode, updates, chrome, scale: crate::panels::scale::percent(), trimmed: self.store.data.settings.trimmed_tools.clone(), claudiu_off: self.store.data.settings.claudiu_tools_off.clone(), skills_field, costly: claude::costly::scan(), hints });
                self.modal_serial += 1;
            }
            Act::PickEffort(choice) => {
                if let Some(Modal::Settings { effort, .. }) = self.modal.as_mut() {
                    *effort = choice;
                }
            }
            Act::PickPermission(choice) => {
                if let Some(Modal::Settings { permission, .. }) = self.modal.as_mut() {
                    *permission = choice;
                }
            }
            Act::NewBranch(cwd) => {
                let input = modal::field("", true);
                self.modal = Some(Modal::NewBranch { cwd, input });
            }
            Act::CreateBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::git::commit::create_branch(&cwd, &name)?))
            }
            Act::AddProject(path) => {
                let id = self.store.add_project(&path);
                let _ = id;
                self.toast("Project added", false);
            }
            Act::RemoveProject(id) => {
                self.store.remove_project(&id);
                if self.store.data.layout.project_filter.as_ref() == Some(&id) {
                    self.store.data.layout.project_filter = None;
                }
                self.toast("Project removed. Its sessions are kept.", false);
            }
            Act::SetAutoCompact(id, on) => {
                // Show the choice at once; the mod's next snapshot confirms or corrects it.
                if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.claude.as_mut()) {
                    meta.auto_compact = Some(on);
                }
                self.store.mark_dirty();
                usage::send_ctl(&usage::ctl_file(&id), "auto_compact", on.into());
                self.toast(format!("Turning auto-compact {}…", if on { "on" } else { "off" }), false);
            }
            Act::SetPrecomputeCompact(id, on) => {
                if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.claude.as_mut()) {
                    meta.precompute_compact = Some(on);
                }
                self.store.mark_dirty();
                usage::send_ctl(&usage::ctl_file(&id), "precompute_compact", on.into());
                self.toast(format!("Turning background compaction {}…", if on { "on" } else { "off" }), false);
            }
            Act::SetChrome(id, on) => {
                if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.claude.as_mut()) {
                    meta.chrome = on;
                }
                self.store.mark_dirty();
                self.restart_session(&id);
                self.toast(format!("Claude in Chrome {}. Session restarted.", if on { "on" } else { "off" }), false);
            }
            Act::SetAdvisor(id, model) => {
                // The saved setting changes at once, but a different model only reaches the current conversation on
                // /clear or /compact; switching the advisor off or on does apply right away.
                let saved = (model != "off").then(|| model.clone());
                let mut pending = false;
                let mut was_off = false;
                if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.claude.as_mut()) {
                    let active = meta.advisor_active.clone().or_else(|| meta.advisor.clone());
                    was_off = active.is_none();
                    pending = !was_off && saved.as_deref() != active.as_deref();
                    meta.advisor = saved.clone();
                    if saved.is_none() {
                        meta.advisor_active = None;
                    } else if was_off {
                        meta.advisor_active = saved.clone();
                    } else {
                        meta.advisor_active = active;
                    }
                }
                self.store.mark_dirty();
                usage::send_ctl(&usage::ctl_file(&id), "advisor", model.clone().into());
                let note = if model == "off" {
                    "Advisor off.".to_string()
                } else if pending {
                    format!("Advisor will switch to {model} after /clear or /compact.")
                } else if was_off {
                    format!("Advisor on ({model}).")
                } else {
                    format!("Advisor set to {model}.")
                };
                self.toast(note, false);
            }
            Act::AskAdvisor(id) => {
                let current = self.store.session(&id).and_then(|s| s.claude.as_ref()).and_then(|m| m.advisor.clone()).unwrap_or_default();
                let input = modal::field(&current, true);
                self.modal = Some(Modal::Advisor { id, input });
            }
            Act::FilterProject(id) => {
                self.store.data.layout.project_filter = id;
                self.store.mark_dirty();
            }
            Act::ToggleOlder => {
                let l = &mut self.store.data.layout;
                l.older_open = !l.older_open;
                self.store.mark_dirty();
            }
            Act::ConfirmQuit => {
                self.modal = None;
                self.finalize();
                self.rt.with_tx(|tx| tx.quit());
            }
            Act::Dismiss => {
                self.modal = None;
                self.focus_active();
            }
            Act::InstallUpdate => {
                let ready = matches!(self.update.status, crate::updater::Status::Ready { .. });
                let running = self.running_count();
                if ready && running > 0 {
                    self.modal = Some(Modal::Confirm {
                        title: "Restart to update?".into(),
                        body: format!(
                            "{running} session{} running. Restarting ends {}. Claude sessions can be resumed afterwards.",
                            if running == 1 { " is" } else { "s are" },
                            if running == 1 { "it" } else { "them" }
                        ),
                        confirm: "Restart and update".into(),
                        danger: true,
                        act: Act::ConfirmInstallUpdate,
                    });
                } else {
                    crate::updater::install(self);
                }
            }
            Act::ConfirmInstallUpdate => {
                self.modal = None;
                crate::updater::install(self);
            }
            Act::SwitchAccount(id) => {
                self.store.data.active_account = Some(id);
                self.store.mark_dirty();
                self.refresh_accounts();
                self.toast("Switched. New and resumed sessions use this account; running sessions keep theirs.", false);
            }
            Act::Handoff => self.ask_handoff(),
            Act::ConfirmStartSession(req_id) => self.confirm_start(&req_id),
            Act::ConfirmHandoff(id) => {
                self.modal = None;
                self.start_handoff(&id);
            }
            Act::AddAccount => {
                self.modal = Some(Modal::Confirm {
                    title: "Add an account?".into(),
                    body: "Only sign in to accounts you own. Sharing or pooling logins, or using several accounts to get around \
                           usage limits, can break Anthropic's terms.\n\n\
                           The new account uses your main Claude Code setup. Settings, CLAUDE.md, keybindings, MCP servers and project trust \\
                           are kept the same in every account, both ways: a change made here (like /model) also changes ~/.claude and \\
                           your other accounts, and the other way round. Agents, skills, plugins and session history are shared with \\
                           ~/.claude. Only the login is separate. Claudiu creates this account's folder."
                        .into(),
                    confirm: "Add and sign in".into(),
                    danger: false,
                    act: Act::ConfirmAddAccount,
                });
            }
            Act::ConfirmAddAccount => {
                self.modal = None;
                match claude::claude_home().ok_or_else(|| std::io::Error::other("Claude Code's folder was not found")).and_then(|h| accounts::create(&h)) {
                    Ok(dir) => {
                        let id = new_id();
                        self.store.data.accounts.push(Account { id: id.clone(), dir: Some(dir), ..Default::default() });
                        self.store.data.active_account = Some(id.clone());
                        self.store.mark_dirty();
                        self.sign_in(&id);
                    }
                    Err(e) => self.toast(format!("Could not create the account folder: {e}"), true),
                }
            }
            Act::SignIn(id) => self.sign_in(&id),
            Act::RemoveAccount(id) => {
                let name = self.store.account(&id).and_then(|a| a.email.clone()).unwrap_or_else(|| "this account".into());
                self.modal = Some(Modal::Confirm {
                    title: format!("Remove {name}?"),
                    body: "Deletes this account's login from Claudiu. Shared settings and session history stay in your main \
                           ~/.claude. End its running sessions first."
                        .into(),
                    confirm: "Remove".into(),
                    danger: true,
                    act: Act::ConfirmRemoveAccount(id),
                });
            }
            Act::ConfirmRemoveAccount(id) => {
                self.modal = None;
                if let Some(dir) = self.store.account(&id).and_then(|a| a.dir.clone()) {
                    match accounts::remove(&dir) {
                        Ok(()) => {
                            self.store.data.accounts.retain(|a| a.id != id);
                            if self.store.data.active_account.as_deref() == Some(id.as_str()) {
                                self.store.data.active_account = None;
                            }
                            self.store.mark_dirty();
                        }
                        Err(e) => self.toast(format!("Could not remove the account: {e}"), true),
                    }
                }
            }
        }
        self.store.save_if_dirty();
    }
}
