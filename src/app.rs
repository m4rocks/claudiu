//! Workspace: the app's state and all of its actions. Rendering lives in `sidebar.rs` / `views.rs`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, PathPromptOptions,
    Pixels, Point, Subscription, Window, actions,
};

use gpui_kit::component::input::{InputEvent, InputState};
use crate::accounts;
use crate::claude;
use crate::sidebar::{SIDEBAR_MAX, SIDEBAR_MIN, SIDEBAR_W};
use crate::github::view::{GhEvent, GithubView};
use crate::mcp;
use crate::platform::{self, Editor};
use crate::{datadir, usage};
use crate::store::{
    Account, Attached, ClaudeMeta, ExternalState, Id, MAIN_ACCOUNT, SessionKind, SessionRecord, Store, new_id, now,
};
use crate::terminal::SpawnSpec;
use crate::terminal_view::{TerminalEvent, TerminalView};
use crate::updater::UpdateState;

mod cross;
mod handoff;
#[cfg(test)]
mod headless;
mod ingest;
mod lifecycle;
mod links;
mod titles;
mod visuals;

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
    ]
);

/// Every user-triggerable operation, so buttons, menus and shortcuts share one code path.
#[derive(Clone, Debug)]
pub enum Act {
    NewClaude {
        cwd: PathBuf,
    },
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
    /// The project's GitHub issues and pull requests.
    OpenGithub {
        cwd: PathBuf,
    },
    /// Have the running session's own mod (Haiku) write a new title from what was said in it.
    RegenTitle(Id),
    /// Change a setting of a running session through its mod (see `usage::send_ctl`).
    SetAutoCompact(Id, bool),
    SetAdvisor(Id, String),
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
        updates: bool,
        scale: u32,
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
    pub visual_scroll: crate::glide::Smooth,
    pub right_drag: Option<SideDrag>,
    /// Right-sidebar rows whose × was clicked and that are shrinking away before their entry is removed.
    pub leaving: std::collections::HashSet<String>,
    pub left_reveal: crate::widgets::Reveal,
    pub right_reveal: crate::widgets::Reveal,
    /// The right sidebar's animated width, and the view it currently shows ("menu", "visual" or "gh:<id>"):
    /// a change of view fades the old one out, slides the width, then fades the new one in.
    pub right_w: crate::widgets::Reveal,
    pub right_view: String,
    pub right_out: Option<std::time::Instant>,
    pub right_in: std::time::Instant,
    pub left_drag: Option<SideDrag>,
    /// The session shown in the main area.
    pub active: Option<Id>,
    /// Shells under Claude sessions, by the session they belong to.
    pub shells: HashMap<Id, ShellPanel>,
    pub shell_drag: Option<ShellDrag>,
    /// The shell panel's animated height (0 while hidden).
    pub shell_h: crate::widgets::Reveal,
    /// Open sessions in the order they were last shown (most recent last); closing one goes back to the previous.
    recent: Vec<Id>,
    pub main: Main,
    pub selected: Option<Id>,
    pub search: String,
    pub search_input: Entity<InputState>,
    pub root_focus: FocusHandle,
    pub modal: Option<Modal>,
    pub modal_focus: FocusHandle,
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
    pub slides: HashMap<Id, crate::sidebar::Slide>,
    pub slide_gen: u64,
    /// When a drag last reordered the sidebar, see `pointer_moved`.
    pub last_swap: std::time::Instant,
    pub drag_pos: Option<gpui::Point<gpui::Pixels>>,
    pub update: UpdateState,
    pub scanning: bool,
    pub quitting: bool,
    /// What `running.json` holds, and whether the saved list has been brought back yet (nothing is written before).
    pub remembered: Vec<Id>,
    /// Smooth wheel scrolling of the left list and the right menu.
    pub sidebar_scroll: crate::glide::Smooth,
    pub menu_scroll: crate::glide::Smooth,
    pub restored: bool,
    /// Sessions that are working or waiting for the user; absent = idle and already seen.
    pub activity: HashMap<Id, Activity>,
    pub git_busy: Option<GitOp>,
}

