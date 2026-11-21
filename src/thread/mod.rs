//! A harness session drawn as a conversation instead of a terminal: it starts the harness's ACP server, opens or
//! creates the session, and sends what the user types. Every harness looks and behaves the same here, keyboard
//! included (`keys`); what differs between them is in `harness`.

mod keys;
mod render;
mod review;

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use futures::StreamExt;
use gpui::{App, AppContext, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, KeystrokeEvent, ListAlignment, ListState, Pixels, Subscription, Window, px};
use gpui_kit::component::input::{InputEvent, TextareaState};
use serde_json::{Value, json};

use crate::harness::acp::{self, Connection, Event, Launch};
use crate::harness::thread::{Entry, Thread};
use crate::harness::{Harness, Reopen};
use crate::visual::Card;
use crate::visual::ui::{CardUi, SendText};

/// Which compact composer popover is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadPicker {
    /// Provider switcher, model list, and draggable effort bar.
    Model,
    /// Permission mode selector dialog.
    Permission,
}

/// A session being moved while its harness restarts: what the waiting screen says.
pub struct Handover {
    pub message: String,
}

pub enum ThreadEvent {
    /// The harness stopped; the session's view closes.
    Exited(Option<i32>),
    /// The agent named the session: a new one learns its id this way.
    SessionId(String),
    Title(String),
    /// A file a tool acted on was clicked: its path and line (1-based).
    OpenPath(String, Option<usize>),
    /// The user switched provider in the composer picker and sent a message: dynamically hand off this chat.
    Handoff { to: Harness, message: String, effort: Option<String> },
}

