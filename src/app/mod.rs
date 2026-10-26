//! Workspace: the app's state, the `Act` table that menus, shortcuts and clicks all go through, and `new`. The methods
//! are split by topic: `sessions` (start, show, close), `background` (timers and polls), `layout` (panels),
//! `editor_mode` (files and editor beside the terminal), `modal` (toasts, menus, modals), `repo` (git), `github_pane`,
//! `alt_accounts`. Rendering lives in `panels/`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, PathPromptOptions,
    Pixels, Point, Subscription, Window, actions,
};

use gpui_kit::component::input::{InputEvent, InputState};
use crate::claude::accounts;
use crate::claude;
use crate::panels::sidebar::{SIDEBAR_MAX, SIDEBAR_MIN, SIDEBAR_W};
use crate::github::view::{GhEvent, GithubView};
use crate::claude::defaults::{Effort, PermissionMode, Starter};
use crate::claude::mcp;
use crate::platform::{self, Editor};
use crate::{claude::usage, datadir};
use crate::store::{
    Account, Attached, ClaudeMeta, ExternalState, Id, MAIN_ACCOUNT, SessionKind, SessionRecord, Store, new_id, now,
};
use crate::terminal::SpawnSpec;
use crate::terminal::view::{TerminalEvent, TerminalView};
use crate::updater::UpdateState;

mod alt_accounts;
mod attached_sync;
mod ask_claude;
mod background;
mod cross;
pub mod editing;
mod editor_mode;
mod github_pane;
mod handoff;
#[cfg(test)]
mod headless;
mod ingest;
mod file_ops;
mod layout;
mod language_servers;
mod lifecycle;
mod links;
mod modal;
mod navigation;
pub mod picker;
mod repo;
mod selection_reference;
mod sessions;
mod settings;
mod titles;
mod visuals;

pub use editor_mode::{EditorPane, Tab, register_languages};
pub use ask_claude::Ask;
pub use editor_mode::EditorCommand;
pub use file_ops::NameFor;
pub use settings::SettingsPage;

actions!(
    claudiu,
    [
        NewClaude,
        ToggleShell,
        CloseSession,
        NextSession,
        PrevSession,
        ToggleSidebar,
        ToggleRight,
        FocusSearch,
        ImportProject,
        SwapFocus,
        OpenGithub,
        Quit,
        HideApp,
        HideOthers,
        ShowAllApps,
        OpenSettings,
        ToggleEditor,
        QuickOpen,
        SaveFile,
        AskExplain,
        AskFix,
        AskTests,
        AskFixProblem,
        GoToLine,
        GoBack,
        GoForward,
    ]
);

/// Ctrl+1 to Ctrl+9 (Cmd on macOS): show the running session with that number, counted down the sidebar's RUNNING list.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = claudiu, no_json)]
pub struct ShowSession(pub usize);

/// Every user-triggerable operation, so buttons, menus and shortcuts share one code path.
#[derive(Clone, Debug)]
pub enum Act {
    NewClaude {
        cwd: PathBuf,
    },
    /// Show or hide the shell under a Claude session's terminal (started on first use, in the session's folder).
    ToggleShell(Id),
    /// Turn editor mode on or off for the shown session (see `editor_mode.rs`).
    ToggleEditor,
    /// Click on a session row: live -> show it, history -> resume it.
    Select(Id),
    Resume(Id),
    CloseLive(Id),
    /// End the session although its editor has unsaved files (the user confirmed).
    ConfirmCloseLive(Id),
    /// Close an editor tab and drop its unsaved edits (the user confirmed).
    DiscardTab(Id, PathBuf),
    Rename(Id),
    /// Hide (true) or restore (false) a session in the sidebar. Archived sessions still turn up in search.
    Archive(Id, bool),
    CopyText(String),
    Reveal(PathBuf),
    OpenEditor(Editor, PathBuf),
    ImportProject,
    OpenSettings,
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
    /// Editor mode's file tree: ask for a file name (new or renamed), ask before deleting, delete.
    AskFileName(Id, NameFor),
    AskDelete(Id, PathBuf),
    ConfirmDelete(Id, PathBuf),
    /// An entry of the editor's right-click menu.
    Editor(EditorCommand),
}

