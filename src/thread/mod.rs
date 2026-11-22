//! A harness session drawn as a conversation instead of a terminal: it starts the harness's ACP server, opens or
//! creates the session, and sends what the user types. Every harness looks and behaves the same here, keyboard
//! included (`keys`); what differs between them is in `harness`.

mod keys;
mod render;
mod review;

pub use render::short_effort_label;

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use futures::StreamExt;
use gpui::{App, AppContext, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, FollowMode, KeystrokeEvent, ListAlignment, ListState, Pixels, Subscription, Window, px};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use serde_json::{Value, json};

use crate::harness::acp::{self, Connection, Event, Launch};
use crate::harness::thread::{Entry, Thread};
use crate::harness::{CatalogModel, Harness, Reopen};
use crate::visual::Card;
use crate::visual::ui::{CardUi, SendText};

/// Which compact composer popover is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadPicker {
    /// Unified aggregated model list (across all enabled harnesses) + draggable effort bar.
    Model,
    /// Permission mode selector dialog.
    Permission,
}

/// A session being moved while its harness restarts: what the waiting screen says.
pub struct Handover {
    pub message: String,
}

/// What the message box's branch chip shows; the workspace keeps it current (`Workspace::sync_git_chips`).
#[derive(Clone, Debug, PartialEq)]
pub struct GitChip {
    /// The branch checked out, or the worktree's name without one.
    pub primary: String,
    /// The linked worktree's name, outside the main checkout.
    pub linked: Option<String>,
    /// A branch or worktree change is running.
    pub busy: bool,
    /// The Branch & Worktree dialog is open for this session.
    pub open: bool,
}

