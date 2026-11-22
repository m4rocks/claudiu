//! Workspace: the app's state and `new`. `act` holds the `Act` table that menus, shortcuts and clicks all go through,
//! and its dispatch. The methods are split by topic: `sessions` (start, show, close), `background` (timers and polls),
//! `layout` (panels), `editor_mode` (files and editor beside the terminal), `modal` (toasts, menus, modals), `repo`
//! (git), `github_pane`, `alt_accounts`. Rendering lives in `panels/`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, PathPromptOptions,
    Pixels, Point, Subscription, Window, actions,
};

use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use crate::claude::accounts;
use crate::claude::subagents::Agent;
use crate::claude;
use crate::panels::sidebar::{SIDEBAR_MAX, SIDEBAR_MIN, SIDEBAR_W};
use crate::github::view::{GhEvent, GithubView};
use crate::claude::defaults::{Effort, Starter};
use crate::claude::mcp;
use crate::harness::{self, Harness};
use crate::git::worktree::Base;
use crate::platform::{self, Editor};
use crate::{claude::usage, datadir};
use crate::store::{
    Account, Attached, ClaudeMeta, ExternalState, Id, MAIN_ACCOUNT, Project, SessionKind, SessionRecord, Store, new_id, now,
};
use crate::terminal::SpawnSpec;
use crate::terminal::view::{TerminalEvent, TerminalView};
use crate::thread::Handover;
use crate::thread::{ThreadEvent, ThreadView};
use crate::updater::UpdateState;

mod act;
mod agent_handoff;
mod alt_accounts;
pub mod already_running;
mod attached_sync;
mod config_sync;
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
mod line_actions;
pub use line_actions::{EditLines, line_action_bindings};
mod links;
mod modal;
mod navigation;
pub mod picker;
mod pickup;
mod repo;
mod line_comment;
pub use line_comment::LineComment;
mod sessions;
mod subagents;
pub use subagents::SubagentState;
mod settings;
mod setup;
mod skills;
mod titles;
mod visuals;
mod switch;
mod worktrees;

pub use act::Act;
pub use editor_mode::{EditorPane, Tab, register_languages};
pub use ask_claude::Ask;
pub use lifecycle::saved_window_bounds;
pub use editor_mode::EditorCommand;
pub use file_ops::NameFor;
pub use settings::{Lookup, SettingsPage};
pub use skills::{SkillScope, SkillsTab};
pub use worktrees::{GitPicker, GitPickerTarget, WorktreesPage};
pub use setup::{Mode as SetupMode, SetupEvent, SetupView, window_options as setup_window_options};

actions!(
    m4code,
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
        CloseTab,
        ReopenClosed,
    ]
);

/// Ctrl+1 to Ctrl+9 (Cmd on macOS): show the running session with that number, counted down the sidebar's running sessions.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = m4code, no_json)]
pub struct ShowSession(pub usize);

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
    pub icon: Option<gpui_kit::assets::IconName>,
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
    /// Empty text = use the default.
    Settings {
        shell: Entity<InputState>,
        claude: Entity<InputState>,
        /// Default model for new sessions (General page); its harness runs new sessions.
        default_model: crate::harness::CatalogModel,
        effort: Effort,
        permission: String,
        updates: bool,
        chrome: bool,
        scale: u32,
        /// m4code tools turned off (m4code tools page).
        m4code_off: Vec<crate::claude::mcp::Feature>,
        /// Skills to turn off, separated by commas (Claude Code tools page).
        skills_field: Entity<InputState>,
        /// Search box on the Skills page (Browse skills.sh).
        skill_search: Entity<InputState>,
        /// Settings found at open time that make Claude Code cost more (Claude Code tools page).
        costly: Vec<crate::claude::costly::Finding>,
        /// Focus of the category list, which has the keyboard when the dialog opens.
        list_focus: FocusHandle,
        /// The project being renamed on the Projects page, and the name typed so far.
        renaming: Option<(Id, Entity<InputState>)>,
        /// The Harnesses page.
        subagents: Box<HarnessesForm>,
    },
}