impl MenuItem {
    /// A plain entry: not dangerous, enabled, no hint, no separator above.
    pub fn new(label: impl Into<String>, act: Act) -> Self {
        Self { label: label.into(), hint: None, act, danger: false, disabled: false, separator_before: false }
    }
}

pub struct MenuItem {
    pub label: String,
    /// Faint text on the right that is never cut, so a long `label` is shortened with "…" instead.
    pub hint: Option<String>,
    pub act: Act,
    pub danger: bool,
    /// Shown greyed out and does nothing when clicked.
    pub disabled: bool,
    pub separator_before: bool,
}

pub struct Menu {
    pub pos: Point<Pixels>,
    pub min_w: Pixels,
    pub items: Vec<MenuItem>,
}

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
        input: Entity<InputState>,
    },
    FileName {
        id: Id,
        purpose: NameFor,
        input: Entity<InputState>,
    },
    GoToLine {
        id: Id,
        input: Entity<InputState>,
    },
    NewBranch {
        cwd: PathBuf,
        input: Entity<InputState>,
    },
    Advisor {
        id: Id,
        input: Entity<InputState>,
    },
    /// Empty text = use the default.
    Settings {
        shell: Entity<InputState>,
        claude: Entity<InputState>,
        /// Model for new sessions (Sessions page); empty = Claude Code's default.
        model: Entity<InputState>,
        effort: Option<Effort>,
        permission: Option<PermissionMode>,
        updates: bool,
        chrome: bool,
        scale: u32,
        /// Claude Code features and tools turned off (Claude Code tools page).
        trimmed: Vec<crate::claude::trim::Trim>,
        /// Claudiu tools turned off (Claudiu tools page).
        claudiu_off: Vec<crate::claude::mcp::Feature>,
        /// Focus of the category list, which has the keyboard when the dialog opens.
        list_focus: FocusHandle,
    },
}

#[derive(Clone, Copy)]
pub enum Main {
    Home,
    Terminals,
}

/// What a running Claude session is doing, as shown by its sidebar dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// Output is streaming (pulsing dot).
    Working,
    /// Finished working while the user was looking elsewhere (orange dot).
    Attention,
}

/// The git operation running in the background, if any (one at a time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitOp {
    CommitPush,
    Pull,
    Branch,
}

pub struct Live {
    pub id: Id,
    pub view: Entity<TerminalView>,
    pub _sub: Subscription,
    pub _focus: [Subscription; 2],
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
    pub view: Entity<GithubView>,
    pub _sub: Subscription,
    pub _active: Subscription,
}

/// The shell under a Claude session's terminal. It belongs to that session (never listed, never persisted), keeps
/// its height while the session lives, and goes away with it.
pub struct ShellPanel {
    pub view: Entity<TerminalView>,
    pub _sub: Subscription,
    pub open: bool,
    pub height: f32,
}

pub const SHELL_HEIGHT: f32 = 260.0;
pub const SHELL_MIN: f32 = 120.0;

/// The right sidebar: the menu or the wider GitHub view, each with its own remembered width.
pub const RIGHT_MENU_W: f32 = 272.0;
pub const RIGHT_MENU_MIN: f32 = 240.0;
pub const RIGHT_MENU_MAX: f32 = 480.0;
pub const RIGHT_GH_W: f32 = 560.0;
pub const RIGHT_GH_MIN: f32 = 380.0;

/// A drag of a sidebar's inner edge: where it started and how wide the sidebar was then.
pub struct SideDrag {
    pub x: f32,
    pub width: f32,
}

