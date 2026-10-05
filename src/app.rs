//! Workspace: the app's state and all of its actions. Rendering lives in `sidebar.rs` / `views.rs`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, PathPromptOptions,
    Pixels, Point, Subscription, Window, actions,
};

use crate::claude;
use crate::mcp;
use crate::platform::{self, Editor};
use crate::statusline;
use crate::store::{
    AccountUsage, ClaudeMeta, ExternalState, Id, SessionKind, SessionRecord, Store, new_id, now,
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
    /// Click on a session row: live -> show its terminal, history -> show details.
    Select(Id),
    Resume(Id),
    CloseLive(Id),
    Rename(Id),
    Hide(Id),
    CopyText(String),
    Reveal(PathBuf),
    OpenEditor(Editor, PathBuf),
    ImportProject,
    /// Stage everything in this folder's repository, commit it with a Haiku-written message, push.
    CommitPush(PathBuf),
    Pull(PathBuf),
    SwitchBranch(PathBuf, String),
    /// Ask for a name, then `CreateBranch`.
    NewBranch(PathBuf),
    CreateBranch(PathBuf, String),
    AddProject(PathBuf),
    RemoveProject(Id),
    ToggleProject(Id),
    ToggleOther,
    ShowAll(Id),
    ConfirmQuit,
    ConfirmInstallUpdate,
    Dismiss,
    InstallUpdate,
}

pub struct MenuItem {
    pub label: String,
    pub act: Act,
    pub danger: bool,
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
        text: String,
    },
    NewBranch {
        cwd: PathBuf,
        text: String,
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
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub serial: u64,
}