/// How to bring a session up.
pub struct Start {
    pub harness: Harness,
    /// Enabled harnesses shown in the composer's provider switcher.
    pub enabled_harnesses: Vec<Harness>,
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

/// A permission the agent is waiting for.
struct Question {
    id: Value,
    title: String,
    /// `(option id, name, kind)`; the kind is allow_once, allow_always, reject_once or reject_always.
    options: Vec<(String, String, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Starting,
    Ready,
    Ended,
}

pub struct ThreadView {
    pub harness: Harness,
    /// Enabled harnesses offered in the compact composer switcher.
    pub enabled_harnesses: Vec<Harness>,
    /// If set to another harness, the next message submitted hands this chat over dynamically.
    pub pending_harness: Option<Harness>,
    /// Effort choice staged for the next handoff or while no live `thought_level` option exists yet.
    pub pending_effort: Option<String>,
    cwd: PathBuf,
    connection: Option<Rc<Connection>>,
    session_id: Option<String>,
    pub thread: Thread,
    phase: Phase,
    /// A prompt is out and the turn hasn't ended.
    working: bool,
    /// Messages typed during a turn, sent one at a time after it.
    queue: VecDeque<String>,
    question: Option<Question>,
    composer: Entity<TextareaState>,
    /// Where Up has got to among the messages sent before, and what was typed before it started.
    history_at: Option<usize>,
    draft: String,
    /// A reference editor mode put in the composer, taken out again if the user leaves without typing: the
    /// composer's text before it, and the whole text with it.
    reference: Option<(String, String)>,
    list: ListState,
    /// How many rows the list has measured.
    rows: usize,
    /// Ctrl+O: every tool's output in full.
    pub detail: bool,
    /// Ctrl+T: the plan.
    pub plan_open: bool,
    /// Active compact picker popover above the composer (Alt+P for Model/Provider/Effort, or click Permission).
    pub picker: Option<ThreadPicker>,
    effort_bounds: Rc<Cell<Bounds<Pixels>>>,
    effort_dragging: Rc<Cell<bool>>,
    last_escape: Option<Instant>,
    /// Why the harness stopped, when it said.
    error: Option<String>,
    /// Shown instead of the conversation while the session moves (a hand-off).
    pub handover: Option<Handover>,
    cards: HashMap<String, Card>,
    card_ui: Entity<CardUi>,
    /// The working tree as it was before each message, oldest first (`git/snapshot.rs`). Empty outside a git
    /// repository.
    pub snapshots: Vec<String>,
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
        let name = start.harness.name();
        let composer = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 12).submit_on_enter(true).placeholder(format!("Message {name}")));
        let card_ui = cx.new(|_| CardUi::default());
        let this = cx.entity().downgrade();
        let mut subscriptions = vec![
            cx.subscribe_in(&composer, window, |v: &mut Self, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => v.submit(window, cx),
                InputEvent::Change => {
                    if v.reference.as_ref().is_some_and(|(_, with)| v.composer.read(cx).value().as_ref() != with) {
                        v.reference = None;
                    }
                    cx.notify();
                }
                _ => {}
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
        let mut thread = Thread::default();
        let rows = start.prior_entries.len();
        thread.entries = start.prior_entries.clone();
        let enabled_harnesses = if start.enabled_harnesses.is_empty() {
            vec![start.harness]
        } else {
            start.enabled_harnesses.clone()
        };
        let mut view = ThreadView {
            harness: start.harness,
            enabled_harnesses,
            pending_harness: None,
            pending_effort: start.options.iter().find(|(k, _)| *k == "thought_level").map(|(_, v)| v.clone()),
            cwd: start.cwd.clone(),
            connection: None,
            session_id: start.session_id.clone(),
            thread,
            phase: Phase::Starting,
            working: false,
            queue: start.prompt.clone().into_iter().collect(),
            question: None,
            composer,
            history_at: None,
            draft: String::new(),
            reference: None,
            list: ListState::new(rows, ListAlignment::Bottom, px(600.0)),
            rows,
            detail: false,
            plan_open: false,
            picker: None,
            effort_bounds: Rc::new(Cell::new(Bounds::default())),
            effort_dragging: Rc::new(Cell::new(false)),
            last_escape: None,
            error: None,
            handover: None,
            cards: HashMap::new(),
            card_ui,
            snapshots: Vec::new(),
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
            let _ = this.update(cx, |v, _| v.connection = Some(connection.clone()));
            let opened = open(&connection, harness, &start).await;
            let ok = this.update(cx, |v, cx| v.opened(opened, &start, cx)).unwrap_or(false);
            if !ok {
                return;
            }
            while let Some(event) = incoming.next().await {
                if this.update(cx, |v, cx| v.on_event(event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// The session is open, or failed to open. Returns whether to go on listening.
    fn opened(&mut self, opened: Result<(String, Value), String>, start: &Start, cx: &mut Context<Self>) -> bool {
        let (session_id, result) = match opened {
            Ok(o) => o,
            Err(why) => {
                self.ended(Some(why), cx);
                return false;
            }
        };
        self.thread.apply_session(&result);
        if self.session_id.as_deref() != Some(&session_id) {
            self.session_id = Some(session_id.clone());
            cx.emit(ThreadEvent::SessionId(session_id));
        }
        if start.session_id.is_none() {
            if let Some(mode) = start.mode.clone().filter(|m| self.thread.modes.iter().any(|(id, _)| id == m)) {
                self.set_mode(mode, cx);
            }
            for (category, value) in &start.options {
                if let Some(option) = self.thread.option(category).filter(|o| o.choices.iter().any(|(v, _)| v == value)) {
                    let id = option.id.clone();
                    self.set_option(id, value.clone(), cx);
                }
            }
        }
        self.phase = Phase::Ready;
        // A move (a worktree switch, a hand-off) is over once the session is open where it went.
        self.handover = None;
        self.remeasure(0);
        self.send_next(cx);
        cx.notify();
        true
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Update(update) => {
                let title = self.thread.title.clone();
                if let Some(at) = self.thread.apply(&update) {
                    self.remeasure(at);
                }
                if self.thread.title != title
                    && let Some(t) = self.thread.title.clone()
                {
                    cx.emit(ThreadEvent::Title(t));
                }
            }
            Event::Permission { id, params } => {
                let options = params["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| (str_of(o, "optionId"), str_of(o, "name"), str_of(o, "kind")))
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

    /// Stage a switch to another harness; the next message sent dynamically hands off the chat.
    pub fn stage_harness(&mut self, to: Harness, cx: &mut Context<Self>) {
        self.pending_harness = (to != self.harness).then_some(to);
        cx.notify();
    }

    /// Enter in the composer.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        self.history_at = None;
        self.reference = None;
        self.picker = None;
        if let Some(to) = self.pending_harness.take().filter(|&h| h != self.harness) {
            let effort = self.pending_effort.take();
            cx.emit(ThreadEvent::Handoff { to, message: text, effort });
            cx.notify();
            return;
        }
        self.send(text, window, cx);
    }

    /// Send `text` as the user's next message, now or after the turn that is running.
    pub fn send(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(to) = self.pending_harness.take().filter(|&h| h != self.harness) {
            let effort = self.pending_effort.take();
            self.picker = None;
            cx.emit(ThreadEvent::Handoff { to, message: text, effort });
            cx.notify();
            return;
        }
        self.queue.push_back(text);
        self.send_next(cx);
        cx.notify();
    }

    fn send_next(&mut self, cx: &mut Context<Self>) {
        if self.working || self.phase != Phase::Ready {
            return;
        }
        let (Some(connection), Some(session)) = (self.connection.clone(), self.session_id.clone()) else { return };
        let Some(text) = self.queue.pop_front() else { return };
        let at = self.thread.sent(text.clone());
        self.remeasure(at);
        self.list.scroll_to_end();
        self.working = true;
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            // The snapshot comes first, so the review sees what this turn changed. Outside a repository it fails, and the
            // message goes anyway.
            let snapshot = {
                let session = session.clone();
                cx.background_executor().spawn(async move { crate::git::snapshot::take(&cwd, &session) }).await
            };
            if let Ok(id) = snapshot {
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
        if let Some((text, error)) = notice {
            let at = self.thread.notice(text, error);
            self.remeasure(at);
        }
        self.send_next(cx);
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
        let option = index.and_then(|i| q.options.get(i)).map(|(id, _, _)| id.as_str());
        if let Some(c) = &self.connection {
            c.respond(q.id, acp::permission_outcome(option));
        }
        cx.notify();
    }

    /// Turn the question down with its own reject option when it has one.
    fn decline(&mut self, cx: &mut Context<Self>) {
        let reject = self.question.as_ref().and_then(|q| q.options.iter().position(|(_, _, kind)| kind == "reject_once"));
        self.answer(reject, cx);
    }

    fn set_mode(&mut self, mode: String, cx: &mut Context<Self>) {
        let (Some(c), Some(s)) = (self.connection.clone(), self.session_id.clone()) else { return };
        // The agent confirms with `current_mode_update`; showing it at once keeps Shift+Tab presses in step.
        self.thread.mode = Some(mode.clone());
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
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn intercept(&mut self, ev: &KeystrokeEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Escape always closes an open composer picker even if focus briefly left the textarea.
        if self.picker.is_some()
            && ev.keystroke.key == "escape"
            && !ev.keystroke.modifiers.control
            && !ev.keystroke.modifiers.alt
            && !ev.keystroke.modifiers.platform
            && !ev.keystroke.modifiers.shift
        {
            self.picker = None;
            self.effort_dragging.set(false);
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
                if let Some(name) = self.completion(&draft) {
                    self.set_draft(&format!("/{name} "), window, cx);
                }
            }
            keys::Key::ToggleDetail => {
                self.detail = !self.detail;
                self.remeasure(0);
            }
            keys::Key::TogglePlan => self.plan_open = !self.plan_open,
            keys::Key::PickModel => {
                self.picker = if self.picker == Some(ThreadPicker::Model) { None } else { Some(ThreadPicker::Model) };
            }
        }
        cx.notify();
    }

    /// The slash command a typed `/` prefix completes to: the first one it starts.
    fn completion(&self, draft: &str) -> Option<String> {
        let typed = draft.strip_prefix('/').filter(|t| !t.contains(char::is_whitespace))?;
        self.thread.commands.iter().map(|c| &c.name).find(|n| n.starts_with(typed) && n.as_str() != typed).cloned()
    }

    /// The commands a typed `/` prefix matches, for the menu over the composer.
    fn matching_commands(&self, cx: &App) -> Vec<&crate::harness::thread::Command> {
        let draft = self.composer.read(cx).value();
        let Some(typed) = draft.strip_prefix('/').filter(|t| !t.contains(char::is_whitespace)) else { return vec![] };
        self.thread.commands.iter().filter(|c| c.name.starts_with(typed)).take(8).collect()
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

    /// Measure entry `from` and every one after it again; the list keeps its place above them.
    fn remeasure(&mut self, from: usize) {
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

    pub fn is_working(&self) -> bool {
        self.working
    }

    /// Waiting for the user to answer a permission question.
    pub fn is_waiting(&self) -> bool {
        self.question.is_some()
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

    /// Editor mode's `@path#L12-30: ` for the selected lines, after whatever the composer holds. Whether it went in.
    pub fn insert_reference(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.reference.is_some() || self.is_loading() {
            return false;
        }
        let before = self.draft(cx);
        let with = with_reference(&before, text);
        self.set_draft(&with, window, cx);
        self.reference = Some((before, with));
        true
    }

    /// Take the reference out again when nothing was typed after it.
    pub fn withdraw_reference(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((before, with)) = self.reference.take()
            && self.draft(cx) == with
        {
            self.set_draft(&before, window, cx);
        }
    }

    /// What the composer holds.
    pub fn draft(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub fn has_card(&self, call: &str) -> bool {
        self.cards.contains_key(call)
    }

    pub fn input_holds(&self, text: &str, cx: &App) -> bool {
        self.composer.read(cx).value().contains(text)
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
async fn open(c: &Connection, harness: Harness, start: &Start) -> Result<(String, Value), String> {
    c.request("initialize", acp::initialize_params()).await.map_err(|e| format!("{} didn't start: {e}", harness.name()))?;
    let mut params = json!({"cwd": start.cwd, "mcpServers": start.mcp_servers, "_meta": start.meta});
    match &start.session_id {
        None => {
            let result = c.request("session/new", params).await.map_err(|e| format!("{} couldn't start a session: {e}", harness.name()))?;
            let id = result["sessionId"].as_str().ok_or("the agent named no session")?.to_string();
            Ok((id, result))
        }
        Some(id) => {
            params["sessionId"] = json!(id);
            let method = match harness.adapter().reopen {
                Reopen::Load => "session/load",
                Reopen::Resume => "session/resume",
            };
            let result = c.request(method, params).await.map_err(|e| format!("{} couldn't open the session: {e}", harness.name()))?;
            Ok((id.clone(), result))
        }
    }
}

/// `reference: ` after the text typed so far, on the same line with a space, so the user types their comment after
/// it; they start a new line themselves when they want one.
fn with_reference(typed: &str, reference: &str) -> String {
    let typed = typed.trim_end();
    if typed.is_empty() { format!("{reference}: ") } else { format!("{typed} {reference}: ") }
}

fn str_of(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::with_reference;

    #[test]
    fn a_reference_follows_what_was_typed() {
        assert_eq!(with_reference("", "@a.rs#L1"), "@a.rs#L1: ");
        assert_eq!(with_reference("explain \n", "@a.rs#L1"), "explain @a.rs#L1: ");
    }
}