pub enum ThreadEvent {
    Ready(String),
    /// The branch chip was clicked: open the Branch & Worktree dialog there.
    OpenGitPicker(crate::panels::widgets::Dropdown),
    /// The harness stopped; the session's view closes.
    Exited(Option<i32>),
    /// The agent named the session: a new one learns its id this way.
    SessionId(String),
    Title(String),
    /// The harness reported its live model catalog `(id, name)`.
    Models(Harness, Vec<(String, String)>),
    /// The harness reported its available permission modes `(id, name)`.
    Modes(Harness, Vec<(String, String)>),
    /// The harness reported live context usage `(used_tokens, max_tokens)`.
    Context(u64, u64),
    /// A file a tool acted on was clicked: its path and line (1-based).
    OpenPath(String, Option<usize>),
    /// The user picked a model served by another harness and sent a message: dynamically hand off this chat.
    /// `by_agent` when the agent asked for it (set_model): its `message` is then the agent's, not the user's.
    Handoff { to: Harness, model: Option<String>, message: String, effort: Option<String>, by_agent: bool },
    /// The session's model or effort changed: `(model, effort)` option values, kept for when it resumes.
    Chosen(Option<String>, Option<String>),
    /// A turn ended with nothing queued after it: `completed`, `interrupted` or `failed`. A subagent's report goes
    /// to its parent then (`app/subagents.rs`).
    Idle(&'static str),
    /// A subagent's report card was clicked: show that session.
    Select(String),
}

/// How to bring a session up.
pub struct Start {
    /// m4code's session record id, used to persist conversation entries across restarts.
    pub record_id: String,
    pub harness: Harness,
    /// Aggregated models across every enabled harness for the unified model picker.
    pub all_models: Vec<CatalogModel>,
    /// The models of `harness` ticked in Settings → Harnesses (`store::keep_picked`).
    pub picked: Vec<String>,
    /// The ACP server to start in place of the harness's own.
    pub server: Option<(PathBuf, Vec<String>)>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// The session's id when it exists already; None makes a new one.
    pub session_id: Option<String>,
    /// `_meta` for `session/new` and `session/load`.
    pub meta: Value,
    pub mcp_servers: Vec<Value>,
    /// Applied once a new session is open: the permission mode, and config options by category (`model`,
    /// `thought_level`) with the value wanted.
    pub mode: Option<String>,
    pub options: Vec<(&'static str, String)>,
    /// Sent as soon as the session is ready.
    pub prompt: Option<String>,
    /// Entries carried over visually from a previous harness during an in-chat handoff.
    pub prior_entries: Vec<Entry>,
}

/// One selectable option in a permission or AskUserQuestion prompt.
struct QuestionOption {
    id: String,
    name: String,
    description: Option<String>,
    kind: String,
}

/// A permission or multiple-choice question the agent is waiting for.
struct Question {
    id: Value,
    title: String,
    options: Vec<QuestionOption>,
}

struct Queued {
    text: String,
    compact: bool,
    /// Already appended to `thread.entries` so a new session shows the user's message while the agent starts.
    shown: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Starting,
    Ready,
    Ended,
}

pub struct ThreadView {
    record_id: String,
    pub harness: Harness,
    /// Aggregated models from every enabled harness, shown together in one picker.
    pub all_models: Vec<CatalogModel>,
    /// The models of this session's harness ticked in Settings → Harnesses; they narrow the live model list too.
    pub picked: Vec<String>,
    /// The branch chip, when the session's folder is in a git repository.
    pub git: Option<GitChip>,
    /// Model from another harness staged for dynamic handoff on the next message.
    pub pending_model: Option<CatalogModel>,
    /// A hand-off the agent asked for with `set_model` and the user approved: the model, its effort and the first
    /// message for the new agent. It runs as soon as the turn that asked ends.
    handoff_after_turn: Option<(CatalogModel, Option<String>, String, bool)>,
    /// Effort choice staged for the next handoff or while no live `thought_level` option exists yet.
    pub pending_effort: Option<String>,
    /// The `(model, effort)` last sent as `ThreadEvent::Chosen`.
    chosen: (Option<String>, Option<String>),
    model_search: Entity<InputState>,
    cwd: PathBuf,
    connection: Option<Rc<Connection>>,
    session_id: Option<String>,
    pub thread: Thread,
    phase: Phase,
    /// A prompt is out and the turn hasn't ended.
    working: bool,
    /// Messages typed during a turn, sent one at a time after it.
    queue: VecDeque<Queued>,
    question: Option<Question>,
    pub composer: Entity<TextareaState>,
    /// Where Up has got to among the messages sent before, and what was typed before it started.
    history_at: Option<usize>,
    draft: String,
    list: ListState,
    /// How many rows the list has measured.
    rows: usize,
    /// Ctrl+O: every tool's output in full.
    pub detail: bool,
    pub shortcuts_shown: bool,
    pub(crate) subagent_edits: Option<bool>,
    pub compaction: crate::harness::compaction::Policy,
    pub(crate) canonical_history: bool,
    persist_on_drop: bool,
    pub capabilities: Value,
    /// Ctrl+T: the plan.
    pub plan_open: bool,
    /// Active compact picker dropdown above the composer (Alt+P for Model/Provider/Effort, or click Permission).
    pub picker: Option<ThreadPicker>,
    picker_at: crate::panels::widgets::Dropdown,
    model_anchor: crate::panels::widgets::Anchor,
    /// Highlighted row index inside the Model picker for Up/Down + Enter keyboard navigation.
    model_highlight: usize,
    mode_highlight: usize,
    /// Follows `model_highlight` so keyboard moves keep the highlighted row in view.
    pub(super) model_scroll: gpui::ScrollHandle,
    /// Tool call ids manually expanded by clicking their compressed single-line header.
    expanded_tools: HashSet<String>,
    effort_bounds: Rc<Cell<Bounds<Pixels>>>,
    effort_dragging: Rc<Cell<bool>>,
    last_escape: Option<Instant>,
    /// Why the harness stopped, when it said.
    error: Option<String>,
    /// Shown instead of the conversation while the session moves (a hand-off).
    pub handover: Option<Handover>,
    /// True while showing locally cached entries for a `Reopen::Load` harness before its live replay arrives.
    cached_replay: bool,
    /// This view took over a conversation from another harness (an in-place hand-off), and shows it while it starts.
    took_over: bool,
    cards: HashMap<String, Card>,
    card_ui: Entity<CardUi>,
    /// The working tree as it was before each message, oldest first (`git/snapshot.rs`). Empty outside a git
    /// repository.
    pub snapshots: Vec<String>,
    /// The slash menu's highlighted row, and its scroll.
    command_highlight: usize,
    command_scroll: gpui::ScrollHandle,
    /// Whether the working tree differs from the last snapshot; None while a turn runs or the check is out.
    pub(super) changed: Option<bool>,
    /// The review of what changed since the last message, while it is open.
    review: Option<review::Review>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ThreadEvent> for ThreadView {}

impl Focusable for ThreadView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl ThreadView {
    pub fn new(start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 12).submit_on_enter(true).placeholder("Ask anything…"));
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter models…"));
        let card_ui = cx.new(|_| CardUi::default());
        let this = cx.entity().downgrade();
        let mut subscriptions = vec![
            cx.subscribe_in(&composer, window, |v: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => v.submit(window, cx),
                InputEvent::Change => {
                    v.command_highlight = 0;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe_in(&model_search, window, |_: &mut Self, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&card_ui, window, |v: &mut Self, ui, ev: &SendText, window, cx| {
                ui.update(cx, |ui, cx| ui.mark_sent(&ev.key, cx));
                v.send(ev.text.clone(), window, cx);
            }),
        ];
        // Before the composer's own bindings, which would take Escape, Up, Down and Shift+Tab.
        subscriptions.push(cx.intercept_keystrokes(move |ev: &KeystrokeEvent, window, cx| {
            let _ = this.update(cx, |v, cx| v.intercept(ev, window, cx));
        }));
        let mut thread = Thread { entries: start.prior_entries.clone(), ..Default::default() };
        let mut queue = VecDeque::new();
        if let Some(prompt) = start.prompt.clone() {
            // Show a new session's first message immediately instead of a blank screen while the agent starts.
            // A resumed session replays history during startup, so keep its prompt queued until open finishes.
            let shown = start.session_id.is_none();
            if shown {
                thread.sent(prompt.clone());
            }
            queue.push_back(Queued { text: prompt, shown, compact: false });
        }
        let rows = thread.entries.len();
        let list = ListState::new(rows, ListAlignment::Top, px(600.0));
        list.set_follow_mode(FollowMode::Tail);
        let cached_replay = start.session_id.is_some()
            && !start.prior_entries.is_empty()
            && start.harness.adapter().reopen == Reopen::Load;
        let took_over = start.session_id.is_none() && !start.prior_entries.is_empty();
        let mut view = ThreadView {
            record_id: start.record_id.clone(),
            harness: start.harness,
            all_models: start.all_models.clone(),
            picked: start.picked.clone(),
            git: None,
            pending_model: None,
            handoff_after_turn: None,
            pending_effort: start.options.iter().find(|(k, _)| *k == "thought_level").map(|(_, v)| v.clone()),
            chosen: (None, None),
            model_search,
            cwd: start.cwd.clone(),
            connection: None,
            session_id: start.session_id.clone(),
            thread,
            phase: Phase::Starting,
            working: false,
            queue,
            question: None,
            composer,
            history_at: None,
            draft: String::new(),
            list,
            rows,
            detail: false,
            shortcuts_shown: false,
            subagent_edits: None,
            compaction: Default::default(),
            canonical_history: false,
            persist_on_drop: true,
            capabilities: Value::Null,
            plan_open: false,
            picker: None,
            picker_at: crate::panels::widgets::Dropdown::at(gpui::point(px(0.0), px(0.0))),
            model_anchor: crate::panels::widgets::Anchor::default(),
            model_highlight: 0,
            mode_highlight: 0,
            model_scroll: gpui::ScrollHandle::new(),
            expanded_tools: HashSet::new(),
            effort_bounds: Rc::new(Cell::new(Bounds::default())),
            effort_dragging: Rc::new(Cell::new(false)),
            last_escape: None,
            error: None,
            handover: None,
            cached_replay,
            took_over,
            cards: HashMap::new(),
            card_ui,
            snapshots: Vec::new(),
            command_highlight: 0,
            command_scroll: gpui::ScrollHandle::new(),
            changed: None,
            review: None,
            _subscriptions: subscriptions,
        };
        view.start(start, cx);
        view
    }

    fn start(&mut self, start: Start, cx: &mut Context<Self>) {
        let (events, mut incoming) = futures::channel::mpsc::unbounded();
        let harness = start.harness;
        cx.spawn(async move |this, cx| {
            // Finding the server may read the login shell's PATH, so it is off the UI thread.
            let cwd = start.cwd.clone();
            let env = start.env.clone();
            let server = start.server.clone();
            let started = cx
                .background_executor()
                .spawn(async move {
                    let (program, args) = match server {
                        Some(s) => s,
                        None => (harness.adapter().server)()?,
                    };
                    Connection::start(&Launch { program, args, cwd, env }, events)
                })
                .await;
            let connection = match started {
                Ok(c) => Rc::new(c),
                Err(why) => {
                    let _ = this.update(cx, |v, cx| v.ended(Some(why), cx));
                    return;
                }
            };
            if this.update(cx, |v, _| v.connection = Some(connection.clone())).is_err() {
                return;
            }
            let opened = open(&connection, harness, &start).await;
            let ok = this.update(cx, |v, cx| v.opened(opened, &start, cx)).unwrap_or(false);
            if !ok {
                return;
            }
            while let Ok(event) = incoming.try_recv() {
                if this.update(cx, |v, cx| v.on_event(event, cx)).is_err() {
                    return;
                }
            }
            let ready = this.update(cx, |v, cx| {
                if v.phase != Phase::Starting { return false; }
                v.phase = Phase::Ready;
                v.cached_replay = false;
                cx.emit(ThreadEvent::Ready(v.session_id.clone().unwrap_or_default()));
                v.send_next(cx);
                cx.notify();
                true
            }).unwrap_or(false);
            if !ready { return; }
            drop(connection);
            while let Some(event) = incoming.next().await {
                if this.update(cx, |v, cx| v.on_event(event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// The session is open, or failed to open. Returns whether to go on listening.
    fn opened(&mut self, opened: Result<Opened, String>, start: &Start, cx: &mut Context<Self>) -> bool {
        let Opened { session_id, result, capabilities, replayed } = match opened {
            Ok(o) => o,
            Err(why) => {
                self.ended(Some(why), cx);
                return false;
            }
        };
        self.capabilities = capabilities;
        self.cached_replay = replayed && !start.prior_entries.is_empty();
        self.thread.apply_session(&result);
        if let Some(model_opt) = self.thread.option("model")
            && !model_opt.choices.is_empty()
        {
            let real: Vec<(String, String)> = model_opt.choices.iter().filter(|(id, _)| id != "default").cloned().collect();
            let choices = if real.is_empty() { model_opt.choices.clone() } else { real };
            cx.emit(ThreadEvent::Models(self.harness, choices));
        }
        if !self.thread.modes.is_empty() {
            cx.emit(ThreadEvent::Modes(self.harness, self.thread.modes.clone()));
        }
        if self.session_id.as_deref() != Some(&session_id) {
            self.session_id = Some(session_id.clone());
            cx.emit(ThreadEvent::SessionId(session_id));
        }
        // A resumed session keeps the mode the harness restores; its options are the model and effort picked in it.
        if (start.session_id.is_none() || self.subagent_edits.is_some())
            && let Some(mode) = start.mode.clone().filter(|m| self.thread.modes.iter().any(|(id, _)| id == m))
        {
            self.set_mode(mode, cx);
        }
        for (category, value) in &start.options {
            if let Some(option) = self.thread.option(category).filter(|o| o.current != *value && o.choices.iter().any(|(v, _)| v == value)) {
                let id = option.id.clone();
                self.set_option(id, value.clone(), cx);
            }
        }
        self.report_chosen(cx);
        // A move (a worktree switch, a hand-off) is over once the session is open where it went.
        self.handover = None;
        self.remeasure(0);
        cx.notify();
        true
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Update(update) => {
                if self.cached_replay && !self.working && is_conversation_update(&update) {
                    if self.canonical_history { return; }
                    self.cached_replay = false;
                    self.thread.entries.clear();
                    self.remeasure(0);
                }
                let title = self.thread.title.clone();
                let prev_ctx = self.thread.context;
                if let Some(at) = self.thread.apply(&update) {
                    self.remeasure(at);
                }
                if self.thread.title != title
                    && let Some(t) = self.thread.title.clone()
                {
                    cx.emit(ThreadEvent::Title(t));
                }
                if self.thread.context != prev_ctx
                    && let Some((used, size)) = self.thread.context
                {
                    self.compaction.observe(used, size);
                    cx.emit(ThreadEvent::Context(used, size));
                }
                self.report_chosen(cx);
            }
            Event::Permission { id, params } => {
                if let Some(edits) = self.subagent_edits && acp::is_tool_permission(&params) {
                    if let Some(connection) = &self.connection {
                        connection.respond(id, json!({"outcome": crate::claude::acp::permission(&params["options"], edits)}));
                    }
                    return;
                }
                let options = params["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| QuestionOption {
                        id: str_of(o, "optionId"),
                        name: str_of(o, "name"),
                        description: o["description"].as_str().filter(|s| !s.trim().is_empty()).map(String::from),
                        kind: str_of(o, "kind"),
                    })
                    .collect();
                let title = params.pointer("/toolCall/title").and_then(Value::as_str).unwrap_or("The agent asks for permission").to_string();
                self.question = Some(Question { id, title, options });
            }
            Event::Exited(words) => {
                let words = (!words.is_empty()).then_some(words);
                self.ended(words, cx);
                return;
            }
        }
        self.send_next(cx);
        cx.notify();
    }

    /// The harness stopped. Asked to or not, the view closes; `why` is shown as the failure.
    fn ended(&mut self, why: Option<String>, cx: &mut Context<Self>) {
        if self.phase == Phase::Ended {
            return;
        }
        self.phase = Phase::Ended;
        self.handover = None;
        self.error = why;
        self.connection = None;
        cx.emit(ThreadEvent::Exited(if self.error.is_some() { Some(1) } else { Some(0) }));
        cx.notify();
    }

    /// Pick a model from the aggregated catalog: switches live on the same harness, or stages a dynamic in-chat
    /// handoff when the model belongs to another harness.
    pub fn pick_model(&mut self, model: CatalogModel, cx: &mut Context<Self>) {
        if model.harness == self.harness {
            self.pending_model = None;
            if let Some(opt) = self.thread.option("model")
                && !model.id.is_empty()
            {
                let id = opt.id.clone();
                self.set_option(id, model.id, cx);
            }
        } else {
            self.pending_model = Some(model);
        }
        self.picker = None;
        cx.notify();
    }

    /// Enter in the composer.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if !crate::panels::composer::can_send(&text) {
            return;
        }
        self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        self.history_at = None;
        self.picker = None;
        self.send(text, window, cx);
    }

    /// Send `text` as the user's next message, now or after the turn that is running.
    pub fn send(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.pending_model.take().filter(|m| m.harness != self.harness) {
            let effort = self.pending_effort.take();
            if self.working {
                let previous = self.handoff_after_turn.take().filter(|(_, _, _, by_agent)| !*by_agent).map(|(_, _, text, _)| text);
                let text = previous.map_or_else(|| text.clone(), |previous| format!("{previous}\n\n{text}"));
                self.handoff_after_turn = Some((target, effort, text, false));
                cx.notify();
                return;
            }
            let model = (!target.id.is_empty()).then_some(target.id);
            self.picker = None;
            cx.emit(ThreadEvent::Handoff { to: target.harness, model, message: text, effort, by_agent: false });
            cx.notify();
            return;
        }
        if self.working && let Some((_, _, message, by_agent)) = self.handoff_after_turn.as_mut()
            && !*by_agent
        {
            message.push_str("\n\n");
            message.push_str(&text);
            cx.notify();
            return;
        }
        let shown = self.phase == Phase::Starting && self.session_id.is_none();
        if shown && let Some(at) = self.thread.sent(text.clone()) {
            self.remeasure(at);
        }
        self.queue.push_back(Queued { text, shown, compact: false });
        self.send_next(cx);
        cx.notify();
    }

    fn send_next(&mut self, cx: &mut Context<Self>) {
        if self.working || self.phase != Phase::Ready {
            return;
        }
        let (Some(connection), Some(session)) = (self.connection.clone(), self.session_id.clone()) else { return };
        let command = self.harness.adapter().compact_command.filter(|name| self.thread.commands.iter().any(|c| c.name == *name));
        let next = if self.compaction.take_due(self.thread.context, command.is_some()) {
            let at = self.thread.notice("Compacting context…", false);
            self.remeasure(at);
            Queued { text: format!("/{}", command.unwrap()), shown: true, compact: true }
        } else {
            let Some(next) = self.queue.pop_front() else { return };
            next
        };
        if !next.shown && !next.compact
            && let Some(at) = self.thread.sent(next.text.clone())
        {
            self.remeasure(at);
        }
        self.cached_replay = false;
        let text = next.text;
        let compact = next.compact;
        self.persist_entries();
        self.list.scroll_to_end();
        self.working = true;
        self.changed = None;
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            // The snapshot comes first, so the review sees what this turn changed. Outside a repository it fails, and the
            // message goes anyway.
            let snapshot = if !compact {
                let session = session.clone();
                Some(cx.background_executor().spawn(async move { crate::git::snapshot::take(&cwd, &session) }).await)
            } else {
                None
            };
            if let Some(Ok(id)) = snapshot {
                let _ = this.update(cx, |v, _| v.snapshots.push(id));
            }
            let turn = connection.request("session/prompt", json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}));
            let result = turn.await;
            let _ = this.update(cx, |v, cx| v.turn_ended(result, cx));
        })
        .detach();
    }

