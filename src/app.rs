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
use crate::github::view::{GhEvent, GithubView};
use crate::mcp;
use crate::platform::{self, Editor};
use crate::statusline;
use crate::store::{
    Account, ClaudeMeta, ExternalState, Id, MAIN_ACCOUNT, SessionKind, SessionRecord, Store, new_id, now,
};
use crate::terminal::SpawnSpec;
use crate::terminal_view::{TerminalEvent, TerminalView};
use crate::updater::UpdateState;

actions!(
    claudiu,
    [
        NewClaude,
        NewShell,
        CloseSession,
        NextSession,
        PrevSession,
        SplitPane,
        ToggleSidebar,
        FocusSearch,
        ImportProject,
        FocusNextPane,
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
    NewShell {
        cwd: PathBuf,
    },
    /// Click on a session row: live -> show its terminal alone (leaving any split), history -> resume it.
    Select(Id),
    /// Put a session (resuming it if it ended) next to the focused pane.
    AddToSplit(Id),
    RemoveFromSplit(Id),
    ExitSplit,
    SplitShell,
    SplitClaude(PathBuf),
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
    /// The project's GitHub issues and pull requests; `split` puts the pane next to the current one.
    OpenGithub {
        cwd: PathBuf,
        split: bool,
    },
    /// Stage everything in this folder's repository, commit it with a Haiku-written message, push.
    CommitPush(PathBuf),
    Pull(PathBuf),
    SwitchBranch(PathBuf, String),
    /// Ask for a name, then `CreateBranch`.
    NewBranch(PathBuf),
    CreateBranch(PathBuf, String),
    /// New Claude session (Sonnet 5.5, medium effort) that resolves a failed pull / push in this repository.
    FixGitWithClaude(PathBuf),
    AddProject(PathBuf),
    RemoveProject(Id),
    ToggleProject(Id),
    ToggleOther,
    ShowAll(Id),
    ConfirmQuit,
    ConfirmInstallUpdate,
    Dismiss,
    InstallUpdate,
    /// New sessions start with this account; running ones keep theirs.
    SwitchAccount(Id),
    /// Shows the sharing warning, then `ConfirmAddAccount`.
    AddAccount,
    ConfirmAddAccount,
    /// Run `claude auth login` for an alt account in a terminal tab.
    SignIn(Id),
    RemoveAccount(Id),
    ConfirmRemoveAccount(Id),
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
    /// Empty text = use the default.
    Settings {
        shell: Entity<InputState>,
        claude: Entity<InputState>,
        updates: bool,
    },
}

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
}

/// A GitHub pane. Like a shell it exists only while open and is never written to `state.json`
/// (an older Claudiu sharing that file would not understand a new session kind).
pub struct GhPane {
    pub id: Id,
    /// The project's root folder: the pane's repository is whatever `gh` resolves from it.
    pub cwd: PathBuf,
    pub project: String,
    pub view: Entity<GithubView>,
    pub _sub: Subscription,
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub serial: u64,
    /// Button label and what it does; the toast stays up longer so there is time to answer.
    pub action: Option<(String, Act)>,
}

pub const CLOSE_MS: u64 = 180;