/// A drag of the divider above the shell panel: which session, and where it started.
pub struct ShellDrag {
    pub id: Id,
    pub y: f32,
    pub height: f32,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub serial: u64,
    /// Button label and what it does; the toast stays up longer so there is time to answer.
    pub action: Option<(String, Act)>,
}

pub struct Workspace {
    pub store: Store,
    pub claude_exe: Option<PathBuf>,
    pub shell: (String, Vec<String>),
    pub live: Vec<Live>,
    pub gh: Vec<GhPane>,
    /// The GitHub view the right sidebar shows; `None` is its menu.
    pub right_gh: Option<Id>,
    /// The right sidebar shows the Visual panel (when the shown session has cards and GitHub is not open).
    pub right_visual: bool,
    /// What each session's model drew in the Visual panel, oldest card first.
    pub visuals: HashMap<Id, Vec<crate::visual::Card>>,
    pub visual_scroll: crate::panels::glide::Smooth,
    pub right_drag: Option<SideDrag>,
    /// Right-sidebar rows whose × was clicked and that are shrinking away before their entry is removed.
    pub leaving: std::collections::HashSet<String>,
    pub left_reveal: crate::panels::widgets::Reveal,
    pub right_reveal: crate::panels::widgets::Reveal,
    /// The right sidebar's animated width, and the view it currently shows ("menu", "visual" or "gh:<id>"):
    /// a change of view fades the old one out, slides the width, then fades the new one in.
    pub right_w: crate::panels::widgets::Reveal,
    pub right_view: String,
    pub right_out: Option<std::time::Instant>,
    pub right_in: std::time::Instant,
    pub left_drag: Option<SideDrag>,
    /// The session shown in the main area.
    pub active: Option<Id>,
    /// Shells under Claude sessions, by the session they belong to.
    pub shells: HashMap<Id, ShellPanel>,
    pub shell_drag: Option<ShellDrag>,
    /// Editor mode is on: the sidebars give way to the shown session's files and editor. Not remembered across restarts.
    pub editor_mode: bool,
    /// Each session's files and editor, by the session they belong to.
    pub panes: HashMap<Id, EditorPane>,
    /// The shell panel's animated height (0 while hidden).
    pub shell_h: crate::panels::widgets::Reveal,
    /// Open sessions in the order they were last shown (most recent last); closing one goes back to the previous.
    recent: Vec<Id>,
    pub main: Main,
    pub selected: Option<Id>,
    pub search: String,
    pub search_input: Entity<InputState>,
    pub root_focus: FocusHandle,
    /// Set when an Ask Claude request moves the keyboard to the terminal, so that arrival doesn't also put the editor's
    /// selection into Claude's input (`selection_reference`).
    pub(crate) skip_selection_reference: bool,
    pub modal: Option<Modal>,
    pub modal_focus: FocusHandle,
    /// The Settings category last shown. Kept here because opening Settings builds the modal anew; not saved to disk.
    pub settings_page: SettingsPage,
    /// Whether this run already said it couldn't write a session's tool settings file (said at most once).
    trim_failure_shown: bool,
    pub menu: Option<Menu>,
    pub toast: Option<Toast>,
    pub toast_serial: u64,
    /// Requests from sessions to other sessions, waiting for the user's answer.
    /// Sessions that leave no history once they end (the "fix with Claude" helper).
    ephemeral: HashSet<Id>,
    /// `claude auth login` tabs; account details are re-read when one ends.
    logins: HashSet<Id>,
    /// Sessions being handed to another account: compacting, then resumed under it (see `handoff.rs`).
    handoffs: HashMap<Id, handoff::Handoff>,
    pub editors: Vec<(Editor, bool)>,
    pub transcript_mtimes: HashMap<Id, SystemTime>,
    pub window_title: String,
    /// The session being dragged in the sidebar (lifted in place while its ghost follows the cursor).
    pub dragging: Option<Id>,
    /// Pointer position at the last drag-over check, see `pointer_moved`.
    /// Sidebar reorder animation: last drawn order of the running rows, and the slides in flight.
    pub prev_current: Vec<Id>,
    /// Ctrl (Cmd on macOS) has been held a moment, so the running rows show their Ctrl+number shortcut.
    pub shortcuts_shown: bool,
    /// Bumped whenever the modifier keys change, so a pending reveal of the shortcuts is dropped.
    pub shortcuts_wait: u64,
    pub slides: HashMap<Id, crate::panels::sidebar::Slide>,
    pub slide_gen: u64,
    /// When a drag last reordered the sidebar, see `pointer_moved`.
    pub last_swap: std::time::Instant,
    /// Attached issues and pull requests are asked about on GitHub from time to time, see `attached_sync.rs`.
    pub attached_sync: attached_sync::Sync,
    /// Sessions asked to open another with a first message; each waits for the user's yes (`cross.rs`).
    pub pending_starts: Vec<cross::PendingStart>,
    pub drag_pos: Option<gpui::Point<gpui::Pixels>>,
    pub update: UpdateState,
    pub scanning: bool,
    pub quitting: bool,
    /// What `running.json` holds, and whether the saved list has been brought back yet (nothing is written before).
    pub remembered: Vec<Id>,
    /// Smooth wheel scrolling of the left list and the right menu.
    pub sidebar_scroll: crate::panels::glide::Smooth,
    pub menu_scroll: crate::panels::glide::Smooth,
    pub restored: bool,
    /// Sessions that are working or waiting for the user; absent = idle and already seen.
    pub activity: HashMap<Id, Activity>,
    pub git_busy: Option<GitOp>,
    /// What Commit & Push would do in the shown session's folder, as last read; `None` until it has been.
    pub pending: Option<(PathBuf, crate::git::commit::Pending)>,
}