    fn turn_ended(&mut self, result: Result<Value, String>, cx: &mut Context<Self>) {
        self.working = false;
        let compacted = self.compaction.running;
        if std::mem::take(&mut self.compaction.running) {
            let completed = result.as_ref().is_ok_and(|r| r["stopReason"] == "end_turn");
            let at = self.thread.notice(if completed { "Context compacted." } else { "Context compaction did not finish." }, !completed);
            self.remeasure(at);
            if let Some((used, size)) = self.thread.context {
                self.compaction.observe(used, size);
            }
        } else {
            self.check_changes(cx);
        }
        self.question = None;
        let notice = match &result {
            Ok(r) => match r["stopReason"].as_str() {
                Some("cancelled") => Some(("Interrupted".to_string(), false)),
                Some("refusal") => Some(("The model declined to go on.".to_string(), true)),
                Some("max_tokens") => Some(("The reply reached its length limit.".to_string(), true)),
                Some("max_turn_requests") => Some(("The turn reached its step limit.".to_string(), true)),
                _ => None,
            },
            Err(why) if self.phase == Phase::Ready => Some((why.clone(), true)),
            Err(_) => None,
        };
        let outcome = match &result {
            Ok(r) if r["stopReason"] == "cancelled" => "interrupted",
            Ok(_) if notice.is_none() => "completed",
            _ => "failed",
        };
        if let Some((text, error)) = notice {
            let at = self.thread.notice(text, error);
            self.remeasure(at);
        }
        self.persist_entries();
        if let Some((target, effort, mut message, mut by_agent)) = self.handoff_after_turn.take() {
            let queued = self.queue.drain(..).map(|q| q.text).collect::<Vec<_>>().join("\n\n");
            if !queued.is_empty() {
                if by_agent {
                    self.thread.entries.push(Entry::Agent(message));
                    message = queued;
                    by_agent = false;
                } else {
                    message = format!("{message}\n\n{queued}");
                }
            }
            let model = (!target.id.is_empty()).then_some(target.id);
            cx.emit(ThreadEvent::Handoff { to: target.harness, model, message, effort, by_agent });
            cx.notify();
            return;
        }
        self.send_next(cx);
        if !self.working && !compacted {
            cx.emit(ThreadEvent::Idle(outcome));
        }
        cx.notify();
    }