pub struct Workspace {
    pub store: Store,
    pub claude_exe: Option<PathBuf>,
    pub shell: (String, Vec<String>),
    pub live: Vec<Live>,
    pub panes: Vec<Id>,
    pub focused: usize,
    pub split_vertical: bool,
    pub main: Main,
    pub selected: Option<Id>,
    pub search: String,
    pub search_focus: FocusHandle,
    pub root_focus: FocusHandle,
    pub modal: Option<Modal>,
    pub modal_focus: FocusHandle,
    pub menu: Option<Menu>,
    pub toast: Option<Toast>,
    pub toast_serial: u64,
    pub show_all: HashSet<Id>,
    pub editors: Vec<(Editor, bool)>,
    pub transcript_mtimes: HashMap<Id, SystemTime>,
    pub window_title: String,
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
        let store = Store::load(Store::default_path());
        let claude_exe = claude::discover(store.data.settings.claude_path.as_deref());
        let mut ws = Self {
            claude_exe,
            shell: platform::default_shell(),
            live: vec![],
            panes: vec![],
            focused: 0,
            split_vertical: false,
            main: Main::Home,
            selected: store.data.layout.selected_session.clone(),
            search: String::new(),
            search_focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            modal: None,
            modal_focus: cx.focus_handle(),
            menu: None,
            toast: None,
            toast_serial: 0,
            show_all: HashSet::new(),
            editors: Editor::ALL
                .iter()
                .map(|e| (*e, e.locate().is_some()))
                .collect(),
            transcript_mtimes: HashMap::new(),
            window_title: String::new(),
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
        ws.start_scan(cx);
        statusline::cleanup();
        ws.ingest_statusline(cx);
        ws.probe_usage_on_launch(cx);
        ws.start_ticker(cx);
        ws.start_activity_poll(window, cx);
        ws.check_updates(cx);
        window.focus(&ws.root_focus);
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
                ws.merge_account(result.five_hour, result.seven_day);
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
        five: Option<crate::store::RateLimit>,
        seven: Option<crate::store::RateLimit>,
    ) -> bool {
        let acct: &mut AccountUsage = &mut self.store.data.account;
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
                if this.update_in(cx, |ws, window, cx| ws.update_activity(window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn update_activity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            match (view.is_working(), self.activity.get(&l.id)) {
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
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        self.purge_dead_shells();
        self.store.save_if_dirty();
        self.refresh_live(cx);
        self.ingest_statusline(cx);
        cx.notify();
    }

    /// Pick up what Claude Code's status line (via our per-process tee) wrote: account limits and exact context,
    /// plus the tab titles Claude chose through the MCP tool.
    /// Costs no tokens; the files are tiny, but the read still happens off the UI thread.
    fn ingest_statusline(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (snaps, titles) = cx
                .background_executor()
                .spawn(async { (statusline::read_snapshots(&statusline::snapshot_dir()), mcp::read_titles()) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                let mut changed = false;
                for (stem, title) in titles {
                    if let Some(rec) = ws
                        .store
                        .data
                        .sessions
                        .iter_mut()
                        .find(|s| s.claude_id().is_some_and(|id| statusline::safe_file_stem(id) == stem))
                        && rec.auto_title.as_deref() != Some(title.as_str())
                    {
                        rec.auto_title = Some(title);
                        changed = true;
                    }
                }
                for snap in snaps {
                    changed |= ws.merge_account(snap.five_hour.clone(), snap.seven_day.clone());
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

    /// Once per launch, fill the account meters without waiting for a first message: a single
    /// `claude -p "/usage"` (see usage.rs). Skipped when we already hold a reading from the last few minutes
    /// (e.g. the status-line tee delivered one).
    fn probe_usage_on_launch(&mut self, cx: &mut Context<Self>) {
        let Some(exe) = self.claude_exe.clone() else {
            return;
        };
        let acct = &self.store.data.account;
        let newest = [acct.five_hour.as_ref(), acct.seven_day.as_ref()]
            .into_iter()
            .flatten()
            .map(|l| l.seen_at)
            .max();
        if newest.is_some_and(|t| now() - t < 300) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::usage::fetch(&exe, jiff::Timestamp::now()) })
                .await;
            let _ = this.update(cx, |ws, cx| {
                if let Ok(reading) = result {
                    ws.merge_account(reading.five_hour, reading.seven_day);
                    ws.store.save_if_dirty();
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
        spec.env.push(("CLAUDE_CODE_AUTO_CONNECT_IDE".into(), "false".into()));
        spec.env.push(("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL".into(), "1".into()));
        spec
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
                    ws.merge_account(
                        scan.limits
                            .iter()
                            .filter(|o| o.kind == "five_hour")
                            .map(|o| o.limit.clone())
                            .next_back(),
                        scan.limits
                            .iter()
                            .filter(|o| o.kind == "seven_day")
                            .map(|o| o.limit.clone())
                            .next_back(),
                    );
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

    pub fn active_cwd(&self) -> PathBuf {
        self.active_id()
            .and_then(|id| self.store.session(id))
            .map(|s| s.cwd.clone())
            .or_else(|| self.store.data.projects.first().map(|p| p.path.clone()))
            .or_else(platform::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    // ------------------------------------------------------------------ session lifecycle

    pub fn toast(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.toast_serial += 1;
        let serial = self.toast_serial;
        self.toast = Some(Toast {
            text: text.into(),
            error,
            serial,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
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
        let Some(exe) = self.claude_exe.clone() else {
            self.toast("Could not find the `claude` CLI. Install Claude Code, or set its path in state.json (settings.claude_path).", true, cx);
            return;
        };
        let session_id = new_id();
        let spec = claude::new_session_spec(&exe, &cwd, &session_id);
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
        let spec = claude::resume_spec(&exe, &rec.cwd, &meta.session_id);
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
        let (terminal, rx) = match TerminalView::spawn(&spec) {
            Ok(t) => t,
            Err(e) => {
                self.toast(format!("Could not start {}: {e}", spec.program), true, cx);
                return;
            }
        };
        let id = record.id.clone();
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
        let view = cx.new(|cx| TerminalView::new(terminal, rx, cx));
        let sub_id = id.clone();
        let sub = cx.subscribe(
            &view,
            move |ws: &mut Workspace, _view, ev: &TerminalEvent, cx| match ev {
                TerminalEvent::Exited(code) => ws.on_exit(&sub_id, *code, cx),
                TerminalEvent::Bell => {}
            },
        );
        self.live.push(Live {
            id: id.clone(),
            view,
            _sub: sub,
        });
        self.place_in_pane(&id, cx);
        self.selected = Some(id.clone());
        self.main = Main::Terminals;
        self.persist_layout();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn place_in_pane(&mut self, id: &Id, cx: &App) {
        if let Some(pos) = self.panes.iter().position(|p| p == id) {
            self.focused = pos;
        } else if self.panes.is_empty() {
            self.panes.push(id.clone());
            self.focused = 0;
        } else {
            let slot = self.focused.min(self.panes.len() - 1);
            self.panes[slot] = id.clone();
            self.focused = slot;
        }
        // Exited terminals are only kept while a pane still shows them.
        let panes = &self.panes;
        self.live
            .retain(|l| panes.contains(&l.id) || l.view.read(cx).is_running());
    }

    /// A Claude session that never produced a transcript (opened and closed without a prompt) has nothing
    /// to resume. Remove Claudiu's own record of it; Claude Code's data is never touched.
    fn drop_if_empty(&mut self, id: &str) {
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

    fn on_exit(&mut self, id: &str, code: Option<i32>, cx: &mut Context<Self>) {
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
            self.place_in_pane(&id, cx);
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
        if let (Main::Terminals, Some(id)) = (&self.main, self.panes.get(self.focused))
            && let Some(view) = self.live_view(id) {
                window.focus(&view.read(cx).focus_handle(cx));
                return;
            }
        window.focus(&self.root_focus);
    }

    fn close_live(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.live.retain(|l| l.id != id); // dropping the view drops the Terminal, which closes the PTY
        self.panes.retain(|p| p != id);
        self.focused = self.focused.min(self.panes.len().saturating_sub(1));
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

    /// Called by the window's close handler. Returns true when it is fine to close right now.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.quitting {
            return true;
        }
        let running = self.running_count(cx);
        if running == 0 {
            self.finalize();
            return true;
        }
        self.modal = Some(Modal::Confirm {
            title: "Quit Claudiu?".into(),
            body: format!(
                "{running} session{} still running. Quitting ends {} — Claude sessions stay in your history and can be resumed.",
                if running == 1 { " is" } else { "s are" },
                if running == 1 { "it" } else { "them" }
            ),
            confirm: "Quit and end sessions".into(),
            danger: true,
            act: Act::ConfirmQuit,
        });
        window.focus(&self.modal_focus);
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
        // Terminal sessions are never persisted.
        self.store
            .data
            .sessions
            .retain(|s| s.kind != SessionKind::Shell);
        self.store.save();
    }

    // ------------------------------------------------------------------ actions

    pub fn act(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match act {
            Act::NewClaude { cwd } => self.new_claude(cwd, window, cx),
            Act::NewShell { cwd } => self.new_shell(cwd, window, cx),
            Act::Select(id) => self.show_session(&id, window, cx),
            Act::Resume(id) => self.resume(&id, window, cx),
            Act::CloseLive(id) => self.close_live(&id, window, cx),
            Act::Rename(id) => {
                let text = self
                    .store
                    .session(&id)
                    .map(|s| s.display_title().to_string())
                    .unwrap_or_default();
                self.modal = Some(Modal::Rename { id, text });
                window.focus(&self.modal_focus);
            }
            Act::Hide(id) => {
                if let Some(rec) = self.store.session_mut(&id) {
                    rec.hidden = true;
                }
                let still_running = self.is_running(&id, cx);
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = None;
                }
                self.toast(
                    if still_running {
                        "Hidden from the list. It is still running; end it from Current Session."
                    } else {
                        "Removed from Claudiu's list. The Claude transcript was not touched."
                    },
                    false,
                    cx,
                );
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
            Act::CommitPush(cwd) => match self.claude_exe.clone() {
                Some(exe) => self.run_git(GitOp::CommitPush, move || crate::commit::commit_and_push(&exe, &cwd), cx),
                None => self.toast("Could not find the `claude` CLI.", true, cx),
            },
            Act::Pull(cwd) => self.run_git(GitOp::Pull, move || crate::commit::pull(&cwd), cx),
            Act::SwitchBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || crate::commit::switch_branch(&cwd, &name), cx)
            }
            Act::NewBranch(cwd) => {
                self.modal = Some(Modal::NewBranch { cwd, text: String::new() });
                window.focus(&self.modal_focus);
            }
            Act::CreateBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || crate::commit::create_branch(&cwd, &name), cx)
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
                    window.focus(&self.modal_focus);
                } else {
                    crate::updater::install(self, window, cx);
                }
            }
            Act::ConfirmInstallUpdate => {
                self.modal = None;
                crate::updater::install(self, window, cx);
            }
        }
        self.store.save_if_dirty();
        cx.notify();
    }

    /// Git actions (see commit.rs): one at a time, off the UI thread, reported through a toast.
    fn run_git(
        &mut self,
        op: GitOp,
        job: impl FnOnce() -> Result<String, String> + Send + 'static,
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
                    Err(e) => ws.toast(e, true, cx),
                }
            });
        })
        .detach();
    }

    /// Branch menu: fetch origin and list branches off the UI thread, then open the menu at `pos`.
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
                                act: Act::SwitchBranch(cwd.clone(), name),
                                danger: false,
                                separator_before: false,
                            })
                            .collect();
                        let first = items.is_empty();
                        items.push(MenuItem { label: "New branch…".into(), act: Act::NewBranch(cwd), danger: false, separator_before: !first });
                        ws.open_menu(pos, items, cx);
                    }
                    Err(e) => ws.toast(e, true, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Enter / OK in a modal.
    pub fn submit_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let act = match self.modal.take() {
            Some(Modal::Confirm { act, .. }) => act,
            Some(Modal::Rename { id, text }) => {
                let text = text.trim().to_string();
                if let Some(rec) = self.store.session_mut(&id) {
                    rec.custom_title = (!text.is_empty()).then_some(text);
                }
                Act::Dismiss
            }
            Some(Modal::NewBranch { cwd, text }) => {
                // Spaces aren't allowed in branch names; git reports anything else that is invalid.
                let name = text.split_whitespace().collect::<Vec<_>>().join("-");
                if name.is_empty() { Act::Dismiss } else { Act::CreateBranch(cwd, name) }
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

    // ------------------------------------------------------------------ pane layout

    pub fn split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // New shell beside the current pane, in the same folder.
        if self.panes.len() >= 4 {
            self.toast("Up to four panes", false, cx);
            return;
        }
        let cwd = self.active_cwd();
        let at = (self.focused + 1).min(self.panes.len());
        // Empty placeholder slot; `start` -> `place_in_pane` fills it with the new session.
        self.panes.insert(at, String::new());
        self.focused = at;
        self.new_shell(cwd, window, cx);
        self.panes.retain(|p| !p.is_empty());
        self.focused = self.focused.min(self.panes.len().saturating_sub(1));
        cx.notify();
    }

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

    pub fn focus_pane(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.panes.len() {
            self.focused = index;
            self.selected = self.panes.get(index).cloned();
            self.focus_active(window, cx);
            cx.notify();
        }
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