/// Which harness's configuration popup is open over Settings → Harnesses.
pub enum HarnessDialog {
    /// With the search box over its model list.
    Claude(Entity<InputState>),
    Agent(Agent, Entity<InputState>),
}

/// The Harnesses page of the Settings dialog, as edited so far.
pub struct HarnessesForm {
    pub enabled: Vec<Harness>,
    pub agents: claude::subagents::AgentMap<AgentForm>,
    /// The ids of the Claude Code models ticked for the picker.
    pub claude_models: Vec<String>,
    pub claude_subagents: harness::SubagentAccess,
    /// The harness whose configuration popup is open over the page.
    pub picking: Option<HarnessDialog>,
}

/// One agent's subagent settings inside Settings → Harnesses.
pub struct AgentForm {
    pub on: bool,
    /// The ids of the models switched on.
    pub models: Vec<String>,
    pub edits: bool,
}

impl HarnessesForm {
    pub fn access(&self, harness: Harness) -> harness::SubagentAccess {
        match harness.agent() {
            None => self.claude_subagents,
            Some(agent) => {
                let form = self.get(agent);
                harness::SubagentAccess { on: form.on, edits: form.edits }
            }
        }
    }

    pub fn set_access(&mut self, harness: Harness, access: harness::SubagentAccess) {
        match harness.agent() {
            None => self.claude_subagents = access,
            Some(agent) => {
                let form = self.get_mut(agent);
                form.on = access.on;
                form.edits = access.edits;
            }
        }
    }

    pub fn get(&self, agent: Agent) -> &AgentForm {
        self.agents.get(agent)
    }

    pub fn get_mut(&mut self, agent: Agent) -> &mut AgentForm {
        self.agents.get_mut(agent)
    }

    /// The model ids ticked for `harness` (for an agent, also its subagents while those are on).
    pub fn picked(&self, harness: Harness) -> &[String] {
        match harness.agent() {
            None => &self.claude_models,
            Some(agent) => &self.get(agent).models,
        }
    }

    pub fn picked_mut(&mut self, harness: Harness) -> &mut Vec<String> {
        match harness.agent() {
            None => &mut self.claude_models,
            Some(agent) => &mut self.get_mut(agent).models,
        }
    }
}

/// What choosing a project in the universal Project picker dialog does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectPickerMode {
    /// Choose the project on the Welcome screen.
    Home,
    /// Filter the sidebar list (includes an "All projects" row).
    SidebarFilter,
}

/// Active universal picker dropdown owned by `Workspace`, anchored to the button that opened it.
pub struct AnchoredPicker {
    pub at: crate::panels::widgets::Dropdown,
    pub kind: WorkspacePicker,
}

/// Which universal picker dropdown is currently open on `Workspace`.
pub enum WorkspacePicker {
    /// Model + draggable effort picker on the Welcome screen.
    HomeModel { search: Entity<InputState> },
    /// Project selector for Welcome, sidebar filter, or "+ New session".
    Project { mode: ProjectPickerMode, search: Entity<InputState> },
    /// Unified Branch & Worktree picker (top bar and Welcome screen).
    Git(GitPicker),
    /// Settings -> General: Default model selector.
    SettingsModel { search: Entity<InputState> },
    /// Settings -> General: Default effort selector.
    SettingsEffort,
    /// Settings -> General: Default permission mode selector.
    SettingsPermission,
    /// Settings -> Skills: Global vs project scope selector.
    SkillsScope { search: Entity<InputState> },
    /// Right sidebar: Choose which installed editor to open in.
    Editor { target: PathBuf },
}

/// Interactive state for the Welcome / "Start a new session" page (`Main::Home`).
pub struct HomeState {
    pub project: Option<Id>,
    pub worktree: Option<PathBuf>,
    /// A worktree git is still creating; the chip shows it at once, and `worktree` becomes it once it exists.
    pub creating: Option<PathBuf>,
    /// Start was pressed while `creating`: the session starts once the worktree exists.
    pub launch_when_created: bool,
    pub model: Option<crate::harness::CatalogModel>,
    pub effort: Effort,
    pub prompt: Entity<TextareaState>,
    pub model_anchor: crate::panels::widgets::Anchor,
    pub model_highlight: usize,
    /// Follows `model_highlight` so keyboard moves keep the highlighted row in view.
    pub model_scroll: gpui::ScrollHandle,
    pub effort_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<Pixels>>>,
    pub effort_dragging: std::rc::Rc<std::cell::Cell<bool>>,
}