    /// The model this session runs now, as the picker lists it.
    pub fn current_model(&self) -> Option<CatalogModel> {
        let id = self.thread.option("model")?.current.clone();
        self.unified_models().into_iter().find(|m| m.harness == self.harness && m.id == id)
    }

    /// `set_model` on this session's own harness: switch the model and the effort from the next request on.
    pub fn switch_model(&mut self, model: Option<&CatalogModel>, effort: Option<&str>, cx: &mut Context<Self>) -> Result<String, String> {
        let mut done = vec![];
        if let Some(model) = model {
            let option = self.thread.option("model").map(|o| o.id.clone()).ok_or("This session's agent doesn't let m4code change its model.")?;
            self.set_option(option, model.id.clone(), cx);
            done.push(model.name.clone());
        }
        if let Some(effort) = effort {
            let (values, _, _, option) = self.effort_choices();
            let value = values.iter().find(|v| v.eq_ignore_ascii_case(effort)).cloned().ok_or_else(|| format!("No effort “{effort}”. Use one of: {}.", values.join(", ")))?;
            let option = option.ok_or("This session's agent doesn't let m4code change its effort.")?;
            self.pending_effort = Some(value.clone());
            self.set_option(option, value.clone(), cx);
            done.push(format!("effort {value}"));
        }
        cx.notify();
        Ok(done.join(", "))
    }