impl Workspace {
    pub fn new(
        window: &mut Window,
        initial_projects: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = Store::load(Store::default_path());
        crate::panels::scale::set(store.data.settings.scale.unwrap_or(100));
        let claude_exe = claude::discover(store.data.settings.claude_path.as_deref());
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search sessions and projects").clean_on_escape());
        cx.subscribe_in(&search_input, window, |ws: &mut Self, input, ev: &InputEvent, window, cx| {
            match ev {
                InputEvent::Change => ws.search = input.read(cx).value().to_string(),
                InputEvent::PressEnter { .. } => ws.focus_active(window, cx),
                _ => {}
            }
            cx.notify();
        })
        .detach();
        let mut ws = Self {
            claude_exe,
            shell: platform::default_shell(store.data.settings.shell.as_deref()),
            live: vec![],
            gh: vec![],
            right_gh: None,
            right_visual: false,
            visuals: HashMap::new(),
            visual_scroll: Default::default(),
            right_drag: None,
            leaving: Default::default(),
            left_reveal: crate::panels::widgets::Reveal::new(store.data.layout.sidebar_visible),
            right_reveal: crate::panels::widgets::Reveal::new(!store.data.layout.right_hidden),
            right_w: crate::panels::widgets::Reveal::at_value(store.data.layout.right_menu_w.unwrap_or(RIGHT_MENU_W)),
            right_view: "menu".into(),
            right_out: None,
            right_in: std::time::Instant::now() - Duration::from_secs(5),
            left_drag: None,
            active: None,
            shells: HashMap::new(),
            shell_drag: None,
            editor_mode: false,
            panes: HashMap::new(),
            shell_h: crate::panels::widgets::Reveal::at_value(0.0),
            recent: vec![],
            main: Main::Home,
            selected: store.data.layout.selected_session.clone(),
            search: String::new(),
            search_input,
            root_focus: cx.focus_handle(),
            skip_selection_reference: false,
            modal: None,
            modal_focus: cx.focus_handle(),
            settings_page: SettingsPage::default(),
            trim_failure_shown: false,
            menu: None,
            toast: None,
            toast_serial: 0,
            ephemeral: HashSet::new(),
            logins: HashSet::new(),
            handoffs: HashMap::new(),
            editors: Editor::ALL
                .iter()
                .map(|e| (*e, e.locate().is_some()))
                .collect(),
            transcript_mtimes: HashMap::new(),
            window_title: String::new(),
            dragging: None,
            drag_pos: None,
            prev_current: Vec::new(),
            shortcuts_shown: false,
            shortcuts_wait: 0,
            slides: HashMap::new(),
            slide_gen: 0,
            last_swap: std::time::Instant::now(),
            attached_sync: Default::default(),
            pending_starts: vec![],
            update: UpdateState::default(),
            scanning: false,
            quitting: false,
            remembered: vec![],
            sidebar_scroll: Default::default(),
            menu_scroll: Default::default(),
            restored: false,
            activity: HashMap::new(),
            git_busy: None,
            pending: None,
            store,
        };
        // Everything from a previous run is history until clicked. Keep the highlight only if it still exists.
        if ws
            .selected
            .as_ref()
            .is_some_and(|id| ws.store.session(id).is_none())
        {
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
        ws.start_scan(cx);
        cx.background_executor().spawn(async { lifecycle::tidy_leftovers() }).detach();
        ws.ingest_usage(cx);
        ws.refresh_accounts(cx);
        // After the first frame: starting every saved session is the slow part of coming back.
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |ws, window, cx| ws.restore_open(window, cx));
        })
        .detach();
        ws.start_ticker(cx);
        ws.start_visual_poll(cx);
        ws.start_fetch_loop(cx);
        ws.start_git_watch(cx);
        ws.start_activity_poll(window, cx);
        ws.check_updates(cx);
        window.focus(&ws.root_focus, cx);
        ws
    }

    pub fn act(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match act {
            Act::NewClaude { cwd } => self.new_claude(cwd, window, cx),
            Act::ToggleShell(id) => self.toggle_shell(&id, window, cx),
            Act::ToggleEditor => self.toggle_editor(window, cx),
            Act::Editor(command) => self.run_editor_command(command, window, cx),
            Act::Select(id) => self.show_session(&id, window, cx),
            Act::Resume(id) => self.resume(&id, window, cx),
            Act::CloseLive(id) => self.request_close_live(&id, window, cx),
            Act::ConfirmCloseLive(id) => {
                self.modal = None;
                self.close_live(&id, window, cx)
            }
            Act::DiscardTab(id, path) => {
                self.modal = None;
                if let Some(ix) = self.panes.get(&id).and_then(|p| p.tabs.iter().position(|t| t.path == path)) {
                    self.close_tab(&id, ix, cx);
                }
            }
            Act::Rename(id) => {
                let text = self
                    .store
                    .session(&id)
                    .map(|s| s.display_title().to_string())
                    .unwrap_or_default();
                let input = self.modal_input(&text, "", window, cx);
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
                    self.toast("Archived. Search still finds it; the Claude transcript was not touched.", false, cx);
                }
            }
            Act::CopyText(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.toast("Copied", false, cx);
            }
            Act::Reveal(path) => cx.reveal_path(&path),
            Act::OpenEditor(editor, path) => {
                // The last editor used becomes the "Open in" default.
                if self.store.data.settings.editor != Some(editor) {
                    self.store.data.settings.editor = Some(editor);
                    self.store.mark_dirty();
                }
                if let Err(e) = editor.open(&path) {
                    self.toast(e, true, cx);
                }
            }
            Act::ImportProject => self.import_project(window, cx),
            Act::OpenGithub { cwd } => {
                // GitHub opens in the right sidebar, which editor mode hides.
                self.editor_mode = false;
                self.open_github(cwd, window, cx)
            }
            Act::RegenTitle(id) => self.regen_title(&id, cx),
            Act::CommitPush(id, cwd) => {
                if self.is_running(&id, cx) {
                    self.run_git(
                        GitOp::CommitPush,
                        move || crate::git::commit::commit_and_push(&|system, prompt| crate::claude::ask::complete(&id, system, prompt), &cwd).map_err(|e| crate::git::commit::classify(&cwd, e)),
                        cx,
                    )
                } else {
                    self.toast("Start the session first: the message is written through the running session.", true, cx)
                }
            }
            Act::Pull(cwd) => self.run_git(
                GitOp::Pull,
                move || crate::git::commit::pull(&cwd).map_err(|e| crate::git::commit::classify(&cwd, e)),
                cx,
            ),
            Act::SwitchBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::git::commit::switch_branch(&cwd, &name)?), cx)
            }
            Act::FixGitWithClaude(cwd) => {
                let prompt = "Claudiu's Pull or Commit & Push just failed in this repository because of a merge conflict,                     a diverged branch or a rejected push. Run git status, fix it (resolve conflicts keeping the intent of both sides,                     finish the merge or rebase, then push if the push was rejected).                     Ask me whenever a conflict is ambiguous or you are unsure which side to keep.";
                match self.running_claude_in(&cwd, cx) {
                    Some(id) => self.send_to_claude(&id, prompt, window, cx),
                    None => {
                        // No session to ask: a short-lived one on a mid-sized model.
                        let args = ["--model", "claude-sonnet-5-5", "--effort", "medium"];
                        let leading = std::iter::once(prompt).chain(args).map(String::from).collect();
                        let before = self.selected.clone();
                        self.new_claude_with(cwd, leading, Starter::FixGit, window, cx);
                        if self.selected != before
                            && let Some(id) = self.selected.clone() {
                                self.ephemeral.insert(id);
                            }
                    }
                }
            }
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
                let claude = self.modal_input(&claude_t, &claude_hint, window, cx);
                let model = self.modal_input(defaults.model.as_deref().unwrap_or_default(), "Claude Code default", window, cx);
                let shell = self.modal_input(&shell_t, &platform::default_shell(None).0, window, cx);
                let list_focus = cx.focus_handle();
                window.focus(&list_focus, cx);
                self.modal = Some(Modal::Settings { shell, claude, model, effort: defaults.effort, permission: defaults.permission_mode, updates, chrome, scale: crate::panels::scale::percent(), trimmed: self.store.data.settings.trimmed_tools.clone(), claudiu_off: self.store.data.settings.claudiu_tools_off.clone(), list_focus });
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
                let input = self.modal_input("", "", window, cx);
                self.modal = Some(Modal::NewBranch { cwd, input });
            }
            Act::CreateBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::git::commit::create_branch(&cwd, &name)?), cx)
            }
            Act::AddProject(path) => {
                let id = self.store.add_project(&path);
                let _ = id;
                self.toast("Project added", false, cx);
            }
            Act::RemoveProject(id) => {
                self.store.remove_project(&id);
                if self.store.data.layout.project_filter.as_ref() == Some(&id) {
                    self.store.data.layout.project_filter = None;
                }
                self.toast("Project removed. Its sessions are kept.", false, cx);
            }
            Act::SetAutoCompact(id, on) => {
                // Show the choice at once; the mod's next snapshot confirms or corrects it.
                if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.claude.as_mut()) {
                    meta.auto_compact = Some(on);
                }
                self.store.mark_dirty();
                cx.notify();
                usage::send_ctl(&usage::ctl_file(&id), "auto_compact", on.into());
                self.toast(format!("Turning auto-compact {}…", if on { "on" } else { "off" }), false, cx);
            }
            Act::SetPrecomputeCompact(id, on) => {
                if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.claude.as_mut()) {
                    meta.precompute_compact = Some(on);
                }
                self.store.mark_dirty();
                cx.notify();
                usage::send_ctl(&usage::ctl_file(&id), "precompute_compact", on.into());
                self.toast(format!("Turning background compaction {}…", if on { "on" } else { "off" }), false, cx);
            }
            Act::SetChrome(id, on) => {
                if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.claude.as_mut()) {
                    meta.chrome = on;
                }
                self.store.mark_dirty();
                self.restart_session(&id, window, cx);
                self.toast(format!("Claude in Chrome {}. Session restarted.", if on { "on" } else { "off" }), false, cx);
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
                cx.notify();
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
                self.toast(note, false, cx);
            }
            Act::AskAdvisor(id) => {
                let current = self.store.session(&id).and_then(|s| s.claude.as_ref()).and_then(|m| m.advisor.clone()).unwrap_or_default();
                let input = self.modal_input(&current, "opus, sonnet, off, or a model id", window, cx);
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
                self.finalize(cx);
                cx.quit();
            }
            Act::Dismiss => {
                self.modal = None;
                self.focus_active(window, cx);
            }
            Act::InstallUpdate => {
                let ready = matches!(self.update.status, crate::updater::Status::Ready { .. });
                let running = self.running_count(cx);
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
                    window.focus(&self.modal_focus, cx);
                } else {
                    crate::updater::install(self, window, cx);
                }
            }
            Act::ConfirmInstallUpdate => {
                self.modal = None;
                crate::updater::install(self, window, cx);
            }
            Act::SwitchAccount(id) => {
                self.store.data.active_account = Some(id);
                self.store.mark_dirty();
                self.refresh_accounts(cx);
                self.toast("Switched. New and resumed sessions use this account; running sessions keep theirs.", false, cx);
            }
            Act::Handoff => self.ask_handoff(window, cx),
            Act::ConfirmStartSession(req_id) => self.confirm_start(&req_id, window, cx),
            Act::ConfirmHandoff(id) => {
                self.modal = None;
                self.start_handoff(&id, cx);
            }
            Act::AddAccount => {
                self.modal = Some(Modal::Confirm {
                    title: "Add an account?".into(),
                    body: "Only sign in to accounts you own. Sharing or pooling logins, or using several accounts to get around \
                           usage limits, can break Anthropic's terms.\n\n\
                           The new account shares your main Claude Code setup: settings, CLAUDE.md, agents, skills, plugins and \
                           session history. Changes made while using it (like /model) change your main setup too. \
                           Only the login is kept separate. Claudiu creates this account's folder; your main ~/.claude is not modified."
                        .into(),
                    confirm: "Add and sign in".into(),
                    danger: false,
                    act: Act::ConfirmAddAccount,
                });
                window.focus(&self.modal_focus, cx);
            }
            Act::ConfirmAddAccount => {
                self.modal = None;
                match claude::claude_home().ok_or_else(|| std::io::Error::other("Claude Code's folder was not found")).and_then(|h| accounts::create(&h)) {
                    Ok(dir) => {
                        let id = new_id();
                        self.store.data.accounts.push(Account { id: id.clone(), dir: Some(dir), ..Default::default() });
                        self.store.data.active_account = Some(id.clone());
                        self.store.mark_dirty();
                        self.sign_in(&id, window, cx);
                    }
                    Err(e) => self.toast(format!("Could not create the account folder: {e}"), true, cx),
                }
            }
            Act::SignIn(id) => self.sign_in(&id, window, cx),
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
                window.focus(&self.modal_focus, cx);
            }
            Act::AskFileName(id, purpose) => self.ask_file_name(id, purpose, window, cx),
            Act::AskDelete(id, path) => self.ask_delete(id, path, window, cx),
            Act::ConfirmDelete(id, path) => self.delete_path(id, path, cx),
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
                        Err(e) => self.toast(format!("Could not remove the account: {e}"), true, cx),
                    }
                }
            }
        }
        self.store.save_if_dirty();
        cx.notify();
    }
}