pub struct Workspace {
    pub store: Store,
    pub claude_exe: Option<PathBuf>,
    pub shell: (String, Vec<String>),
    pub live: Vec<Live>,
    pub gh: Vec<GhPane>,
    pub panes: Vec<Id>,
    /// Splits that are not on screen right now; a session in one of them brings its whole split back when shown.
    pub groups: Vec<Vec<Id>>,
    /// Where the shown split sits among `groups` in the sidebar, so cards do not jump when you switch.
    active_slot: usize,
    pub focused: usize,
    /// Open sessions in the order they were last shown (most recent last); closing one goes back to the previous.
    recent: Vec<Id>,
    pub split_vertical: bool,
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
    pub show_all: HashSet<Id>,
    /// Projects whose session list is still sliding shut.
    pub closing: HashSet<Id>,
    /// Sessions that leave no history once they end (the "fix with Claude" helper).
    ephemeral: HashSet<Id>,
    /// `claude auth login` tabs; account details are re-read when one ends.
    logins: HashSet<Id>,
    /// Account the "switch to a less used account" suggestion was shown for; cleared once its use drops.
    limit_warned: Option<Id>,
    pub editors: Vec<(Editor, bool)>,
    pub transcript_mtimes: HashMap<Id, SystemTime>,
    pub window_title: String,
    /// The session or project being dragged in the sidebar (dimmed in place while its ghost follows the cursor).
    pub dragging: Option<Id>,
    /// Pointer position at the last drag-over check, see `pointer_moved`.
    /// Sidebar reorder animation: last drawn order, painted project block heights, and the slides in flight.
    pub prev_current: Vec<Id>,
    pub prev_projects: Vec<Id>,
    pub block_heights: std::rc::Rc<std::cell::RefCell<HashMap<Id, f32>>>,
    pub slides: HashMap<Id, crate::sidebar::Slide>,
    pub slide_gen: u64,
    /// When a drag last reordered the sidebar, see `pointer_moved`.
    pub last_swap: std::time::Instant,
    pub drag_pos: Option<gpui::Point<gpui::Pixels>>,
    pub update: UpdateState,
    pub scanning: bool,
    pub quitting: bool,
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
        let mut store = Store::load(Store::default_path());
        // Projects start collapsed on every launch.
        for p in &mut store.data.projects {
            p.collapsed = true;
        }
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
            panes: vec![],
            groups: vec![],
            active_slot: usize::MAX,
            focused: 0,
            recent: vec![],
            split_vertical: false,
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
            show_all: HashSet::new(),
            closing: HashSet::new(),
            ephemeral: HashSet::new(),
            logins: HashSet::new(),
            limit_warned: None,
            editors: Editor::ALL
                .iter()
                .map(|e| (*e, e.locate().is_some()))
                .collect(),
            transcript_mtimes: HashMap::new(),
            window_title: String::new(),
            dragging: None,
            drag_pos: None,
            prev_current: Vec::new(),
            prev_projects: Vec::new(),
            block_heights: Default::default(),
            slides: HashMap::new(),
            slide_gen: 0,
            last_swap: std::time::Instant::now(),
            update: UpdateState::default(),
            scanning: false,
            quitting: false,
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
        statusline::cleanup();
        ws.ingest_statusline(cx);
        ws.refresh_accounts(cx);
        ws.start_ticker(cx);
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