    /// `set_model` onto another harness, once the user approved it: hand the conversation over when this turn ends.
    pub fn hand_off_after_turn(&mut self, model: CatalogModel, effort: Option<String>, message: String, cx: &mut Context<Self>) {
        if self.working {
            self.handoff_after_turn = Some((model, effort, message, true));
        } else {
            let to = model.harness;
            let model = (!model.id.is_empty()).then_some(model.id);
            cx.emit(ThreadEvent::Handoff { to, model, message, effort, by_agent: true });
        }
        cx.notify();
    }

    fn interrupt(&mut self, cx: &mut Context<Self>) {
        self.queue.clear();
        if let (Some(c), Some(s)) = (&self.connection, &self.session_id) {
            c.notify("session/cancel", json!({"sessionId": s}));
        }
        // ACP says a client that cancels answers every open permission question as cancelled.
        self.answer(None, cx);
    }

    /// Answer the open permission question with option `index` (from 0), or turn it down.
    fn answer(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let Some(q) = self.question.take() else { return };
        let option = index.and_then(|i| q.options.get(i)).map(|o| o.id.as_str());
        if let Some(c) = &self.connection {
            c.respond(q.id, acp::permission_outcome(option));
        }
        cx.notify();
    }

    /// Turn the question down with its own reject option when it has one.
    fn decline(&mut self, cx: &mut Context<Self>) {
        let reject = self.question.as_ref().and_then(|q| q.options.iter().position(|o| o.kind == "reject_once"));
        self.answer(reject, cx);
    }

    pub(crate) fn set_mode(&mut self, mode: String, cx: &mut Context<Self>) {
        let (Some(c), Some(s)) = (self.connection.clone(), self.session_id.clone()) else { return };
        // The agent confirms with `current_mode_update`; showing it at once keeps Shift+Tab presses in step.
        self.thread.mode = Some(mode.clone());
        if let Some(opt) = self.thread.option("mode") {
            let id = opt.id.clone();
            self.set_option(id, mode.clone(), cx);
        }
        let reply = c.request("session/set_mode", json!({"sessionId": s, "modeId": mode}));
        cx.spawn(async move |_, _| drop(reply.await)).detach();
        cx.notify();
    }