impl Workspace {
    pub fn new(
        window: &mut Window,
        initial_projects: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = Store::load(Store::default_path());
        crate::scale::set(store.data.settings.scale.unwrap_or(100));
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
            left_reveal: crate::widgets::Reveal::new(store.data.layout.sidebar_visible),
            right_reveal: crate::widgets::Reveal::new(!store.data.layout.right_hidden),
            right_w: crate::widgets::Reveal::at_value(store.data.layout.right_menu_w.unwrap_or(RIGHT_MENU_W)),
            right_view: "menu".into(),
            right_out: None,
            right_in: std::time::Instant::now() - Duration::from_secs(5),
            left_drag: None,
            active: None,
            shells: HashMap::new(),
            shell_drag: None,
            shell_h: crate::widgets::Reveal::at_value(0.0),
            recent: vec![],
            main: Main::Home,
            selected: store.data.layout.selected_session.clone(),
            search: String::new(),
            search_input,
            root_focus: cx.focus_handle(),
            modal: None,
            modal_focus: cx.focus_handle(),
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
            slides: HashMap::new(),
            slide_gen: 0,
            last_swap: std::time::Instant::now(),
            update: UpdateState::default(),
            scanning: false,
            quitting: false,
            remembered: vec![],
            sidebar_scroll: Default::default(),
            menu_scroll: Default::default(),
            restored: false,
            activity: HashMap::new(),
            git_busy: None,
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
        datadir::cleanup();
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

    // ------------------------------------------------------------------ background work

    /// Startup reconciliation: scan Claude Code's records off the UI thread, then merge.
    pub fn start_scan(&mut self, cx: &mut Context<Self>) {
        let Some(home) = claude::claude_home() else {
            return;
        };
        self.scanning = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { claude::scan_all(&home) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                ws.scanning = false;
                ws.store.reconcile(&result.sessions, result.ok);
                if result.ok {
                    // Records from earlier runs that never produced a transcript: nothing to resume.
                    let live: Vec<Id> = ws.live.iter().map(|l| l.id.clone()).collect();
                    ws.store.data.sessions.retain(|s| {
                        live.contains(&s.id)
                            || !s.claude.as_ref().is_some_and(|c| {
                                c.external == ExternalState::Unknown && c.transcript.is_none()
                            })
                    });
                }
                ws.store.save_if_dirty();
                cx.notify();
            });
        })
        .detach();
    }

    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                if this.update(cx, |ws, cx| ws.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Once a second: derive each Claude session's `Activity` from its terminal output. When one stops working
    /// while the user isn't looking at it, mark it for attention, and if the whole window is in the
    /// background as well, flash the taskbar icon / bounce the dock icon.
    fn start_activity_poll(&mut self, window: &Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let (statuses, requests) = cx
                    .background_executor()
                    .spawn(async { (claude::claude_home().map(|h| claude::session_statuses(&h)).unwrap_or_default(), crate::bridge::take_requests()) })
                    .await;
                let update = this.update_in(cx, |ws, window, cx| {
                    ws.serve_requests(requests, window, cx);
                    ws.finish_handoffs(window, cx);
                    ws.update_activity(window, cx, &statuses)
                });
                if update.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// `statuses` is Claude Code's own busy/idle/waiting per session id; sessions it doesn't list (older Claude
    /// Code, no registry file) fall back to the terminal-output heuristic.
    fn update_activity(&mut self, window: &mut Window, cx: &mut Context<Self>, statuses: &HashMap<String, String>) {
        let window_active = window.is_window_active();
        let shown = self.active_id().cloned();
        let mut next: HashMap<Id, Activity> = HashMap::new();
        let mut flash = false;
        for l in &self.live {
            let is_claude = self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude);
            let view = l.view.read(cx);
            if !is_claude || !view.is_running() {
                continue;
            }
            let viewing = window_active && shown.as_deref() == Some(l.id.as_str());
            let status = self.store.session(&l.id).and_then(|r| statuses.get(r.claude_id()?));
            let working = status.map_or_else(|| view.is_working(), |s| s == "busy");
            match (working, self.activity.get(&l.id)) {
                (true, _) => {
                    next.insert(l.id.clone(), Activity::Working);
                }
                (false, Some(Activity::Working)) if !viewing => {
                    flash |= !window_active;
                    next.insert(l.id.clone(), Activity::Attention);
                }
                (false, Some(Activity::Attention)) if !viewing => {
                    next.insert(l.id.clone(), Activity::Attention);
                }
                _ => {}
            }
        }
        if next != self.activity {
            self.activity = next;
            cx.notify();
        }
        if flash {
            platform::request_attention(window);
        }
        self.sync_claude_names();
    }

    /// Claude Code keeps its own session name (`/resume`, Remote Control, the mobile apps). `--name` covers
    /// launches; for a running session the name goes to its name file and the mod runs `/rename` once Claude
    /// Code is idle. Nothing is typed into the PTY.
    fn sync_claude_names(&mut self) {
        for l in &mut self.live {
            let Some((cid, w)) = self
                .store
                .session(&l.id)
                .and_then(|r| Some((r.claude_id()?, r.chosen_title()?)))
                .filter(|(_, w)| l.claude_name.as_deref() != Some(*w))
            else {
                continue;
            };
            l.claude_name = Some(w.to_string());
            mcp::write_name(cid, Some(w));
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        self.purge_dead_shells();
        self.store.save_if_dirty();
        self.refresh_live(cx);
        self.remember_open(cx);
        self.ingest_usage(cx);
        cx.notify();
    }

    /// Per-process additions to a Claude launch; nothing in the user's Claude Code config changes:
    /// the mod (`--plugin-dir`, which reports usage and status), the tab-title MCP server (`--mcp-config`, its
    /// tools pre-allowed), and IDE integration switched off through environment variables.
    fn with_helpers(
        &self,
        mut spec: SpawnSpec,
        claude_session_id: &str,
        record_id: &str,
    ) -> SpawnSpec {
        let started = std::time::Instant::now();
        // Diagnostics for the start-up trace (`trace.rs`): start without Claudiu's MCP server or mod to see what they cost.
        let without = |name: &str| crate::trace::on() && std::env::var_os(name).is_some();
        if let Ok(config) = mcp::prepare(claude_session_id, record_id).and_then(|c| if without("CLAUDIU_NO_MCP") { Err(std::io::Error::other("off")) } else { Ok(c) }) {
            spec.args.push("--allowedTools".into());
            spec.args.extend(mcp::allowed_tools());
            spec.args.extend(["--mcp-config".into(), config.to_string_lossy().into_owned()]);
        }
        if let Ok(dir) = mcp::prepare_mod().and_then(|d| if without("CLAUDIU_NO_MOD") { Err(std::io::Error::other("off")) } else { Ok(d) }) {
            spec.args.extend(["--plugin-dir".into(), dir.to_string_lossy().into_owned()]);
            spec.env.push(("CLAUDIU_NAME_FILE".into(), mcp::name_file(claude_session_id).to_string_lossy().into_owned()));
            spec.env.push((usage::USAGE_DIR_ENV.into(), usage::snapshot_dir().to_string_lossy().into_owned()));
            spec.env.push((usage::CTL_ENV.into(), usage::ctl_file(record_id).to_string_lossy().into_owned()));
            spec.env.push((crate::visual::VISUAL_ENV.into(), crate::visual::file(record_id).to_string_lossy().into_owned()));
            spec.env.push((crate::ask::ASK_ENV.into(), crate::ask::request_file(record_id).to_string_lossy().into_owned()));
        }
        spec.env.push(("CLAUDE_CODE_AUTO_CONNECT_IDE".into(), "false".into()));
        spec.env.push(("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL".into(), "1".into()));
        if let Some(acct) = self.store.active_account() {
            spec.env.push((usage::ACCOUNT_ENV.into(), acct.id.clone()));
            spec.env.extend(Self::account_env(acct));
        }
        // Extra `claude` flags for the start-up trace, e.g. CLAUDIU_TRACE_ARGS="--safe-mode".
        if crate::trace::on() && let Some(extra) = std::env::var_os("CLAUDIU_TRACE_ARGS") {
            spec.args.extend(extra.to_string_lossy().split_whitespace().map(String::from));
        }
        (spec.program, spec.args) = platform::through_login_shell(spec.program, spec.args);
        if let Some(cwd) = spec.cwd.as_ref().and_then(|c| c.file_name()) {
            crate::trace::note(&cwd.to_string_lossy(), started.elapsed(), "launch files and arguments prepared (before the terminal exists)");
        }
        spec
    }

    /// Environment for an alt account (empty for the main one), after linking anything new from the main folder into it.
    fn account_env(acct: &Account) -> Vec<(String, String)> {
        let Some(dir) = acct.dir.as_ref() else { return vec![] };
        if let Some(main) = claude::claude_home() {
            let _ = std::fs::create_dir_all(dir);
            let _ = accounts::link_shared(&main, dir);
        }
        accounts::env(dir)
    }

    /// Read each account's email and plan through `claude auth status`, off the UI thread.
    pub fn refresh_accounts(&mut self, cx: &mut Context<Self>) {
        let Some(exe) = self.claude_exe.clone() else { return };
        let jobs: Vec<(Id, Option<PathBuf>)> = self.store.data.accounts.iter().map(|a| (a.id.clone(), a.dir.clone())).collect();
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move { jobs.into_iter().map(|(id, dir)| (id, accounts::status(&exe, dir.as_deref()))).collect::<Vec<_>>() })
                .await;
            let _ = this.update(cx, |ws, cx| {
                let mut signed_in = vec![];
                for (id, status) in out {
                    // No answer (CLI failed to run) keeps what we knew; an answer of "logged out" clears it.
                    let Some(s) = status else { continue };
                    let Some(a) = ws.store.data.accounts.iter_mut().find(|a| a.id == id) else { continue };
                    let (email, plan) = if s.logged_in { (s.email, s.plan) } else { (None, None) };
                    if a.email != email || a.plan != plan {
                        if let Some(e) = email.clone().filter(|_| !a.is_main() && a.email != email) {
                            signed_in.push((a.id.clone(), e));
                        }
                        (a.email, a.plan) = (email, plan);
                        ws.store.mark_dirty();
                    }
                }
                // Warn once, when an alt has just signed in to the main account's login.
                let main = ws.store.account(MAIN_ACCOUNT).and_then(|a| a.email.clone());
                if let Some((id, _)) = signed_in.into_iter().find(|(_, e)| Some(e) == main.as_ref()) {
                    // The login page approves whichever claude.ai account the browser is already signed in to.
                    let text = "This alt signed in to your main account. Sign out of claude.ai in your browser (or use a private window), then sign in again.";
                    ws.show_toast(text.into(), true, Some(("Sign in again".into(), Act::SignIn(id))), cx);
                }
                ws.store.save_if_dirty();
                cx.notify();
            });
        })
        .detach();
    }

    /// `claude auth login` for an alt account in a terminal tab; account details refresh when it ends.
    fn sign_in(&mut self, account: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(exe) = self.claude_exe.clone() else {
            self.toast("Could not find the `claude` CLI.", true, cx);
            return;
        };
        let Some(env) = self.store.account(account).map(Self::account_env).filter(|e| !e.is_empty()) else { return };
        let cwd = platform::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let (program, args) = platform::through_login_shell(exe.to_string_lossy().into_owned(), vec!["auth".into(), "login".into()]);
        let spec = SpawnSpec { program, args, cwd: Some(cwd.clone()), env };
        let record = SessionRecord {
            id: new_id(),
            kind: SessionKind::Shell,
            title: "Sign in to account".into(),
            custom_title: None,
            auto_title: None,
            project_id: None,
            cwd,
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            claude: None,
        };
        self.logins.insert(record.id.clone());
        self.start(record, spec, true, window, cx);
    }

    /// Terminal sessions live only in CURRENT SESSION: once a shell's process is gone and its pane has been
    /// closed or replaced, its record is dropped instead of lingering in project history.
    fn purge_dead_shells(&mut self) {
        let keep: Vec<Id> = self.live.iter().map(|l| l.id.clone()).collect();
        self.store
            .data
            .sessions
            .retain(|s| s.kind != SessionKind::Shell || keep.contains(&s.id));
        if self
            .selected
            .as_ref()
            .is_some_and(|id| self.store.session(id).is_none())
        {
            self.selected = None;
        }
    }

    /// Re-read transcripts of running Claude sessions so the context meter follows reality.
    fn refresh_live(&mut self, cx: &mut Context<Self>) {
        let Some(home) = claude::claude_home() else {
            return;
        };
        let jobs: Vec<(Id, String, Option<PathBuf>, Option<SystemTime>)> = self
            .live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
            .filter_map(|l| {
                let rec = self.store.session(&l.id)?;
                let meta = rec.claude.as_ref()?;
                Some((
                    l.id.clone(),
                    meta.session_id.clone(),
                    meta.transcript.clone(),
                    self.transcript_mtimes.get(&l.id).copied(),
                ))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::new();
                    for (id, claude_id, known, last_mtime) in jobs {
                        let Some(path) = known
                            .filter(|p| p.is_file())
                            .or_else(|| claude::find_transcript(&home, &claude_id))
                        else {
                            continue;
                        };
                        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                        if mtime.is_some() && mtime == last_mtime {
                            continue;
                        }
                        if let Some(scan) = claude::read_session(&path) {
                            out.push((id, mtime, scan));
                        }
                    }
                    out
                })
                .await;
            let _ = this.update(cx, |ws, cx| {
                for (id, mtime, scan) in out {
                    if let Some(m) = mtime {
                        ws.transcript_mtimes.insert(id.clone(), m);
                    }
                    ws.store
                        .reconcile(std::slice::from_ref(&scan.session), false);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn check_updates(&mut self, cx: &mut Context<Self>) {
        crate::updater::check(self, cx);
    }

    // ------------------------------------------------------------------ queries

    pub fn is_running(&self, id: &str, cx: &App) -> bool {
        self.live
            .iter()
            .any(|l| l.id == id && l.view.read(cx).is_running())
    }

    pub fn running_count(&self, cx: &App) -> usize {
        self.live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
            .count()
    }

    pub fn live_view(&self, id: &str) -> Option<&Entity<TerminalView>> {
        self.live.iter().find(|l| l.id == id).map(|l| &l.view)
    }

    pub fn active_id(&self) -> Option<&Id> {
        match self.main {
            Main::Terminals => self.active.as_ref(),
            Main::Home => None,
        }
    }

    pub fn gh_pane(&self, id: &str) -> Option<&GhPane> {
        self.gh.iter().find(|p| p.id == id)
    }

    /// Where a new Claude session starts: the project the sidebar is filtered to, else where the shown session is.
    pub fn new_session_cwd(&self) -> PathBuf {
        self.project_filter().map_or_else(|| self.active_cwd(), |p| p.path.clone())
    }

    pub fn active_cwd(&self) -> PathBuf {
        self.active_id()
            .and_then(|id| self.store.session(id).map(|s| s.cwd.clone()))
            .or_else(|| self.store.data.projects.first().map(|p| p.path.clone()))
            .or_else(platform::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    // ------------------------------------------------------------------ session lifecycle

    pub fn toast(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.show_toast(text.into(), error, None, cx);
    }

    fn show_toast(&mut self, text: String, error: bool, action: Option<(String, Act)>, cx: &mut Context<Self>) {
        self.toast_serial += 1;
        let serial = self.toast_serial;
        let secs = if action.is_some() { 20 } else { 5 };
        self.toast = Some(Toast {
            text,
            error,
            serial,
            action,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(secs)).await;
            let _ = this.update(cx, |ws, cx| {
                if ws.toast.as_ref().is_some_and(|t| t.serial == serial) {
                    ws.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn new_claude(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.new_claude_with(cwd, vec![], window, cx);
    }

    /// `leading` goes before every other argument: the initial prompt must come ahead of the variadic
    /// `--allowedTools` / `--mcp-config` flags, or they would swallow it.
    fn new_claude_with(&mut self, cwd: PathBuf, leading: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(exe) = self.claude_exe.clone() else {
            self.toast("Could not find the `claude` CLI. Install Claude Code, or set its path in Settings.", true, cx);
            return;
        };
        let session_id = new_id();
        let record_id = new_id();
        let mut spec = claude::new_session_spec(&exe, &cwd, &session_id);
        spec.args.splice(0..0, leading);
        let spec = self.with_helpers(spec, &session_id, &record_id);
        let record = SessionRecord {
            id: record_id,
            kind: SessionKind::Claude,
            title: "New Claude session".into(),
            custom_title: None,
            auto_title: None,
            project_id: self.store.project_for_cwd(&cwd),
            cwd,
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            claude: Some(ClaudeMeta {
                session_id,
                ..Default::default()
            }),
        };
        self.start(record, spec, true, window, cx);
    }

    /// Show or hide the shell under a Claude session. It starts on first use, in the session's folder, and lives and
    /// dies with the session.
    pub fn toggle_shell(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let rec = self.store.session(id).filter(|r| r.kind == SessionKind::Claude).cloned();
        let Some(rec) = rec.filter(|_| self.is_running(id, cx)) else {
            self.toast("Open a Claude session to use its shell.", false, cx);
            return;
        };
        // From another session the shell is brought up with its session; only on the shown one does it toggle.
        let shown = self.active.as_deref() == Some(id) && matches!(self.main, Main::Terminals);
        if !shown {
            self.show_session(id, window, cx);
        }
        if let Some(p) = self.shells.get_mut(id) {
            p.open = !shown || !p.open;
            if !p.open {
                self.focus_active(window, cx);
                cx.notify();
                return;
            }
        } else {
            let (program, args) = self.shell.clone();
            // A session's folder may be gone (temp dirs, deleted worktrees); CreateProcess then fails with "filename syntax is incorrect".
            let cwd = if rec.cwd.is_dir() { rec.cwd.clone() } else { platform::home_dir().unwrap_or(rec.cwd.clone()) };
            let spec = SpawnSpec { program, args, cwd: Some(cwd), ..Default::default() };
            let (terminal, rx) = TerminalView::spawn(&spec);
            let view = cx.new(|cx| TerminalView::new(terminal, rx, cx));
            let owner = id.to_string();
            let sub = cx.subscribe_in(&view, window, move |ws: &mut Workspace, view, ev: &TerminalEvent, window, cx| {
                if matches!(ev, TerminalEvent::Exited(_)) {
                    if let Some(err) = view.read(cx).start_error() {
                        ws.toast(format!("Could not start the shell: {err}"), true, cx);
                    }
                    ws.shells.remove(&owner);
                    ws.focus_active(window, cx);
                    cx.notify();
                }
            });
            self.shells.insert(id.to_string(), ShellPanel { view, _sub: sub, open: true, height: SHELL_HEIGHT });
        }
        self.focus_shell(window, cx);
        cx.notify();
    }

    fn focus_shell(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.active_id().and_then(|id| self.shells.get(id)).filter(|p| p.open) {
            window.focus(&p.view.read(cx).focus_handle(cx), cx);
        }
    }

    /// Move the keyboard between the Claude terminal and its open shell.
    pub fn swap_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shell) = self.active_id().and_then(|id| self.shells.get(id)).filter(|p| p.open) else { return };
        if shell.view.read(cx).focus_handle(cx).is_focused(window) {
            self.focus_active(window, cx);
        } else {
            self.focus_shell(window, cx);
        }
    }

    /// The divider above the shell panel is being dragged: follow the pointer, keeping room for the terminal.
    pub fn drag_shell(&mut self, y: f32, window_h: f32) {
        let Some(d) = self.shell_drag.as_ref() else { return };
        let height = (d.height + d.y - y).clamp(SHELL_MIN, (window_h - 200.0).max(SHELL_MIN));
        if let Some(p) = self.shells.get_mut(&d.id) {
            p.height = height;
        }
    }

    /// Resume a Claude session through `claude --resume`. Reuses the history record.
    pub fn resume(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running(id, cx) {
            self.show_session(id, window, cx);
            return;
        }
        let Some(rec) = self.store.session(id).cloned() else {
            return;
        };
        let Some(meta) = rec.claude.clone() else {
            return;
        };
        let Some(exe) = self.claude_exe.clone() else {
            self.toast("Could not find the `claude` CLI.", true, cx);
            return;
        };
        let mut spec = claude::resume_spec(&exe, &rec.cwd, &meta.session_id);
        // Hand Claudiu's tab title to Claude Code so its own name (prompt box, /resume, Remote Control) matches.
        if let Some(name) = rec.chosen_title() {
            spec.args.extend(["--name".into(), name.into()]);
        }
        let spec = self.with_helpers(spec, &meta.session_id, &rec.id);
        self.start(rec, spec, false, window, cx);
    }

    fn start(
        &mut self,
        mut record: SessionRecord,
        spec: SpawnSpec,
        is_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (terminal, rx) = TerminalView::spawn(&spec);
        let id = record.id.clone();
        let loading = record.kind == SessionKind::Claude;
        record.exit_code = None;
        record.last_active = now();
        // Re-resuming: drop the previous (exited) terminal view for this record.
        self.live.retain(|l| l.id != id);
        self.purge_dead_shells();
        self.transcript_mtimes.remove(&id);
        if is_new {
            self.store.add_session(record);
        } else if let Some(r) = self.store.session_mut(&id) {
            r.exit_code = None;
            r.last_active = now();
        }
        let view = cx.new(|cx| {
            let mut v = TerminalView::new(terminal, rx, cx);
            v.loading = loading;
            v
        });
        let sub_id = id.clone();
        let sub = cx.subscribe_in(
            &view,
            window,
            move |ws: &mut Workspace, _view, ev: &TerminalEvent, window, cx| match ev {
                TerminalEvent::Exited(code) => ws.on_exit(&sub_id, *code, window, cx),
                TerminalEvent::Bell => {}
            },
        );
        let claude_name = spec.args.iter().position(|a| a == "--name").and_then(|i| spec.args.get(i + 1)).cloned();
        // The mod reads the launch name first and leaves it alone; a stale file would otherwise rename the session.
        if let Some(cid) = self.store.session(&id).and_then(|r| r.claude_id()) {
            mcp::write_name(cid, claude_name.as_deref());
        }
        let account = self.store.active_account().map_or_else(|| MAIN_ACCOUNT.into(), |a| a.id.clone());
        self.live.push(Live {
            id: id.clone(),
            view,
            _sub: sub,
            claude_name,
            title_stamp: None,
            account,
        });
        self.activate(&id, cx);
        self.remember(&id);
        self.selected = Some(id.clone());
        self.main = Main::Terminals;
        self.persist_layout();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Make `id` the session shown.
    fn activate(&mut self, id: &Id, cx: &App) {
        self.active = Some(id.clone());
        self.prune_live(cx);
    }

    /// Exited terminals are only kept while shown; a shell panel goes with its session.
    fn prune_live(&mut self, cx: &App) {
        let active = &self.active;
        self.live.retain(|l| active.as_ref() == Some(&l.id) || l.view.read(cx).is_running());
        let live = &self.live;
        self.shells.retain(|id, _| live.iter().any(|l| &l.id == id));
    }

    /// A Claude session that never produced a transcript (opened and closed without a prompt) has nothing
    /// to resume. Remove Claudiu's own record of it; Claude Code's data is never touched.
    fn drop_if_empty(&mut self, id: &str) {
        if self.ephemeral.remove(id) {
            self.store.data.sessions.retain(|s| s.id != id);
            if self.selected.as_deref() == Some(id) {
                self.selected = None;
            }
            self.store.mark_dirty();
            return;
        }
        let Some(rec) = self.store.session(id) else {
            return;
        };
        let Some(meta) = rec.claude.as_ref() else {
            return;
        };
        if meta.external != ExternalState::Unknown || meta.transcript.is_some() {
            return;
        }
        let has_transcript = claude::claude_home()
            .and_then(|h| claude::find_transcript(&h, &meta.session_id))
            .is_some();
        if !has_transcript {
            self.store.data.sessions.retain(|s| s.id != id);
            if self.selected.as_deref() == Some(id) {
                self.selected = None;
            }
            self.store.mark_dirty();
        }
    }

    fn remember(&mut self, id: &Id) {
        self.recent.retain(|r| r != id);
        self.recent.push(id.clone());
    }

    fn on_exit(&mut self, id: &str, code: Option<i32>, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(err) = self.live_view(id).and_then(|v| v.read(cx).start_error()) {
            let program = self.store.session(id).map(|r| r.display_title().to_string()).unwrap_or_default();
            self.toast(format!("Could not start {program}: {err}"), true, cx);
        }
        if self.logins.remove(id) {
            self.refresh_accounts(cx);
        }
        if let Some(rec) = self.store.session_mut(id) {
            rec.exit_code = Some(code.unwrap_or(0));
            rec.last_active = now();
        }
        self.drop_if_empty(id);
        self.store.save_if_dirty();
        cx.notify();
    }

    /// Clicking a session: running -> show its terminal; past -> open it right away (no intermediate page).
    pub fn show_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.selected = Some(id.clone());
        if self.live_view(&id).is_some() {
            self.activate(&id, cx);
            self.remember(&id);
            self.main = Main::Terminals;
            self.sync_github(window, cx);
            self.focus_active(window, cx);
            self.persist_layout();
            cx.notify();
        } else {
            self.resume(&id, window, cx);
        }
    }

    fn persist_layout(&mut self) {
        self.store.data.layout.selected_session = self.selected.clone();
        self.store.mark_dirty();
    }

    pub fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let (Main::Terminals, Some(id)) = (&self.main, self.active.as_ref()) {
            if let Some(view) = self.live_view(id) {
                window.focus(&view.read(cx).focus_handle(cx), cx);
                return;
            }
        }
        window.focus(&self.root_focus, cx);
    }

    fn close_live(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.logins.remove(id) {
            self.refresh_accounts(cx);
        }
        let was_active = self.active_id().is_some_and(|a| a == id);
        self.live.retain(|l| l.id != id); // dropping the view drops the Terminal, which closes the PTY
        self.shells.remove(id);
        self.visuals.remove(id);
        self.recent.retain(|r| r != id);
        // Closing the shown session goes back to the one shown before it; otherwise stay where we are.
        if was_active {
            self.active = self.recent.iter().rev().find(|r| self.live.iter().any(|l| &l.id == *r)).cloned();
            self.selected = self.active.clone();
            self.persist_layout();
        }
        if let Some(rec) = self.store.session_mut(id) {
            if rec.exit_code.is_none() {
                rec.exit_code = Some(-1);
            }
            rec.last_active = now();
        }
        self.drop_if_empty(id);
        self.purge_dead_shells();
        if self.active.is_none() {
            self.main = Main::Home;
        }
        self.store.save_if_dirty();
        self.focus_active(window, cx);
        cx.notify();
    }

    // ------------------------------------------------------------------ actions

    pub fn act(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match act {
            Act::NewClaude { cwd } => self.new_claude(cwd, window, cx),
            Act::ToggleShell(id) => self.toggle_shell(&id, window, cx),
            Act::Select(id) => self.show_session(&id, window, cx),
            Act::Resume(id) => self.resume(&id, window, cx),
            Act::CloseLive(id) => self.close_live(&id, window, cx),
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
            Act::OpenGithub { cwd } => self.open_github(cwd, window, cx),
            Act::RegenTitle(id) => self.regen_title(&id, cx),
            Act::CommitPush(id, cwd) => {
                if self.is_running(&id, cx) {
                    self.run_git(
                        GitOp::CommitPush,
                        move || crate::commit::commit_and_push(&|system, prompt| crate::ask::complete(&id, system, prompt), &cwd).map_err(|e| crate::commit::classify(&cwd, e)),
                        cx,
                    )
                } else {
                    self.toast("Start the session first: the message is written through the running session.", true, cx)
                }
            }
            Act::Pull(cwd) => self.run_git(
                GitOp::Pull,
                move || crate::commit::pull(&cwd).map_err(|e| crate::commit::classify(&cwd, e)),
                cx,
            ),
            Act::SwitchBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::commit::switch_branch(&cwd, &name)?), cx)
            }
            Act::FixGitWithClaude(cwd) => {
                let prompt = "Claudiu's Pull or Commit & Push just failed in this repository because of a merge conflict,                     a diverged branch or a rejected push. Run git status, fix it (resolve conflicts keeping the intent of both sides,                     finish the merge or rebase, then push if the push was rejected).                     Ask me whenever a conflict is ambiguous or you are unsure which side to keep.";
                let args = ["--model", "claude-sonnet-5-5", "--effort", "medium"];
                let leading = std::iter::once(prompt).chain(args).map(String::from).collect();
                let before = self.selected.clone();
                self.new_claude_with(cwd, leading, window, cx);
                if self.selected != before
                    && let Some(id) = self.selected.clone() {
                        self.ephemeral.insert(id);
                    }
            }
            Act::OpenSettings => {
                let s = &self.store.data.settings;
                let (shell_t, claude_t) = (
                    s.shell.clone().unwrap_or_default(),
                    s.claude_path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                );
                let updates = s.update_url.as_deref() != Some("");
                let claude_hint = self.claude_exe.as_ref().map_or("not found".to_string(), |p| p.to_string_lossy().into_owned());
                let claude = self.modal_input(&claude_t, &claude_hint, window, cx);
                let shell = self.modal_input(&shell_t, &platform::default_shell(None).0, window, cx);
                self.modal = Some(Modal::Settings { shell, claude, updates, scale: crate::scale::percent() });
            }
            Act::NewBranch(cwd) => {
                let input = self.modal_input("", "", window, cx);
                self.modal = Some(Modal::NewBranch { cwd, input });
            }
            Act::CreateBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::commit::create_branch(&cwd, &name)?), cx)
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
            Act::ConfirmHandoff(id) => {
                self.modal = None;
                self.start_handoff(&id, cx);
            }
            Act::AddAccount => {
                self.modal = Some(Modal::Confirm {
                    title: "Add an account?".into(),
                    body: "The new account shares your main Claude Code setup: settings, CLAUDE.md, agents, skills, plugins and \
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

    /// Git actions (see commit.rs): one at a time, off the UI thread, reported through a toast.
    fn run_git(
        &mut self,
        op: GitOp,
        job: impl FnOnce() -> Result<String, crate::commit::GitFail> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.git_busy.is_some() {
            return;
        }
        self.git_busy = Some(op);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.git_busy = None;
                // The branch shown in the top bar follows a pull, switch or new branch.
                ws.store.refresh_git();
                ws.store.save_if_dirty();
                match result {
                    Ok(summary) => ws.toast(summary, false, cx),
                    Err(fail) => match fail.conflict_in {
                        Some(cwd) => {
                            let what = if op == GitOp::Pull { "Pull" } else { "Commit & Push" };
                            ws.show_toast(
                                format!("{what} hit a conflict. Ask Claude to fix it?"),
                                true,
                                Some(("Ask Claude".into(), Act::FixGitWithClaude(cwd))),
                                cx,
                            );
                        }
                        None => ws.toast(fail.msg, true, cx),
                    },
                }
            });
        })
        .detach();
    }

    /// Fetch origin for the folder in view once a minute so the branch menu opens instantly and the
    /// remote branches it lists are current. Skipped while another git operation runs.
    fn start_fetch_loop(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(cwd) = this.update(cx, |ws, _| (ws.git_busy.is_none()).then(|| ws.active_cwd())) else { break };
                if let Some(cwd) = cwd {
                    cx.background_executor().spawn(async move { crate::commit::background_fetch(&cwd) }).await;
                }
                cx.background_executor().timer(Duration::from_secs(60)).await;
            }
        })
        .detach();
    }

    /// Re-read each project's git info every few seconds, off the UI thread, so the branch badge follows a
    /// checkout made in a terminal or by Claude, not only Claudiu's own branch actions.
    fn start_git_watch(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let Ok(paths) = this.update(cx, |ws, _| ws.store.data.projects.iter().map(|p| p.path.clone()).collect::<Vec<_>>())
                else {
                    break;
                };
                let infos = cx
                    .background_executor()
                    .spawn(async move { paths.into_iter().map(|p| { let g = crate::git::detect(&p); (p, g) }).collect::<Vec<_>>() })
                    .await;
                let _ = this.update(cx, |ws, cx| {
                    let mut changed = false;
                    for (path, git) in infos {
                        if let Some(p) = ws.store.data.projects.iter_mut().find(|p| p.path == path && p.git != git) {
                            p.git = git;
                            changed = true;
                        }
                    }
                    if changed {
                        ws.store.mark_dirty();
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Branch menu: list branches off the UI thread, then open the menu at `pos`.
    pub fn open_branch_menu(&mut self, cwd: PathBuf, pos: Point<Pixels>, cx: &mut Context<Self>) {
        if self.git_busy.is_some() {
            return;
        }
        self.git_busy = Some(GitOp::Branch);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let dir = cwd.clone();
            let result = cx.background_executor().spawn(async move { crate::commit::branches(&dir) }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.git_busy = None;
                match result {
                    Ok((names, current)) => {
                        // ponytail: the menu doesn't scroll, so only the most recently used branches are listed.
                        let mut items: Vec<MenuItem> = names
                            .into_iter()
                            .take(15)
                            .map(|name| MenuItem::new(format!("{}{name}", crate::widgets::tick(current.as_deref() == Some(name.as_str()))), Act::SwitchBranch(cwd.clone(), name)))
                            .collect();
                        let first = items.is_empty();
                        items.push(MenuItem { label: "New branch…".into(), hint: None, act: Act::NewBranch(cwd), danger: false, disabled: false, separator_before: !first });
                        ws.open_menu(pos, items, cx);
                    }
                    Err(e) => ws.toast(e, true, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A single-line field for a dialog, focused, with Enter submitting the dialog.
    fn modal_input(&mut self, value: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value.to_string()).placeholder(placeholder.to_string()));
        cx.subscribe_in(&input, window, |ws: &mut Self, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                ws.submit_modal(window, cx);
            }
        })
        .detach();
        input.update(cx, |s, cx| s.focus(window, cx));
        input
    }

    /// Enter / OK in a modal.
    pub fn submit_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let act = match self.modal.take() {
            Some(Modal::Confirm { act, .. }) => act,
            Some(Modal::Rename { id, input }) => {
                let text = input.read(cx).value().trim().to_string();
                if let Some(rec) = self.store.session_mut(&id) {
                    rec.custom_title = (!text.is_empty()).then_some(text);
                }
                Act::Dismiss
            }
            Some(Modal::Advisor { id, input }) => {
                let name = input.read(cx).value().trim().to_lowercase();
                if name.is_empty() {
                    Act::Dismiss
                } else if !usage::valid_model_name(&name) {
                    self.modal = Some(Modal::Advisor { id, input });
                    self.toast("Use a model alias or id (letters, digits, . _ -), or “off”.", true, cx);
                    return;
                } else {
                    self.act(Act::SetAdvisor(id, name), window, cx);
                    Act::Dismiss
                }
            }
            Some(Modal::NewBranch { cwd, input }) => {
                // Spaces aren't allowed in branch names; git reports anything else that is invalid.
                let name = input.read(cx).value().split_whitespace().collect::<Vec<_>>().join("-");
                if name.is_empty() { Act::Dismiss } else { Act::CreateBranch(cwd, name) }
            }
            Some(Modal::Settings { shell, claude, updates, scale }) => {
                let clean = |s: &str| s.trim().trim_matches('"').to_string();
                let (shell_t, claude_t) = (clean(shell.read(cx).value().as_ref()), clean(claude.read(cx).value().as_ref()));
                let bad = [("Shell", &shell_t), ("Claude path", &claude_t)]
                    .into_iter()
                    .find(|(_, t)| !t.is_empty() && platform::resolve_program(t).is_none());
                if let Some((what, t)) = bad {
                    let msg = format!("{what}: “{t}” is not a file or a program on PATH");
                    self.modal = Some(Modal::Settings { shell, claude, updates, scale });
                    self.toast(msg, true, cx);
                    return;
                }
                let s = &mut self.store.data.settings;
                s.shell = (!shell_t.is_empty()).then_some(shell_t);
                s.claude_path = platform::resolve_program(&claude_t);
                // Only the on/off state is edited here; a custom feed URL survives while checks stay on.
                if updates == (s.update_url.as_deref() == Some("")) {
                    s.update_url = (!updates).then(String::new);
                }
                s.scale = (scale != 100).then_some(scale);
                crate::scale::set(scale);
                self.shell = platform::default_shell(s.shell.as_deref());
                self.claude_exe = claude::discover(s.claude_path.as_deref());
                self.store.mark_dirty();
                self.toast("Settings saved. Open sessions keep their current shell.", false, cx);
                Act::Dismiss
            }
            None => return,
        };
        self.focus_active(window, cx);
        self.act(act, window, cx);
    }

    /// The editor "Open in" uses: the last one picked if it is still installed, else the first one found.
    pub fn preferred_editor(&self) -> Option<Editor> {
        let installed = |e: &Editor| self.editors.iter().any(|(x, ok)| x == e && *ok);
        self.store.data.settings.editor.filter(installed).or_else(|| Editor::ALL.into_iter().find(installed))
    }

    fn import_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next() {
                    let _ = this.update_in(cx, |ws, window, cx| {
                        ws.act(Act::AddProject(path), window, cx)
                    });
                }
        })
        .detach();
    }

    pub fn open_menu(&mut self, pos: Point<Pixels>, items: Vec<MenuItem>, cx: &mut Context<Self>) {
        self.menu = Some(Menu { pos, items });
        cx.notify();
    }

    // ------------------------------------------------------------------ GitHub panes

    /// Show the project's GitHub view in the right sidebar and move the keyboard into it.
    pub fn open_github(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.github_pane(cwd, window, cx);
        self.right_gh = Some(id.clone());
        self.store.data.layout.right_hidden = false;
        self.store.mark_dirty();
        if let Some(p) = self.gh_pane(&id) {
            let handle = p.view.read(cx).focus.clone();
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// An attached issue or pull request was clicked: show it in the GitHub view of the shown session's project.
    pub fn open_attached(&mut self, pull: bool, number: u32, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd();
        self.open_github(cwd, window, cx);
        let kind = if pull { crate::github::model::Kind::Pull } else { crate::github::model::Kind::Issue };
        if let Some(view) = self.right_gh.as_ref().and_then(|id| self.gh_pane(id)).map(|p| p.view.clone()) {
            view.update(cx, |v, cx| v.open_item(kind, number, window, cx));
        }
    }

    /// While the right sidebar shows GitHub, it follows the shown session to that session's project (no focus change).
    fn sync_github(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only the panel on screen follows; other projects' panes are made when the panel opens.
        if self.right_gh.is_none() {
            return;
        }
        let Some(id) = self.active.clone() else { return };
        if let Some(cwd) = self.store.session(&id).map(|s| s.cwd.clone()) {
            self.right_gh = Some(self.github_pane(cwd, window, cx));
        }
    }

    /// The project's GitHub view (one per project), created on first use.
    fn github_pane(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Id {
        let project = self.store.project_for_cwd(&cwd).and_then(|id| self.store.project(&id)).cloned();
        let root = project.as_ref().map(|p| p.path.clone()).unwrap_or(cwd);
        let key = platform::path_key(&root);
        match self.gh.iter().find(|p| platform::path_key(&p.cwd) == key) {
            Some(p) => p.id.clone(),
            None => {
                let name = project
                    .map(|p| p.name)
                    .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "GitHub".into());
                let id = new_id();
                let view = cx.new(|cx| GithubView::new(root.clone(), name.clone(), crate::github::cli::locate().map(|exe| crate::github::cli::Gh::new(exe, root.clone())), window, cx));
                let sub_id = id.clone();
                let sub = cx.subscribe_in(&view, window, move |ws: &mut Workspace, _view, ev: &GhEvent, window, cx| ws.on_gh_event(&sub_id, ev, window, cx));
                // Returning to the window refreshes the pane, but only the one that is on screen.
                let (shown, watched) = (id.clone(), view.clone());
                let active = cx.observe_window_activation(window, move |ws: &mut Workspace, window, cx| {
                    if window.is_window_active() && ws.right_gh.as_ref() == Some(&shown) && !ws.store.data.layout.right_hidden {
                        watched.update(cx, |v, cx| v.refresh_on_return(window, cx));
                    }
                });
                self.gh.push(GhPane { id: id.clone(), cwd: root, project: name, view, _sub: sub, _active: active });
                id
            }
        }
    }

    pub fn toggle_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = &mut self.store.data.layout;
        layout.right_hidden = !layout.right_hidden;
        self.store.mark_dirty();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// GitHub and the Visual panel are read and written in, so they share one wider, resizable width.
    fn right_wide(&self) -> bool {
        self.right_gh.is_some() || self.visual_shown()
    }

    /// The right sidebar's current width: the remembered one for the menu or for GitHub, kept within bounds.
    pub fn right_width(&self, window_w: f32) -> f32 {
        self.right_width_of(self.right_wide(), window_w)
    }

    pub fn right_width_of(&self, wide: bool, window_w: f32) -> f32 {
        let layout = &self.store.data.layout;
        let room = (window_w - 560.0).max(RIGHT_GH_MIN);
        if !wide {
            return layout.right_menu_w.unwrap_or(RIGHT_MENU_W).clamp(RIGHT_MENU_MIN, RIGHT_MENU_MAX.min(room));
        }
        layout.right_gh_w.unwrap_or(RIGHT_GH_W).clamp(RIGHT_GH_MIN, room)
    }

    /// The right sidebar's left edge is being dragged: follow the pointer, wider to the left.
    pub fn drag_right(&mut self, x: f32, window_w: f32) {
        let Some(d) = self.right_drag.as_ref() else { return };
        let wanted = d.width + d.x - x;
        let room = (window_w - 560.0).max(RIGHT_GH_MIN);
        let wide = self.right_wide();
        let layout = &mut self.store.data.layout;
        if !wide {
            layout.right_menu_w = Some(wanted.clamp(RIGHT_MENU_MIN, RIGHT_MENU_MAX.min(room)));
        } else {
            layout.right_gh_w = Some(wanted.clamp(RIGHT_GH_MIN, room));
        }
        self.store.mark_dirty();
    }

    /// The left sidebar's current width, kept within bounds and leaving room for the session.
    pub fn left_width(&self, window_w: f32) -> f32 {
        let wanted = self.store.data.layout.left_w.unwrap_or(SIDEBAR_W);
        wanted.clamp(SIDEBAR_MIN, SIDEBAR_MAX.min((window_w - 560.0).max(SIDEBAR_MIN)))
    }

    /// The left sidebar's right edge is being dragged: follow the pointer, wider to the right.
    pub fn drag_left(&mut self, x: f32, window_w: f32) {
        let Some(d) = self.left_drag.as_ref() else { return };
        let wanted = d.width + x - d.x;
        self.store.data.layout.left_w = Some(wanted.clamp(SIDEBAR_MIN, SIDEBAR_MAX.min((window_w - 560.0).max(SIDEBAR_MIN))));
        self.store.mark_dirty();
    }

    fn on_gh_event(&mut self, pane: &str, ev: &GhEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.gh_pane(pane).map(|p| p.cwd.clone()) else { return };
        match ev {
            GhEvent::Toast { text, error } => self.toast(text.clone(), *error, cx),
            GhEvent::AskClaude(prompt) => self.new_claude_with(cwd, vec![prompt.clone()], window, cx),
            GhEvent::Checkout(number) => {
                let Some(exe) = crate::github::cli::locate() else { return };
                let number = *number;
                self.run_git(
                    GitOp::Branch,
                    move || crate::github::cli::Gh::new(exe, cwd).checkout(number).map_err(|e| e.msg.into()),
                    cx,
                );
            }
            GhEvent::SignIn => {
                let Some(exe) = crate::github::cli::locate() else { return };
                let home = platform::home_dir().unwrap_or_else(|| PathBuf::from("."));
                let spec = SpawnSpec { program: exe.to_string_lossy().into_owned(), args: vec!["auth".into(), "login".into()], cwd: Some(home.clone()), ..Default::default() };
                let record = SessionRecord {
                    id: new_id(),
                    kind: SessionKind::Shell,
                    title: "GitHub sign in".into(),
                    custom_title: None,
                    auto_title: None,
                    project_id: None,
                    cwd: home,
                    created_at: now(),
                    last_active: now(),
                    exit_code: None,
                    hidden: false,
                    claude: None,
                };
                self.start(record, spec, true, window, cx);
            }
        }
    }

    // ------------------------------------------------------------------ session switching

    pub fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let running: Vec<Id> = self
            .live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
            .map(|l| l.id.clone())
            .collect();
        if running.is_empty() {
            return;
        }
        let cur = self
            .active_id()
            .and_then(|a| running.iter().position(|r| r == a))
            .unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(running.len() as isize) as usize;
        self.show_session(&running[next].clone(), window, cx);
    }

    /// Prompt-cache timer for the selected session: `(remaining fraction, remaining secs, ttl secs)`.
    /// Every request that reads or writes the cache restarts its TTL; that moment is the timestamp of the last
    /// assistant response that used it (not the transcript's mtime, which user messages and renames also bump).
    /// Unknown TTL assumes the 5-minute tier.
    pub fn selected_cache(&self) -> Option<(f32, i64, i64)> {
        let id = self.selected.as_ref().or(self.active_id())?;
        let meta = self.store.session(id)?.claude.as_ref()?;
        let last = meta.cache_at?;
        let ttl = meta.cache_ttl.unwrap_or(300) as i64;
        let left = (last + ttl - now()).clamp(0, ttl);
        Some((left as f32 / ttl as f32, left, ttl))
    }

    /// Context-meter source for the selected session: `(fraction, tokens, exact, full at)`. `exact` means Claude Code
    /// itself reported the figures; otherwise they are estimated from the transcript. `full at` is the token count
    /// the bar ends at (`claude::context_limit`), when known.
    pub fn selected_context(&self) -> Option<(f32, u64, bool, Option<u64>)> {
        let id = self.selected.as_ref().or(self.active_id())?;
        let scaled = |tokens: u64, limit: Option<u64>| limit.map(|l| (tokens as f64 / l as f64).min(1.0) as f32);
        let meta = self.store.session(id)?.claude.as_ref()?;
        let limit = claude::context_limit(meta.context_window, meta.compact_at, meta.auto_compact);
        if let Some(f) = meta.context_fraction {
            let tokens = meta.context_tokens.unwrap_or_else(|| meta.context_window.map_or(0, |w| (w as f64 * f as f64) as u64));
            return Some((scaled(tokens, limit).unwrap_or(f), tokens, true, limit));
        }
        let tokens = meta.context_tokens?;
        Some((claude::context_fraction(tokens), tokens, false, None))
    }
}