    /// Keep the newest reading per window. Returns whether anything changed (the same snapshot is re-read
    /// every tick, so an unconditional save would rewrite state.json every few seconds).
    fn merge_account(
        &mut self,
        account: &str,
        five: Option<crate::store::RateLimit>,
        seven: Option<crate::store::RateLimit>,
    ) -> bool {
        // A removed account's leftover snapshots are ignored.
        let Some(acct) = self.store.data.accounts.iter_mut().find(|a| a.id == account).map(|a| &mut a.usage) else {
            return false;
        };
        let mut changed = false;
        for (slot, new) in [(&mut acct.five_hour, five), (&mut acct.seven_day, seven)] {
            if let Some(new) = new
                && slot.as_ref().is_none_or(|cur| new.seen_at >= cur.seen_at && *cur != new)
            {
                *slot = Some(new);
                changed = true;
            }
        }
        if changed {
            self.store.mark_dirty();
        }
        changed
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
                let statuses = cx
                    .background_executor()
                    .spawn(async { claude::claude_home().map(|h| claude::session_statuses(&h)).unwrap_or_default() })
                    .await;
                if this.update_in(cx, |ws, window, cx| ws.update_activity(window, cx, &statuses)).is_err() {
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
        self.ingest_statusline(cx);
        self.suggest_less_used_account(cx);
        cx.notify();
    }

    /// When the active account's 5-hour or 7-day use reaches 90% and another signed-in account has used less,
    /// offer to switch to the least used one. Once per crossing; it re-arms after the window resets.
    fn suggest_less_used_account(&mut self, cx: &mut Context<Self>) {
        const HIGH: f32 = 0.9;
        let t = now();
        let Some(active) = self.store.active_account() else { return };
        let peak = active.usage.peak(t);
        if peak < HIGH {
            self.limit_warned = None;
            return;
        }
        if self.limit_warned.as_ref() == Some(&active.id) {
            return;
        }
        let best = self
            .store
            .data
            .accounts
            .iter()
            .filter(|a| a.id != active.id && (a.is_main() || a.email.is_some()))
            .map(|a| (a, a.usage.peak(t)))
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .filter(|(_, p)| *p < peak);
        let Some((best, best_peak)) = best else { return };
        let text = format!(
            "{} has used {:.0}% of a usage limit. {} is at {:.0}%.",
            active.name(),
            peak * 100.0,
            best.name(),
            best_peak * 100.0
        );
        let act = Act::SwitchAccount(best.id.clone());
        self.limit_warned = Some(active.id.clone());
        self.show_toast(text, true, Some(("Switch".into(), act)), cx);
    }

    /// Pick up what Claude Code's status line (via our per-process tee) wrote: account limits and exact context,
    /// plus the tab titles Claude chose through the MCP tool.
    /// Costs no tokens; the files are tiny, but the read still happens off the UI thread.
    fn ingest_statusline(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (snaps, titles) = cx
                .background_executor()
                .spawn(async move { (statusline::read_snapshots(&statusline::snapshot_dir()), mcp::read_titles()) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                let mut changed = false;
                for (stem, title, stamp) in titles {
                    let Some(rec) = ws
                        .store
                        .data
                        .sessions
                        .iter_mut()
                        .find(|s| s.claude_id().is_some_and(|id| statusline::safe_file_stem(id) == stem))
                    else {
                        continue;
                    };
                    if let Some(l) = ws.live.iter_mut().find(|l| l.id == rec.id)
                        && l.title_stamp != stamp
                    {
                        // The first read only records the stamp: a resumed session already got `--name`.
                        if l.title_stamp.is_some() {
                            l.claude_name = None;
                        }
                        l.title_stamp = stamp;
                    }
                    if rec.auto_title.as_deref() != Some(title.as_str()) {
                        rec.auto_title = Some(title);
                        changed = true;
                    }
                }
                for snap in snaps {
                    let account = snap.account.as_deref().unwrap_or(MAIN_ACCOUNT);
                    changed |= ws.merge_account(account, snap.five_hour.clone(), snap.seven_day.clone());
                    if let Some(rec) = ws
                        .store
                        .data
                        .sessions
                        .iter_mut()
                        .find(|s| s.claude_id() == Some(snap.session_id.as_str()))
                        && let Some(meta) = rec.claude.as_mut() {
                            let (f, t, w) = (
                                snap.context_fraction,
                                snap.context_tokens,
                                snap.context_window,
                            );
                            if f.is_some()
                                && (meta.context_fraction != f || meta.context_window != w)
                            {
                                meta.context_fraction = f;
                                meta.context_window = w;
                                if t.is_some() {
                                    meta.context_tokens = t;
                                }
                                changed = true;
                            }
                            if snap.model.is_some() && meta.model != snap.model {
                                meta.model = snap.model.clone();
                                changed = true;
                            }
                        }
                }
                if changed {
                    ws.store.mark_dirty();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Per-process additions to a Claude launch; nothing in the user's Claude Code config changes:
    /// the status-line tee (`--settings`), the tab-title MCP server (`--mcp-config`, its one tool pre-allowed),
    /// and IDE integration switched off through environment variables.
    fn with_helpers(
        &self,
        mut spec: SpawnSpec,
        claude_session_id: &str,
        cwd: &std::path::Path,
    ) -> SpawnSpec {
        if let Ok((path, env)) = statusline::prepare(claude_session_id, cwd) {
            spec.args.push("--settings".into());
            spec.args.push(path.to_string_lossy().into_owned());
            spec.env.extend(env);
        }
        if let Ok(config) = mcp::prepare(claude_session_id) {
            spec.args.extend(["--allowedTools".into(), mcp::ALLOWED_TOOL.into(), "--mcp-config".into(), config.to_string_lossy().into_owned()]);
        }
        if let Ok(dir) = mcp::prepare_mod() {
            spec.args.extend(["--plugin-dir".into(), dir.to_string_lossy().into_owned()]);
            spec.env.push(("CLAUDIU_NAME_FILE".into(), mcp::name_file(claude_session_id).to_string_lossy().into_owned()));
        }
        spec.env.push(("CLAUDE_CODE_AUTO_CONNECT_IDE".into(), "false".into()));
        spec.env.push(("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL".into(), "1".into()));
        if let Some(acct) = self.store.active_account() {
            spec.env.push((statusline::ACCOUNT_ENV.into(), acct.id.clone()));
            spec.env.extend(Self::account_env(acct));
        }
        (spec.program, spec.args) = platform::through_login_shell(spec.program, spec.args);
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
            .is_some_and(|id| self.store.session(id).is_none() && self.gh_pane(id).is_none())
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

    pub fn persist_for_restart(&mut self) {
        self.finalize();
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
            Main::Terminals => self.panes.get(self.focused),
            Main::Home => None,
        }
    }

    pub fn gh_pane(&self, id: &str) -> Option<&GhPane> {
        self.gh.iter().find(|p| p.id == id)
    }

    pub fn active_cwd(&self) -> PathBuf {
        self.active_id()
            .and_then(|id| self.store.session(id).map(|s| s.cwd.clone()).or_else(|| self.gh_pane(id).map(|p| p.cwd.clone())))
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
        let mut spec = claude::new_session_spec(&exe, &cwd, &session_id);
        spec.args.splice(0..0, leading);
        let spec = self.with_helpers(spec, &session_id, &cwd);
        let record = SessionRecord {
            id: new_id(),
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

    pub fn new_shell(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let (program, args) = self.shell.clone();
        // A session's folder may be gone (temp dirs, deleted worktrees); CreateProcess then fails with "filename syntax is incorrect".
        let cwd = if cwd.is_dir() { cwd } else { platform::home_dir().unwrap_or(cwd) };
        let spec = SpawnSpec {
            program: program.clone(),
            args,
            cwd: Some(cwd.clone()),
            ..Default::default()
        };
        let record = SessionRecord {
            id: new_id(),
            kind: SessionKind::Shell,
            title: platform::shell_label(&program),
            custom_title: None,
            auto_title: None,
            project_id: self.store.project_for_cwd(&cwd),
            cwd,
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            claude: None,
        };
        self.start(record, spec, true, window, cx);
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
            // Shell history can't be resumed: open a fresh shell in the same folder.
            self.new_shell(rec.cwd, window, cx);
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
        let spec = self.with_helpers(spec, &meta.session_id, &rec.cwd);
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
        self.live.push(Live {
            id: id.clone(),
            view,
            _sub: sub,
            claude_name,
            title_stamp: None,
        });
        self.place_in_pane(&id, cx);
        self.remember(&id);
        self.selected = Some(id.clone());
        self.main = Main::Terminals;
        self.persist_layout();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// A session already in the split is just focused. One that a split action made room for (`with_pane_after`)
    /// joins it. Anything else switches layouts: the current split is parked as it is, and the session shows
    /// with its own split if it has one, else alone. Clicking never swaps a pane or breaks a split up.
    fn place_in_pane(&mut self, id: &Id, cx: &App) {
        if let Some(pos) = self.panes.iter().position(|p| p == id) {
            self.focused = pos;
        } else if let Some(slot) = self.panes.iter().position(|p| p.is_empty()) {
            self.leave_stored(id);
            self.panes[slot] = id.clone();
            self.focused = slot;
        } else {
            let (panes, slot, parked) = switch_layout(self.splits(), id);
            self.focused = panes.iter().position(|p| p == id).unwrap_or(0);
            (self.panes, self.active_slot, self.groups) = (panes, slot, parked);
        }
        self.prune_live(cx);
    }

    /// Every split in sidebar order: the parked ones, with the shown one (if it is a split) at its own slot.
    pub fn splits(&self) -> Vec<Vec<Id>> {
        let mut all = self.groups.clone();
        if self.panes.len() > 1 {
            all.insert(self.active_slot.min(all.len()), self.panes.clone());
        }
        all
    }

    /// Take `id` out of any parked split; a split left with one session dissolves.
    fn leave_stored(&mut self, id: &str) {
        for g in &mut self.groups {
            g.retain(|p| p != id);
        }
        self.groups.retain(|g| g.len() > 1);
    }

    /// Exited terminals are only kept while a pane (or a parked split) still shows them.
    fn prune_live(&mut self, cx: &App) {
        let (panes, groups) = (&self.panes, &self.groups);
        self.live
            .retain(|l| panes.contains(&l.id) || groups.iter().any(|g| g.contains(&l.id)) || l.view.read(cx).is_running());
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

    fn on_exit(&mut self, id: &str, code: Option<i32>, window: &mut Window, cx: &mut Context<Self>) {
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
        // A shell that exits inside a split has nothing left to show: close its pane.
        let is_shell = self.store.session(id).is_some_and(|r| r.kind == SessionKind::Shell);
        if is_shell && self.panes.len() > 1 && self.panes.iter().any(|p| p == id) {
            self.close_live(id, window, cx);
            return;
        }
        self.drop_if_empty(id);
        self.store.save_if_dirty();
        cx.notify();
    }

    /// Clicking a session: running -> show its terminal; past -> open it right away (no intermediate page).
    pub fn show_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.selected = Some(id.clone());
        if self.live_view(&id).is_some() || self.gh_pane(&id).is_some() {
            self.place_in_pane(&id, cx);
            self.remember(&id);
            self.main = Main::Terminals;
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
        if let (Main::Terminals, Some(id)) = (&self.main, self.panes.get(self.focused)) {
            if let Some(view) = self.live_view(id) {
                window.focus(&view.read(cx).focus_handle(cx), cx);
                return;
            }
            if let Some(p) = self.gh_pane(id) {
                // A text box inside the pane may already hold the focus (it was just clicked); leave it there.
                let handle = p.view.read(cx).focus.clone();
                if !handle.contains_focused(window, cx) {
                    window.focus(&handle, cx);
                }
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
        let current = self.active_id().cloned();
        self.live.retain(|l| l.id != id); // dropping the view drops the Terminal, which closes the PTY
        self.gh.retain(|p| p.id != id);
        self.panes.retain(|p| p != id);
        self.leave_stored(id);
        self.recent.retain(|r| r != id);
        // Closing the shown session goes back to the one shown before it; otherwise stay where we are.
        let target = if was_active {
            self.recent.iter().rev().find(|r| self.live.iter().any(|l| &l.id == *r) || self.gh.iter().any(|p| &p.id == *r)).cloned()
        } else {
            current
        };
        match target.as_ref().map(|t| (t, self.panes.iter().position(|p| p == t))) {
            Some((_, Some(pos))) => self.focused = pos,
            Some((t, None)) if self.panes.is_empty() => {
                self.panes.push(t.clone());
                self.focused = 0;
            }
            _ => self.focused = self.focused.min(self.panes.len().saturating_sub(1)),
        }
        if was_active {
            self.selected = self.panes.get(self.focused).cloned();
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
        if self.panes.is_empty() {
            self.main = Main::Home;
        }
        self.store.save_if_dirty();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Called by the window's close handler. Returns true when it is fine to close right now: asks first only
    /// while a Claude session is mid-turn; idle sessions just end with the app.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.quitting {
            return true;
        }
        let working = self.live.iter().filter(|l| {
            let v = l.view.read(cx);
            v.is_running() && v.is_working() && self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude)
        });
        let working = working.count();
        if working == 0 {
            self.finalize();
            return true;
        }
        self.modal = Some(Modal::Confirm {
            title: "Quit Claudiu?".into(),
            body: format!(
                "{working} Claude session{} still working. Quitting interrupts {} — sessions stay in your history and can be resumed.",
                if working == 1 { " is" } else { "s are" },
                if working == 1 { "it" } else { "them" }
            ),
            confirm: "Quit anyway".into(),
            danger: true,
            act: Act::ConfirmQuit,
        });
        window.focus(&self.modal_focus, cx);
        cx.notify();
        false
    }

    fn finalize(&mut self) {
        self.quitting = true;
        let t = now();
        let running: Vec<Id> = self.live.iter().map(|l| l.id.clone()).collect();
        for id in running {
            if let Some(rec) = self.store.session_mut(&id)
                && rec.exit_code.is_none() {
                    rec.exit_code = Some(-1);
                    rec.last_active = t;
                }
        }
        // Terminal sessions are never persisted, nor are ephemeral ones.
        let ephemeral = &self.ephemeral;
        self.store
            .data
            .sessions
            .retain(|s| s.kind != SessionKind::Shell && !ephemeral.contains(&s.id));
        self.store.save();
    }

    // ------------------------------------------------------------------ actions

    pub fn act(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match act {
            Act::NewClaude { cwd } => self.new_claude(cwd, window, cx),
            Act::NewShell { cwd } => self.new_shell(cwd, window, cx),
            Act::Select(id) => self.show_session(&id, window, cx),
            Act::AddToSplit(id) => self.add_to_split(&id, window, cx),
            Act::RemoveFromSplit(id) => self.remove_from_split(&id, window, cx),
            Act::ExitSplit => self.exit_split(window, cx),
            Act::SplitShell => self.split(window, cx),
            Act::SplitClaude(cwd) => self.split_claude(cwd, window, cx),
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
            Act::OpenGithub { cwd, split } => self.open_github(cwd, split, window, cx),
            Act::CommitPush(cwd) => match self.claude_exe.clone() {
                Some(exe) => self.run_git(
                    GitOp::CommitPush,
                    move || crate::commit::commit_and_push(&exe, &cwd).map_err(|e| crate::commit::classify(&cwd, e)),
                    cx,
                ),
                None => self.toast("Could not find the `claude` CLI.", true, cx),
            },
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
                self.modal = Some(Modal::Settings { shell, claude, updates });
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
                self.toast(
                    "Project removed. Its sessions are kept under “Other sessions”.",
                    false,
                    cx,
                );
            }
            Act::ToggleProject(id) => {
                if let Some(p) = self.store.data.projects.iter_mut().find(|p| p.id == id) {
                    p.collapsed = !p.collapsed;
                    if p.collapsed {
                        // Keep rendering the list while it slides shut.
                        self.closing.insert(id.clone());
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(Duration::from_millis(CLOSE_MS)).await;
                            this.update(cx, |ws, cx| {
                                ws.closing.remove(&id);
                                cx.notify();
                            })
                            .ok();
                        })
                        .detach();
                    } else {
                        self.closing.remove(&id);
                    }
                    self.store.mark_dirty();
                }
            }
            Act::ToggleOther => {
                let l = &mut self.store.data.layout;
                l.other_collapsed = !l.other_collapsed;
                self.store.mark_dirty();
            }
            Act::ShowAll(id) => {
                self.show_all.insert(id);
            }
            Act::ConfirmQuit => {
                self.modal = None;
                self.finalize();
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
                self.toast("Switched. New and resumed sessions use this account; running ones keep theirs.", false, cx);
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
                            .map(|name| MenuItem {
                                label: format!("{}{name}", if current.as_deref() == Some(name.as_str()) { "✓  " } else { "    " }),
                                hint: None,
                                disabled: false,
                                act: Act::SwitchBranch(cwd.clone(), name),
                                danger: false,
                                separator_before: false,
                            })
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
            Some(Modal::NewBranch { cwd, input }) => {
                // Spaces aren't allowed in branch names; git reports anything else that is invalid.
                let name = input.read(cx).value().split_whitespace().collect::<Vec<_>>().join("-");
                if name.is_empty() { Act::Dismiss } else { Act::CreateBranch(cwd, name) }
            }
            Some(Modal::Settings { shell, claude, updates }) => {
                let clean = |s: &str| s.trim().trim_matches('"').to_string();
                let (shell_t, claude_t) = (clean(shell.read(cx).value().as_ref()), clean(claude.read(cx).value().as_ref()));
                let bad = [("Shell", &shell_t), ("Claude path", &claude_t)]
                    .into_iter()
                    .find(|(_, t)| !t.is_empty() && platform::resolve_program(t).is_none());
                if let Some((what, t)) = bad {
                    let msg = format!("{what}: “{t}” is not a file or a program on PATH");
                    self.modal = Some(Modal::Settings { shell, claude, updates });
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

    /// Show the project's GitHub pane (one per project), creating it on first use. With `split` it opens
    /// beside the current pane instead of replacing it.
    pub fn open_github(&mut self, cwd: PathBuf, split: bool, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.store.project_for_cwd(&cwd).and_then(|id| self.store.project(&id)).cloned();
        let root = project.as_ref().map(|p| p.path.clone()).unwrap_or(cwd);
        let key = platform::path_key(&root);
        let id = match self.gh.iter().find(|p| platform::path_key(&p.cwd) == key) {
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
                self.gh.push(GhPane { id: id.clone(), cwd: root, project: name, view, _sub: sub });
                id
            }
        };
        if split && !self.panes.contains(&id) {
            self.with_pane_after(self.focused, |ws| ws.show_session(&id, window, cx));
        } else {
            self.show_session(&id, window, cx);
        }
    }

    /// Run `f` (which fills one pane, e.g. by starting a session) with an empty slot opened after pane `at`.
    /// At the four-pane limit, or with no panes yet, `f` simply replaces the focused pane as usual.
    fn with_pane_after(&mut self, at: usize, f: impl FnOnce(&mut Self)) {
        let mut replaced = None;
        if self.panes.is_empty() {
        } else if self.panes.len() < 4 {
            let slot = (at + 1).min(self.panes.len());
            self.panes.insert(slot, String::new());
            self.focused = slot;
        } else {
            let slot = self.focused.min(self.panes.len() - 1);
            replaced = Some((slot, std::mem::take(&mut self.panes[slot])));
        }
        f(self);
        // `f` failed (e.g. no `claude` CLI): give the replaced pane back.
        if let Some((slot, old)) = replaced
            && self.panes[slot].is_empty()
        {
            self.panes[slot] = old;
        }
        self.panes.retain(|p| !p.is_empty());
        self.focused = self
            .selected
            .as_ref()
            .and_then(|s| self.panes.iter().position(|p| p == s))
            .unwrap_or(self.focused)
            .min(self.panes.len().saturating_sub(1));
    }

    /// Show `id` next to the focused pane, keeping the split.
    pub fn add_to_split(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() >= 4 && !self.panes.iter().any(|p| p == id) {
            self.toast("Up to four panes", false, cx);
            return;
        }
        let at = self.focused;
        self.with_pane_after(at, |ws| ws.show_session(id, window, cx));
    }

    /// Take `id` out of the split; the session itself keeps running and stays in the sidebar.
    pub fn remove_from_split(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.groups.iter().any(|g| g.iter().any(|p| p == id)) {
            self.leave_stored(id);
            cx.notify();
            return;
        }
        let Some(pos) = self.panes.iter().position(|p| p == id) else { return };
        if self.panes.len() < 2 {
            return;
        }
        self.panes.remove(pos);
        if pos < self.focused {
            self.focused -= 1;
        }
        self.focused = self.focused.min(self.panes.len() - 1);
        self.after_layout_change(window, cx);
    }

    /// Leave the split, keeping only the focused pane.
    pub fn exit_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() < 2 {
            return;
        }
        self.panes = vec![self.panes[self.focused.min(self.panes.len() - 1)].clone()];
        self.active_slot = usize::MAX;
        self.focused = 0;
        self.after_layout_change(window, cx);
    }

    fn after_layout_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prune_live(cx);
        self.selected = self.panes.get(self.focused).cloned();
        self.persist_layout();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The top bar's split dropdown: ways to put something next to the current pane, then ways out.
    pub fn split_menu(&self, cx: &App) -> Vec<MenuItem> {
        let item = |label: String, hint: Option<String>, act: Act, disabled: bool, sep: bool| MenuItem { label, hint, act, danger: false, disabled, separator_before: sep };
        let full = self.panes.len() >= 4;
        let cwd = self.active_cwd();
        let mut items = vec![
            item("New shell".into(), None, Act::SplitShell, full, false),
            item("New Claude session".into(), None, Act::SplitClaude(cwd.clone()), full, false),
        ];
        let project = self.store.project_for_cwd(&cwd).and_then(|id| self.store.project(&id));
        if let Some(p) = project.filter(|p| p.git.is_some()) {
            let shown = self.gh.iter().any(|g| platform::path_key(&g.cwd) == platform::path_key(&p.path) && self.panes.contains(&g.id));
            items.push(item("GitHub issues & PRs".into(), None, Act::OpenGithub { cwd: p.path.clone(), split: true }, full || shown, false));
        }
        let mut first = true;
        let others = self
            .live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
            .filter_map(|l| self.store.session(&l.id).map(|r| (r.id.clone(), r.display_title().to_string(), r.cwd.file_name().map(|n| n.to_string_lossy().into_owned()))))
            .chain(self.gh.iter().map(|g| (g.id.clone(), "GitHub".to_string(), Some(g.project.clone()))));
        for (id, title, folder) in others.filter(|(id, ..)| !self.panes.contains(id)) {
            items.push(item(title, folder, Act::AddToSplit(id), full, std::mem::take(&mut first)));
        }
        if self.panes.len() > 1 {
            let active = self.active_id().cloned().unwrap_or_default();
            items.push(item("Remove this pane from split".into(), None, Act::RemoveFromSplit(active), false, true));
            items.push(item("Exit split".into(), None, Act::ExitSplit, false, false));
        }
        items
    }

    fn on_gh_event(&mut self, pane: &str, ev: &GhEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.gh_pane(pane).map(|p| p.cwd.clone()) else { return };
        let at = self.panes.iter().position(|p| p == pane).unwrap_or(self.focused);
        match ev {
            GhEvent::Toast { text, error } => self.toast(text.clone(), *error, cx),
            // A Claude session about the item opens beside the GitHub pane so both stay in view.
            GhEvent::AskClaude(prompt) => self.with_pane_after(at, |ws| ws.new_claude_with(cwd, vec![prompt.clone()], window, cx)),
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
                self.with_pane_after(at, |ws| ws.start(record, spec, true, window, cx));
            }
        }
    }

    // ------------------------------------------------------------------ pane layout

    pub fn split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // New shell beside the current pane, in the same folder.
        if self.panes.len() >= 4 {
            self.toast("Up to four panes", false, cx);
            return;
        }
        let (cwd, at) = (self.active_cwd(), self.focused);
        self.with_pane_after(at, |ws| ws.new_shell(cwd, window, cx));
        cx.notify();
    }

    pub fn split_claude(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() >= 4 {
            self.toast("Up to four panes", false, cx);
            return;
        }
        let at = self.focused;
        self.with_pane_after(at, |ws| ws.new_claude(cwd, window, cx));
        cx.notify();
    }

    pub fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let mut running: Vec<Id> = self
            .live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
            .map(|l| l.id.clone())
            .collect();
        running.extend(self.gh.iter().map(|p| p.id.clone()));
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

    pub fn focus_pane(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.panes.len() {
            self.focused = index;
            self.selected = self.panes.get(index).cloned();
            if let Some(id) = self.selected.clone() {
                self.remember(&id);
            }
            self.focus_active(window, cx);
            cx.notify();
        }
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

    /// Context-meter source for the selected session: `(fraction, tokens, exact)`. `exact` means Claude Code
    /// itself reported the percentage (status line); otherwise it is estimated from the transcript.
    pub fn selected_context(&self) -> Option<(f32, u64, bool)> {
        let id = self.selected.as_ref().or(self.active_id())?;
        let meta = self.store.session(id)?.claude.as_ref()?;
        if let Some(f) = meta.context_fraction {
            let tokens = meta.context_tokens.unwrap_or_else(|| {
                meta.context_window
                    .map_or(0, |w| (w as f64 * f as f64) as u64)
            });
            return Some((f, tokens, true));
        }
        let tokens = meta.context_tokens?;
        Some((claude::context_fraction(tokens), tokens, false))
    }
}

/// Show session `id` given every split (shown one included, in sidebar order): its own split if it is in one,
/// else just itself. Returns the panes to show, the shown split's slot among the parked ones, and the parked ones.
fn switch_layout(mut all: Vec<Vec<Id>>, id: &Id) -> (Vec<Id>, usize, Vec<Vec<Id>>) {
    match all.iter().position(|g| g.contains(id)) {
        Some(t) => (all.remove(t), t, all),
        None => (vec![id.clone()], usize::MAX, all),
    }
}

#[cfg(test)]
mod split_tests {
    use super::switch_layout;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_session_outside_every_split_shows_alone_and_keeps_all_splits() {
        let all = vec![ids(&["a", "b"]), ids(&["c", "d"])];
        let (panes, _, parked) = switch_layout(all.clone(), &"x".to_string());
        assert_eq!(panes, ids(&["x"]));
        assert_eq!(parked, all);
    }

    #[test]
    fn a_session_in_a_split_brings_its_whole_split_and_slot_back() {
        let (panes, slot, parked) = switch_layout(vec![ids(&["a", "b"]), ids(&["c", "d"]), ids(&["e", "f"])], &"d".to_string());
        assert_eq!((panes, slot), (ids(&["c", "d"]), 1));
        assert_eq!(parked, vec![ids(&["a", "b"]), ids(&["e", "f"])]);
    }
}

#[cfg(test)]
mod headless {
    //! Drives the real workspace on gpui's test platform (no window opens) with a throwaway data folder, to catch
    //! hangs and panics when sessions start, split and switch. Spawns the real `claude` and shell.
    //! `cargo test -- --ignored headless`.
    use super::*;
    use gpui::TestAppContext;

    fn run() {
        let data = tempfile::tempdir().unwrap();
        // SAFETY: set before any other thread reads the environment; this test owns the process.
        unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
        let work = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        cx.update(|app| {
            gpui_kit::init(app);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
        });
        let (window, ws) = cx
            .update(|app| {
                gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))
            })
            .unwrap();
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        };
        let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
            let t = std::time::Instant::now();
            cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
            let acted = t.elapsed();
            draw(cx);
            draw(cx);
            // Starting a process must never run on the UI thread (a big `claude.exe` took a second to create).
            assert!(acted < Duration::from_millis(400), "the window was blocked for {acted:?}");
        };
        let cwd = work.path().to_path_buf();
        draw(&mut cx);
        step(&mut cx, &|ws, w, cx| ws.new_claude(cwd.clone(), w, cx));
        assert!(ws.read_with(&cx, |ws, _| ws.panes.len() == 1), "claude pane exists");
        step(&mut cx, &|ws, w, cx| ws.new_shell(cwd.clone(), w, cx));
        step(&mut cx, &|ws, w, cx| ws.split(w, cx));
        step(&mut cx, &|ws, w, cx| ws.split_claude(cwd.clone(), w, cx));
        assert!(ws.read_with(&cx, |ws, _| ws.panes.len() == 3), "three panes");
        // Switch to a session outside the split: the split is parked, then comes back.
        step(&mut cx, &|ws, w, cx| ws.new_shell(cwd.clone(), w, cx));
        assert!(ws.read_with(&cx, |ws, _| ws.panes.len() == 1 && ws.groups.len() == 1), "split parked");
        let member = ws.read_with(&cx, |ws, _| ws.groups[0][0].clone());
        step(&mut cx, &|ws, w, cx| ws.show_session(&member, w, cx));
        assert!(ws.read_with(&cx, |ws, _| ws.panes.len() == 3), "split restored");
        step(&mut cx, &|ws, w, cx| ws.exit_split(w, cx));
        assert!(ws.read_with(&cx, |ws, _| ws.panes.len() == 1));
        // The GitHub pane as a session type: opens once per folder, "Ask Claude" starts a session beside it.
        step(&mut cx, &|ws, w, cx| ws.open_github(cwd.clone(), false, w, cx));
        assert!(ws.read_with(&cx, |ws, _| ws.panes.len() == 1 && ws.gh.len() == 1 && ws.gh_pane(&ws.panes[0]).is_some()), "github pane shown");
        step(&mut cx, &|ws, w, cx| ws.open_github(cwd.clone(), false, w, cx));
        assert!(ws.read_with(&cx, |ws, _| ws.gh.len() == 1), "one github pane per folder");
        step(&mut cx, &|ws, _w, cx| {
            let view = ws.gh[0].view.clone();
            view.update(cx, |_, cx| cx.emit(crate::github::view::GhEvent::AskClaude("explain issue 1".into())));
        });
        assert!(ws.read_with(&cx, |ws, _| ws.panes.len() == 2), "Ask Claude opens a session beside the pane");
        step(&mut cx, &|ws, w, cx| ws.exit_split(w, cx));
    }

    #[test]
    #[ignore]
    fn starting_sessions_does_not_hang_or_panic() {
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            run();
            tx.send(()).ok();
        });
        if rx.recv_timeout(std::time::Duration::from_secs(120)).is_err() {
            // Either it froze (the bug) or the run panicked (the sender dropped).
            match t.is_finished() {
                true => t.join().unwrap(),
                false => panic!("the workspace froze while starting sessions"),
            }
        }
    }
}