    pub(crate) fn set_option(&mut self, id: String, value: String, cx: &mut Context<Self>) {
        let (Some(c), Some(s)) = (self.connection.clone(), self.session_id.clone()) else { return };
        let reply = c.request("session/set_config_option", json!({"sessionId": s, "configId": id, "value": value}));
        cx.spawn(async move |this, cx| {
            if let Ok(result) = reply.await {
                let _ = this.update(cx, |v, cx| {
                    v.thread.apply_session(&result);
                    v.report_chosen(cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn intercept(&mut self, ev: &KeystrokeEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keyboard navigation while the Model picker is open (works even when the search input has focus):
        // Up/Down moves the highlighted model, Left/Right steps effort, Enter confirms, Escape closes.
        if self.picker == Some(ThreadPicker::Model)
            && !ev.keystroke.modifiers.control
            && !ev.keystroke.modifiers.alt
            && !ev.keystroke.modifiers.platform
            && !ev.keystroke.modifiers.shift
        {
            match ev.keystroke.key.as_str() {
                "up" | "down" => {
                    let count = self.filtered_models(cx).len();
                    if count > 0 {
                        let cur = self.model_highlight.min(count - 1);
                        self.model_highlight = if ev.keystroke.key == "up" {
                            if cur == 0 { count - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % count
                        };
                        self.model_scroll.scroll_to_item(self.model_highlight);
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "left" | "right" => {
                    self.step_effort(if ev.keystroke.key == "left" { -1 } else { 1 }, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "enter" => {
                    let models = self.filtered_models(cx);
                    if let Some(choice) = models.get(self.model_highlight.min(models.len().saturating_sub(1))).cloned() {
                        self.pick_model(choice, cx);
                        self.close_picker(window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        // Keyboard navigation while the Permission mode picker is open.
        if self.picker == Some(ThreadPicker::Permission)
            && !ev.keystroke.modifiers.control
            && !ev.keystroke.modifiers.alt
            && !ev.keystroke.modifiers.platform
            && !ev.keystroke.modifiers.shift
        {
            match ev.keystroke.key.as_str() {
                "up" | "down" => {
                    let count = self.thread.modes.len();
                    if count > 0 {
                        let cur = self.mode_highlight.min(count - 1);
                        self.mode_highlight = if ev.keystroke.key == "up" {
                            if cur == 0 { count - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % count
                        };
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "enter" => {
                    if let Some((mode_id, _)) = self.thread.modes.get(self.mode_highlight.min(self.thread.modes.len().saturating_sub(1))).cloned() {
                        self.set_mode(mode_id, cx);
                        self.close_picker(window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        // Escape always closes an open composer picker even if focus briefly left the textarea.
        if self.picker.is_some()
            && ev.keystroke.key == "escape"
            && !ev.keystroke.modifiers.control
            && !ev.keystroke.modifiers.alt
            && !ev.keystroke.modifiers.platform
            && !ev.keystroke.modifiers.shift
        {
            self.close_picker(window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Alt+P / Option+P toggles the model picker even while focus is inside the model search box.
        if ev.keystroke.key == "p"
            && ev.keystroke.modifiers.alt
            && !ev.keystroke.modifiers.control
            && !ev.keystroke.modifiers.platform
            && !ev.keystroke.modifiers.shift
            && (self.composer.focus_handle(cx).is_focused(window) || self.model_search.focus_handle(cx).is_focused(window))
        {
            self.toggle_model_picker(window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if !self.composer.focus_handle(cx).is_focused(window) {
            return;
        }
        let draft = self.composer.read(cx).value().to_string();
        let state = keys::State {
            picker_open: self.picker.is_some(),
            working: self.working,
            question: self.question.as_ref().map_or(0, |q| q.options.len()),
            draft_empty: draft.trim().is_empty(),
            in_history: self.history_at.is_some(),
            escape_just_pressed: self.last_escape.is_some_and(|t| t.elapsed() < keys::DOUBLE_PRESS),
            completing: self.completion(&draft).is_some(),
            commands: self.matching_commands(cx).len(),
        };
        let is_escape = ev.keystroke.key == "escape";
        let Some(key) = keys::key(&ev.keystroke, state) else {
            self.last_escape = is_escape.then(Instant::now);
            return;
        };
        // A double press is used up; a third press starts over.
        self.last_escape = None;
        cx.stop_propagation();
        match key {
            keys::Key::ClosePicker => {
                self.picker = None;
                self.effort_dragging.set(false);
            }
            keys::Key::Interrupt => self.interrupt(cx),
            keys::Key::Decline => self.decline(cx),
            keys::Key::Choose(n) => self.answer(Some(n - 1), cx),
            keys::Key::EditLast => {
                if let Some(last) = self.thread.sent_texts().pop() {
                    self.set_draft(&last, window, cx);
                }
            }
            keys::Key::Clear => {
                self.history_at = None;
                self.set_draft("", window, cx);
            }
            keys::Key::CycleMode => {
                if let Some(next) = self.thread.next_mode().map(String::from) {
                    self.set_mode(next, cx);
                }
            }
            keys::Key::Older | keys::Key::Newer => self.walk_history(key == keys::Key::Older, &draft, window, cx),
            keys::Key::Complete => {
                if let Some(name) = self.highlighted_command(cx).or_else(|| self.completion(&draft)) {
                    self.set_draft(&format!("/{name} "), window, cx);
                }
            }
            keys::Key::MenuUp | keys::Key::MenuDown => {
                let last = self.matching_commands(cx).len().saturating_sub(1);
                let at = self.command_highlight.min(last);
                self.command_highlight = match (key == keys::Key::MenuDown, at) {
                    (true, at) if at >= last => 0,
                    (true, at) => at + 1,
                    (false, 0) => last,
                    (false, at) => at - 1,
                };
                self.command_scroll.scroll_to_item(self.command_highlight);
            }
            keys::Key::RunCommand => {
                if let Some(name) = self.highlighted_command(cx) {
                    self.set_draft(&format!("/{name}"), window, cx);
                    self.submit(window, cx);
                }
            }
            keys::Key::ToggleDetail => {
                self.detail = !self.detail;
                self.remeasure(0);
            }
            keys::Key::TogglePlan => self.plan_open = !self.plan_open,
            keys::Key::PickModel => self.toggle_model_picker(window, cx),
        }
        cx.notify();
    }

    fn report_chosen(&mut self, cx: &mut Context<Self>) {
        let current = |category| self.thread.option(category).map(|o| o.current.clone());
        let now = (current("model"), current("thought_level"));
        if now != self.chosen && (now.0.is_some() || now.1.is_some()) {
            self.chosen = now.clone();
            cx.emit(ThreadEvent::Chosen(now.0, now.1));
        }
    }

    /// Close the model or permission dropdown and give the keyboard back to the message box, which the model
    /// search box took when the dropdown opened.
    pub(super) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    pub(super) fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.effort_dragging.set(false);
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(super) fn toggle_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker == Some(ThreadPicker::Model) {
            self.close_picker(window, cx);
        } else {
            self.picker = Some(ThreadPicker::Model);
            self.picker_at = self.model_anchor.above();
            self.model_search.update(cx, |s, cx| {
                s.set_value("", window, cx);
                s.focus(window, cx);
            });
            let current_id = self.thread.option("model").map(|m| m.current.as_str());
            let models = self.unified_models();
            self.model_highlight = models
                .iter()
                .position(|m| match &self.pending_model {
                    Some(p) => p.harness == m.harness && p.id == m.id,
                    None => m.harness == self.harness && current_id == Some(m.id.as_str()),
                })
                .unwrap_or(0);
            self.model_scroll.scroll_to_item(self.model_highlight);
        }
    }

    pub(super) fn filtered_models(&self, cx: &App) -> Vec<CatalogModel> {
        let query = self.model_search.read(cx).value().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        self.unified_models()
            .into_iter()
            .filter(|m| {
                let hay = format!("{} {} {}", m.name, m.id, m.harness.name()).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect()
    }

    /// `(values, short_labels, current_index, live_option_id)` for the effort slider, with no `"default"` step and
    /// Medium as the fallback default.
    pub(super) fn effort_choices(&self) -> (Vec<String>, Vec<String>, usize, Option<String>) {
        if self.pending_model.is_none()
            && let Some(opt) = self.thread.option("thought_level")
        {
            let non_default: Vec<(String, String)> = opt.choices.iter().filter(|(v, _)| !v.eq_ignore_ascii_case("default")).cloned().collect();
            if !non_default.is_empty() {
                let values: Vec<String> = non_default.iter().map(|(v, _)| v.clone()).collect();
                let labels: Vec<String> = non_default.iter().map(|(_, n)| short_effort_label(n)).collect();
                let ix = values
                    .iter()
                    .position(|v| v == &opt.current)
                    .or_else(|| values.iter().position(|v| v.eq_ignore_ascii_case("medium")))
                    .unwrap_or(1.min(values.len() - 1));
                return (values, labels, ix, Some(opt.id.clone()));
            }
        }
        let values: Vec<String> = crate::claude::defaults::Effort::ALL.iter().map(|e| e.name().to_string()).collect();
        let labels: Vec<String> = vec!["Low".into(), "Med".into(), "High".into(), "XHigh".into(), "Max".into()];
        let ix = self
            .pending_effort
            .as_deref()
            .and_then(|cur| values.iter().position(|v| v.eq_ignore_ascii_case(cur)))
            .unwrap_or(1);
        (values, labels, ix, None)
    }

    fn step_effort(&mut self, delta: isize, cx: &mut Context<Self>) {
        let (values, _, cur, option_id) = self.effort_choices();
        if values.is_empty() {
            return;
        }
        let next = (cur as isize + delta).clamp(0, values.len() as isize - 1) as usize;
        let val = values[next].clone();
        self.pending_effort = Some(val.clone());
        if let Some(id) = option_id {
            self.set_option(id, val, cx);
        }
    }

    /// The slash command a typed `/` prefix completes to: the first one it starts.
    fn completion(&self, draft: &str) -> Option<String> {
        let typed = draft.strip_prefix('/').filter(|t| !t.contains(char::is_whitespace))?;
        self.shown_commands().map(|c| &c.name).find(|n| n.starts_with(typed) && n.as_str() != typed).cloned()
    }

    /// The commands a typed `/` prefix matches, for the menu over the composer: all of them, which it scrolls.
    pub(super) fn matching_commands(&self, cx: &App) -> Vec<&crate::harness::thread::Command> {
        let draft = self.composer.read(cx).value();
        let Some(typed) = draft.strip_prefix('/').filter(|t| !t.contains(char::is_whitespace)) else { return vec![] };
        self.shown_commands().filter(|c| c.name.starts_with(typed)).collect()
    }

    /// The agent's commands, without those m4code's own UI does (`Harness::hidden_commands`).
    fn shown_commands(&self) -> impl Iterator<Item = &crate::harness::thread::Command> {
        let hidden = self.harness.hidden_commands();
        self.thread.commands.iter().filter(move |c| !hidden.contains(&c.name.as_str()))
    }

    fn highlighted_command(&self, cx: &App) -> Option<String> {
        let commands = self.matching_commands(cx);
        commands.get(self.command_highlight.min(commands.len().saturating_sub(1))).map(|c| c.name.clone())
    }

    fn walk_history(&mut self, older: bool, draft: &str, window: &mut Window, cx: &mut Context<Self>) {
        let sent = self.thread.sent_texts();
        if sent.is_empty() {
            return;
        }
        let at = match (self.history_at, older) {
            (None, true) => {
                self.draft = draft.to_string();
                Some(sent.len() - 1)
            }
            (None, false) => return,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) => (i + 1 < sent.len()).then_some(i + 1),
        };
        self.history_at = at;
        let text = at.map_or_else(|| self.draft.clone(), |i| sent[i].clone());
        self.set_draft(&text, window, cx);
    }

    /// Replace what the composer holds, with the cursor after it so typing carries on from the end.
    fn set_draft(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        let end = text.len();
        self.composer.update(cx, |c, cx| {
            c.set_value(text, window, cx);
            c.set_selected_range(end..end, cx);
        });
    }

    fn persist_entries(&self) {
        crate::harness::thread::save_entries(&self.record_id, &self.thread.entries);
    }

    /// Measure entry `from` and every one after it again; the list keeps its place above them.
    pub(crate) fn remeasure(&mut self, from: usize) {
        let rows = self.thread.entries.len();
        let from = from.min(self.rows);
        self.list.splice(from..self.rows, rows - from);
        self.rows = rows;
    }

    fn open_location(&mut self, path: PathBuf, line: Option<u32>, cx: &mut Context<Self>) {
        let path = if path.is_absolute() { path } else { self.cwd.join(path) };
        cx.emit(ThreadEvent::OpenPath(path.to_string_lossy().into_owned(), line.map(|l| l as usize)));
    }

    // What the workspace asks of a session view, as it asks a terminal.

    pub fn is_running(&self) -> bool {
        self.phase != Phase::Ended
    }

    pub fn set_auto_compact(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.compaction.enabled = enabled;
        self.send_next(cx);
        cx.notify();
    }

    pub fn compaction_status(&self) -> &'static str {
        let Some(command) = self.harness.adapter().compact_command else { return "Unsupported by this adapter" };
        if !self.thread.commands.iter().any(|c| c.name == command) {
            return "Compact command unavailable";
        }
        if !self.compaction.native_controlled {
            return if self.compaction.enabled { "80%; native fallback remains" } else { "Off; native fallback remains" };
        }
        if !self.compaction.enabled { return "Off"; }
        if self.thread.context.is_none() { return "Waiting for context usage"; }
        "80% between turns"
    }

    pub fn stop(&self) {
        if let Some(connection) = &self.connection {
            if let Some(session) = &self.session_id {
                connection.notify("session/cancel", json!({"sessionId": session}));
            }
            connection.terminate();
        }
    }

    pub(crate) fn relinquish_record(&mut self) {
        self.persist_on_drop = false;
        self.stop();
    }

    pub fn is_working(&self) -> bool {
        self.working
    }

    /// Waiting for the user to answer a permission question.
    pub fn is_waiting(&self) -> bool {
        self.question.is_some()
    }

    /// What the message box says while the agent starts, so the wait after a hand-off is visible.
    pub fn starting_label(&self) -> Option<String> {
        (self.phase == Phase::Starting && self.handover.is_none())
            .then(|| if self.took_over { format!("Handing over to {}…", self.harness.name()) } else { format!("Starting {}…", self.harness.name()) })
    }

    pub fn is_loading(&self) -> bool {
        self.phase == Phase::Starting || self.handover.is_some()
    }

    pub fn start_error(&self) -> Option<String> {
        self.error.clone()
    }

    /// The last `entries` of the conversation as text, for another session to read.
    pub fn tail_text(&self, entries: usize) -> String {
        self.thread.transcript(entries)
    }

    /// Put `text` in the composer where the cursor is.
    pub fn paste_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        self.composer.update(cx, |c, cx| c.insert(text, window, cx));
    }

    /// Dropped files go in as `@path` mentions, which every harness reads as files to look at.
    pub fn paste_paths(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let mentions: Vec<String> = paths.iter().map(|p| format!("@{}", p.to_string_lossy())).collect();
        self.paste_text(&format!("{} ", mentions.join(" ")), window, cx);
    }

    /// Editor mode's `@path#L12-30: comment`, after whatever the composer holds.
    pub fn add_comment(&mut self, comment: &str, window: &mut Window, cx: &mut Context<Self>) {
        let with = with_comment(&self.draft(cx), comment);
        self.set_draft(&with, window, cx);
    }

    /// What the composer holds.
    pub fn draft(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub fn has_card(&self, call: &str) -> bool {
        self.cards.contains_key(call)
    }

    /// A `show_visual` card, drawn under the tool call that made it.
    pub fn set_card(&mut self, card: Card, cx: &mut Context<Self>) {
        let at = self.thread.entries.iter().position(|e| matches!(e, crate::harness::thread::Entry::Tool(t) if t.id == card.call));
        self.cards.insert(card.call.clone(), card);
        if let Some(at) = at {
            self.remeasure(at);
        }
        cx.notify();
    }
}

/// `initialize`, then open the session: a new one, or the stored one the way its harness reopens. The session's
/// id and what opening it answered.
struct Opened {
    session_id: String,
    result: Value,
    capabilities: Value,
    replayed: bool,
}

async fn open(c: &Connection, harness: Harness, start: &Start) -> Result<Opened, String> {
    let initialized = c.request("initialize", acp::initialize_params()).await.map_err(|e| format!("{} didn't start: {e}", harness.name()))?;
    let capabilities = initialized["agentCapabilities"].clone();
    let mut params = json!({"cwd": start.cwd, "mcpServers": start.mcp_servers, "_meta": start.meta});
    match &start.session_id {
        None => {
            let result = c.request("session/new", params).await.map_err(|e| format!("{} couldn't start a session: {e}", harness.name()))?;
            let id = result["sessionId"].as_str().ok_or("the agent named no session")?.to_string();
            Ok(Opened { session_id: id, result, capabilities, replayed: false })
        }
        Some(id) => {
            params["sessionId"] = json!(id);
            let reopen = acp::reopen_method(&capabilities, harness.adapter().reopen).map_err(|why| format!("{} cannot reopen this session: {why}.", harness.name()))?;
            let method = match reopen {
                Reopen::Load => "session/load",
                Reopen::Resume => "session/resume",
            };
            let result = c.request(method, params).await.map_err(|e| format!("{} couldn't open the session: {e}", harness.name()))?;
            Ok(Opened { session_id: id.clone(), result, capabilities, replayed: reopen == Reopen::Load })
        }
    }
}

impl Drop for ThreadView {
    fn drop(&mut self) {
        self.stop();
        if self.persist_on_drop {
            self.persist_entries();
        }
    }
}

fn is_conversation_update(update: &Value) -> bool {
    matches!(
        update["sessionUpdate"].as_str(),
        Some("user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk" | "tool_call" | "tool_call_update")
    )
}

/// `comment` after the text typed so far, on a line of its own, so several comments read as a list.
pub(crate) fn with_comment(typed: &str, comment: &str) -> String {
    let typed = typed.trim_end();
    if typed.is_empty() { comment.to_string() } else { format!("{typed}\n{comment}") }
}

fn str_of(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::with_comment;
    use crate::harness::{Harness, thread::Entry};

    #[test]
    fn a_comment_goes_on_its_own_line_after_what_was_typed() {
        assert_eq!(with_comment("", "@a.rs#L1: why"), "@a.rs#L1: why");
        assert_eq!(with_comment("look at these \n", "@a.rs#L1: why"), "look at these\n@a.rs#L1: why");
    }

    #[gpui::test]
    fn new_session_shows_prompt_immediately_at_top(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let start = super::Start {
            record_id: String::new(),
            harness: Harness::Claude,
            all_models: vec![],
            picked: vec![],
            cwd: dir.path().to_path_buf(),
            env: vec![],
            mcp_servers: vec![],
            meta: serde_json::Value::Null,
            session_id: None,
            mode: None,
            options: vec![],
            prompt: Some("hello world".into()),
            prior_entries: vec![],
            server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        };
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.read_with(cx, |v, _| {
            assert_eq!(v.thread.entries, [Entry::User("hello world".into())]);
            assert_eq!(v.rows, 1);
            assert_eq!(v.queue.len(), 1);
            assert!(v.queue[0].shown);
        });
        // Paint once so the list lays out, and verify the first entry sits near the top of the viewport, not the bottom.
        cx.refresh().unwrap();
        view.read_with(cx, |v, _| {
            let vp = v.list.viewport_bounds();
            let item = v.list.bounds_for_item(0).expect("first entry laid out");
            assert!(
                item.origin.y - vp.origin.y < vp.size.height / 2.0,
                "expected first entry near top of viewport, got item={item:?} viewport={vp:?}"
            );
        });
    }
}