#[derive(Clone, Copy)]
pub enum Main {
    Home,
    Terminals,
}

/// What a running Claude session is doing, as shown by its sidebar dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    pub view: SessionView,
    pub _sub: Subscription,
    /// The session name Claude Code was last given (`--name` or `/rename`), so a changed title is sent once.
    pub claude_name: Option<String>,
    /// The account this process was started under; a session on another account than the active one can be handed off.
    pub account: Id,
}

/// What a session is drawn as: a harness session's conversation, or the terminal of a shell tab (a sign-in).
#[derive(Clone)]
pub enum SessionView {
    Thread(Entity<ThreadView>),
    Terminal(Entity<TerminalView>),
}

impl SessionView {
    pub fn is_running(&self, cx: &App) -> bool {
        match self {
            Self::Thread(v) => v.read(cx).is_running(),
            Self::Terminal(v) => v.read(cx).is_running(),
        }
    }

    pub fn is_working(&self, cx: &App) -> bool {
        match self {
            Self::Thread(v) => v.read(cx).is_working(),
            Self::Terminal(v) => v.read(cx).is_working(),
        }
    }

    pub fn is_loading(&self, cx: &App) -> bool {
        match self {
            Self::Thread(v) => v.read(cx).is_loading(),
            Self::Terminal(_) => false,
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Thread(v) => v.focus_handle(cx),
            Self::Terminal(v) => v.focus_handle(cx),
        }
    }

    pub fn start_error(&self, cx: &App) -> Option<String> {
        match self {
            Self::Thread(v) => v.read(cx).start_error(),
            Self::Terminal(v) => v.read(cx).start_error(),
        }
    }

    /// The last `n` lines (a terminal) or entries (a thread), as text.
    pub fn tail_text(&self, n: usize, cx: &App) -> String {
        match self {
            Self::Thread(v) => v.read(cx).tail_text(n),
            Self::Terminal(v) => v.read(cx).tail_text(n),
        }
    }

    #[cfg(test)]
    pub fn entity_id(&self) -> gpui::EntityId {
        match self {
            Self::Thread(v) => v.entity_id(),
            Self::Terminal(v) => v.entity_id(),
        }
    }

    /// Only a harness session moves; a shell terminal has nothing to say while it restarts.
    pub fn set_handover(&self, handover: Option<Handover>, cx: &mut App) {
        if let Self::Thread(v) = self {
            v.update(cx, |v, cx| {
                v.handover = handover;
                cx.notify();
            })
        }
    }

    pub fn thread(&self) -> Option<&Entity<ThreadView>> {
        match self {
            Self::Thread(v) => Some(v),
            Self::Terminal(_) => None,
        }
    }

    pub fn terminal(&self) -> Option<&Entity<TerminalView>> {
        match self {
            Self::Terminal(v) => Some(v),
            Self::Thread(_) => None,
        }
    }
}

/// A project's GitHub view, shown in the right sidebar. Kept while m4code runs so each project keeps its place;
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

/// The owner of the shell on the welcome screen, which has no session. Session ids are UUIDs, so it never collides.
pub const HOME_SHELL: &str = "home";

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
    /// The ACP server every harness session starts, in place of the harness's own. Tests stand in a fake agent.
    pub acp_server: Option<(PathBuf, Vec<String>)>,
    pub shell: (String, Vec<String>),
    pub live: Vec<Live>,
    pub gh: Vec<GhPane>,
    /// The GitHub view the right sidebar shows; `None` is its menu.
    pub right_gh: Option<Id>,
    pub right_drag: Option<SideDrag>,
    /// Right-sidebar rows whose × was clicked and that are shrinking away before their entry is removed.
    pub leaving: std::collections::HashSet<String>,
    pub left_reveal: crate::panels::widgets::Reveal,
    pub right_reveal: crate::panels::widgets::Reveal,
    /// The right sidebar's animated width, and the view it currently shows ("menu" or "gh:<id>"):
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
    pub home: HomeState,
    pub selected: Option<Id>,
    pub search: String,
    pub search_input: Entity<InputState>,
    pub root_focus: FocusHandle,
    /// Up/Down/Enter in the open workspace dropdown (`panels::picker::Choices`).
    pub picker_nav: crate::panels::picker::Nav,
    pub modal: Option<Modal>,
    pub modal_focus: FocusHandle,
    /// The Settings category last shown. Kept here because opening Settings builds the modal anew; not saved to disk.
    pub settings_page: SettingsPage,
    /// What the Worktrees page of Settings shows. Not saved to disk.
    pub worktrees_page: worktrees::WorktreesPage,
    /// What the Skills page of Settings shows. Not saved to disk.
    pub skills_page: skills::SkillsPage,
    /// Smooth wheel scrolling of the settings page.
    pub settings_scroll: crate::panels::glide::Smooth,
    /// Whether this run already said it couldn't write a session's tool settings file (said at most once).
    pub menu: Option<Menu>,
    /// Active universal anchored picker dropdown (model, project, branch & worktree, settings, scope, editor).
    pub picker: Option<AnchoredPicker>,
    pub toast: Option<Toast>,
    pub toast_serial: u64,
    /// Requests from sessions to other sessions, waiting for the user's answer.
    /// Sessions that leave no history once they end (the "fix with Claude" helper).
    ephemeral: HashSet<Id>,
    /// `claude auth login` tabs; account details are re-read when one ends.
    logins: HashSet<Id>,
    /// Sessions the user closed since m4code started, the last closed last; Ctrl+Z (Cmd+Z) resumes them in turn.
    closed: Vec<Id>,
    /// Whether the Custom subagents page is asking each agent for its models. Not saved to disk.
    pub model_lookups: settings::ModelLookups,
    /// When each provider's limits were last asked for; failed reads wait for its next refresh too.
    pub limits_asked: crate::claude::subagents::AgentMap<Option<std::time::Instant>>,
    pub editors: Vec<(Editor, bool)>,
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
    /// The session list's own view, made on the sidebar's first drawing (see `panels::sidebar::SessionList`).
    pub session_list: Option<Entity<crate::panels::sidebar::SessionList>>,
    /// What the session list showed when last drawn.
    pub list_model: crate::panels::sidebar::ListModel,
    /// The sidebar row under the pointer, the only one built with its × or archive button.
    pub hovered_row: Option<gpui::SharedString>,
    /// Running rows the pointer just left, whose × slot is still shrinking away (see `shrink_close_slot`).
    pub unhovered_rows: std::collections::HashSet<gpui::SharedString>,
    /// The minute (Unix time / 60) of the last tick's redraw, see `tick`.
    pub tick_minute: i64,
    /// The working icon's spin is running, see `start_spin`.
    pub spinning: bool,
    /// When a drag last reordered the sidebar, see `pointer_moved`.
    pub last_swap: std::time::Instant,
    /// Attached issues and pull requests are asked about on GitHub from time to time, see `attached_sync.rs`.
    pub attached_sync: attached_sync::Sync,
    /// The shared Claude Code settings are kept equal across accounts, see `config_sync.rs`.
    pub config_sync: config_sync::Runner,
    /// Sessions asked to open another with a first message; each waits for the user's yes (`cross.rs`).
    pub pending_starts: Vec<cross::PendingStart>,
    pub pending_subagent_waits: Vec<subagents::PendingWait>,
    /// Parents whose finished subagents are unfolded in the right sidebar.
    pub finished_subagents_open: std::collections::HashSet<Id>,
    pub pending_handoffs: HashMap<Id, agent_handoff::Pending>,
    /// Worktree switches the user approved that a session asked for; each waits for the session's turn to end (`cross.rs`).
    pub pending_switches: Vec<cross::PendingSwitch>,
    pub drag_pos: Option<gpui::Point<gpui::Pixels>>,
    pub update: UpdateState,
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
    /// The setup window while it is open, so a second request shows it instead of opening another.
    setup_window: Option<gpui::AnyWindowHandle>,
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
        let home_prompt = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 8).submit_on_enter(true).placeholder("Ask anything, or describe a task…"));
        cx.subscribe_in(&home_prompt, window, |ws: &mut Self, _, ev: &InputEvent, window, cx| {
            match ev {
                InputEvent::PressEnter { .. } => ws.act(Act::HomeLaunch, window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }
        })
        .detach();
        let home_effort = store.data.settings.new_session.effort;
        let mut ws = Self {
            claude_exe,
            acp_server: None,
            shell: platform::default_shell(store.data.settings.shell.as_deref()),
            live: vec![],
            gh: vec![],
            right_gh: None,
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
            home: HomeState {
                project: None,
                worktree: None,
                model: None,
                effort: home_effort.unwrap_or_default(),
                prompt: home_prompt,
                model_anchor: crate::panels::widgets::Anchor::default(),
                model_highlight: 0,
                model_scroll: gpui::ScrollHandle::new(),
                creating: None,
                launch_when_created: false,
                effort_bounds: std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::default())),
                effort_dragging: std::rc::Rc::new(std::cell::Cell::new(false)),
            },
            selected: store.data.layout.selected_session.clone(),
            search: String::new(),
            search_input,
            root_focus: cx.focus_handle(),
            picker_nav: Default::default(),
            modal: None,
            modal_focus: cx.focus_handle(),
            settings_page: SettingsPage::default(),
            worktrees_page: Default::default(),
            skills_page: Default::default(),
            settings_scroll: Default::default(),
            menu: None,
            picker: None,
            toast: None,
            toast_serial: 0,
            ephemeral: HashSet::new(),
            logins: HashSet::new(),
            closed: vec![],
            model_lookups: Default::default(),
            limits_asked: Default::default(),
            editors: Editor::ALL
                .iter()
                .map(|e| (*e, e.locate().is_some()))
                .collect(),
            window_title: String::new(),
            dragging: None,
            drag_pos: None,
            prev_current: Vec::new(),
            shortcuts_shown: false,
            shortcuts_wait: 0,
            slides: HashMap::new(),
            slide_gen: 0,
            session_list: None,
            list_model: Default::default(),
            hovered_row: None,
            unhovered_rows: Default::default(),
            tick_minute: 0,
            spinning: false,
            last_swap: std::time::Instant::now(),
            attached_sync: Default::default(),
            config_sync: Default::default(),
            pending_starts: vec![],
            pending_subagent_waits: vec![],
            finished_subagents_open: Default::default(),
            pending_handoffs: HashMap::new(),
            pending_switches: vec![],
            update: UpdateState::default(),
            quitting: false,
            remembered: vec![],
            sidebar_scroll: Default::default(),
            menu_scroll: Default::default(),
            restored: false,
            activity: HashMap::new(),
            git_busy: None,
            pending: None,
            setup_window: None,
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
        // Alt folders the list doesn't know (e.g. lost to an older m4code rewriting its state) are adopted again.
        for dir in accounts::folders() {
            if !ws.store.data.accounts.iter().any(|a| a.dir.as_ref() == Some(&dir)) {
                ws.store.data.accounts.push(Account { id: new_id(), dir: Some(dir), ..Default::default() });
                ws.store.mark_dirty();
            }
        }
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
        ws.sync_skills(None, cx);
        if !cfg!(test) {
            ws.load_enabled_models(cx);
            ws.take_back_global_trims();
        }
        cx.observe_window_activation(window, |_, _, cx| cx.notify()).detach();
        ws.check_updates(cx);
        window.focus(&ws.root_focus, cx);
        ws
    }

    /// Project currently chosen on the Welcome screen (defaults to the sidebar filter or the first project).
    pub fn home_selected_project(&self) -> Option<&Project> {
        self.home
            .project
            .as_ref()
            .and_then(|id| self.store.project(id))
            .or_else(|| self.project_filter())
            .or_else(|| self.store.data.projects.first())
    }

    /// Working folder the Welcome screen will launch into: chosen worktree if still under the chosen project, else the
    /// project's root, else the fallback cwd.
    pub fn home_target_cwd(&self) -> PathBuf {
        if let Some(project) = self.home_selected_project() {
            if let Some(wt) = &self.home.worktree
                && wt.is_dir()
                && project.git.as_ref().is_some_and(|g| g.folder_of(wt).is_some())
            {
                return wt.clone();
            }
            return project.path.clone();
        }
        self.active_cwd()
    }

    /// Model currently chosen on the Welcome screen (from the aggregated catalog across enabled harnesses).
    pub fn home_selected_model(&self) -> crate::harness::CatalogModel {
        let all = self.store.data.settings.aggregated_models();
        if let Some(m) = &self.home.model
            && all.iter().any(|x| x.harness == m.harness && x.id == m.id)
        {
            return m.clone();
        }
        self.store.data.settings.default_model()
    }

    pub fn picker_kind(&self) -> Option<&WorkspacePicker> {
        self.picker.as_ref().map(|p| &p.kind)
    }

    pub fn git_picker(&self) -> Option<&GitPicker> {
        match self.picker_kind() {
            Some(WorkspacePicker::Git(g)) => Some(g),
            _ => None,
        }
    }

    /// Create a focused search input for a universal picker dropdown that triggers live re-filtering as you type.
    pub(crate) fn picker_search(&self, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        cx.subscribe_in(&input, window, |_: &mut Self, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        input.update(cx, |s, cx| s.focus(window, cx));
        input
    }

    /// Switch to the New Session (`Main::Home`) composer page and focus its prompt.
    pub fn open_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.picker = None;
        self.main = Main::Home;
        self.active = None;
        self.selected = None;
        self.editor_mode = false;
        self.home.prompt.update(cx, |p, cx| p.focus(window, cx));
        cx.notify();
    }

    pub fn open_home_model_picker(&mut self, at: crate::panels::widgets::Dropdown, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.load_enabled_models(cx);
        let chosen = self.home_selected_model();
        let all = self.store.data.settings.aggregated_models();
        self.home.model_highlight = all.iter().position(|m| m.harness == chosen.harness && m.id == chosen.id).unwrap_or(0);
        self.home.model_scroll.scroll_to_item(self.home.model_highlight);
        let search = self.picker_search("Filter models…", window, cx);
        self.picker = Some(AnchoredPicker { at, kind: WorkspacePicker::HomeModel { search } });
        cx.notify();
    }

    pub fn open_project_picker(&mut self, mode: ProjectPickerMode, at: crate::panels::widgets::Dropdown, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let search = self.picker_search("Filter projects…", window, cx);
        self.picker = Some(AnchoredPicker { at, kind: WorkspacePicker::Project { mode, search } });
        cx.notify();
    }

    pub fn open_settings_model_picker(&mut self, at: crate::panels::widgets::Dropdown, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.load_models(cx);
        let search = self.picker_search("Filter models…", window, cx);
        self.picker = Some(AnchoredPicker { at, kind: WorkspacePicker::SettingsModel { search } });
        cx.notify();
    }

    pub fn open_settings_effort_picker(&mut self, at: crate::panels::widgets::Dropdown, cx: &mut Context<Self>) {
        self.menu = None;
        self.picker = Some(AnchoredPicker { at, kind: WorkspacePicker::SettingsEffort });
        cx.notify();
    }

    pub fn open_settings_permission_picker(&mut self, at: crate::panels::widgets::Dropdown, cx: &mut Context<Self>) {
        self.menu = None;
        self.picker = Some(AnchoredPicker { at, kind: WorkspacePicker::SettingsPermission });
        cx.notify();
    }

    pub fn open_skills_scope_picker(&mut self, at: crate::panels::widgets::Dropdown, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let search = self.picker_search("Filter scope…", window, cx);
        self.picker = Some(AnchoredPicker { at, kind: WorkspacePicker::SkillsScope { search } });
        cx.notify();
    }

    pub fn open_editor_picker(&mut self, target: PathBuf, at: crate::panels::widgets::Dropdown, cx: &mut Context<Self>) {
        self.menu = None;
        self.picker = Some(AnchoredPicker { at, kind: WorkspacePicker::Editor { target } });
        cx.notify();
    }

    pub fn dismiss_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.home.effort_dragging.set(false);
        if let Some(Modal::Settings { list_focus, .. }) = &self.modal {
            window.focus(list_focus, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }
}
