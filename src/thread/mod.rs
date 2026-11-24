//! A harness session drawn as a conversation instead of a terminal: it starts the harness's ACP server, opens or
//! creates the session, and sends what the user types. Every harness looks and behaves the same here, keyboard
//! included (`keys`); what differs between them is in `harness`.

mod asks;
mod elements;
mod keys;
mod queue;
mod render;
mod review;
pub(crate) use review::clear_turn_reviews;
mod timeline;
mod find;
mod telemetry;
pub(crate) use telemetry::exhausted_usage;

pub use render::short_effort_label;

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use futures::StreamExt;
use gpui::{App, AppContext, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, FollowMode, KeystrokeEvent, ListAlignment, ListState, Pixels, Point, Subscription, Window, px};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use serde_json::{Value, json};
use uuid::Uuid;

use self::queue::Queued;

use crate::harness::acp::{self, Connection, Event, Launch};
use crate::harness::codex;
use crate::harness::thread::{Entry, Thread, ToolStatus};
use crate::harness::permission::{self, Permission};
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

/// A background task (such as a running child subagent) displayed in the main chat view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundTask {
    pub id: String,
    pub label: String,
    pub waiting: bool,
}

struct ProviderShell {
    id: String,
    label: String,
    stopping: bool,
}

/// A background shell/command task reported in tool output (e.g. Antigravity's `run_command` async tasks).
struct BackgroundToolTask {
    id: String,
    label: String,
}

const BG_TASK_PREFIX: &str = "Tool is running as a background task with task id:";

fn tool_output_texts(call: &crate::harness::thread::ToolCall) -> Vec<&str> {
    let mut texts = Vec::new();
    if let Some(out) = call.details.output.as_deref() {
        texts.push(out);
    }
    if let Some(out) = call.details.stdout.as_deref() {
        texts.push(out);
    }
    for content in &call.content {
        if let crate::harness::thread::ToolContent::Text(text) = content {
            texts.push(text.as_str());
        }
    }
    texts
}

fn background_task_from_tool(call: &crate::harness::thread::ToolCall) -> Option<(String, String)> {
    let task_id = tool_output_texts(call).into_iter().find_map(|text| {
        let rest = text.split_once(BG_TASK_PREFIX)?.1.trim_start();
        let id = rest.split_whitespace().next()?.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '.'));
        (!id.is_empty()).then(|| id.to_string())
    })?;
    let label = call
        .details
        .command
        .as_deref()
        .or(Some(call.title.as_str()))
        .and_then(|s| s.lines().find(|l| !l.trim().is_empty()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Background task".into());
    Some((task_id, label))
}

fn text_retires_background_task(text: &str, task_id: &str) -> bool {
    text.lines().any(|line| {
        let Some((_, rest)) = line.split_once(task_id) else { return false };
        let status = rest.trim_start_matches(['"', '\'', ')', ']', ':', ' ']).to_ascii_lowercase();
        ["finished", "canceled", "cancelled", "killed", "exited"].iter()
            .any(|word| status.strip_prefix(word).is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '.', ':'])))
    })
}

struct ShellOutput {
    task_id: String,
    text: String,
    loading: bool,
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

pub(crate) const FORK_SEED: &str = "Forked thread. Send a message to continue.";

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
    /// The harness reported per-turn token and prompt-cache usage.
    Usage(crate::harness::thread::TurnUsage),
    /// The harness reported live account rate limits (5h / 7d windows).
    UsageLimits(Harness, crate::store::UsageLimits),
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
    AbortChildren,
    /// A subagent's report card was clicked: show that session.
    Select(String),
    /// Built-in `/new [prompt]` command: start a new session in this project.
    NewSession(Option<String>),
    Fork(usize),
    OpenToolShell(String),
    /// Built-in `/clear` command: clear this session's conversation and start fresh in place.
    ClearSession,
    /// Built-in `/compact [instructions]` fallback when the harness doesn't provide native compaction.
    Compact { transcript: String, instructions: Option<String> },
}

/// Built-in slash commands always offered by m4code regardless of harness.
const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("compact", "Compact conversation context"),
    ("new", "Start a new session in this project"),
    ("clear", "Clear conversation and start fresh"),
];

/// Find an active `/command` token around `cursor` (byte offset in `draft`).
/// Triggers at the start of `draft` OR immediately after any whitespace so paths like `src/mod.rs` and URLs are ignored.
/// Returns `(token_start, token_end, typed_after_slash_up_to_cursor)`.
pub(super) fn active_slash_token(draft: &str, cursor: usize) -> Option<(usize, usize, &str)> {
    let mut cur = cursor.min(draft.len());
    while cur > 0 && !draft.is_char_boundary(cur) {
        cur -= 1;
    }
    let before = &draft[..cur];
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    if !draft[start..].starts_with('/') {
        return None;
    }
    let after = &draft[cur..];
    let end = cur
        + after
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map_or(after.len(), |(i, _)| i);
    let typed = &draft[start + 1..cur];
    Some((start, end, typed))
}

/// Find byte ranges of all `/command` tokens in `draft` (at start or after whitespace) together with their name slice.
pub(super) fn slash_command_spans(draft: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let mut out = Vec::new();
    let mut prev_ws = true;
    let mut iter = draft.char_indices().peekable();
    while let Some((i, ch)) = iter.next() {
        if prev_ws && ch == '/' {
            let start = i;
            let mut end = i + 1;
            while let Some(&(j, next)) = iter.peek() {
                if next.is_whitespace() {
                    break;
                }
                end = j + next.len_utf8();
                iter.next();
            }
            if end > start + 1 {
                out.push((start..end, &draft[start + 1..end]));
            }
            prev_ws = false;
        } else {
            prev_ws = ch.is_whitespace();
        }
    }
    out
}

type InitialOption = (&'static str, String, String);

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
    /// Whether native inline generative UI (` ```ui ` blocks and `show_visual` cards) is enabled.
    pub inline_visuals: bool,
}

/// One selectable option in a permission or AskUserQuestion prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOption {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub kind: String,
}

/// Public read-only view of a pending question, for rendering outside the chat (e.g. sidebar hover popover).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQuestion {
    pub id: Uuid,
    pub title: String,
    pub options: Vec<QuestionOption>,
}

enum QuestionReply {
    Acp(Value),
    Elicitation {
        id: Value,
        field: String,
        kind: acp::ElicitationKind,
        answers: serde_json::Map<String, Value>,
        remaining: VecDeque<acp::ElicitationField>,
    },
    Bridge(String),
}

/// A permission or multiple-choice question the agent is waiting for.
struct Question {
    id: Uuid,
    reply: QuestionReply,
    title: String,
    options: Vec<QuestionOption>,
    /// The option Enter picks; the arrows and the mouse move it.
    highlight: usize,
    /// What is typed in the message box can be sent as the answer instead of an option (`acp::takes_own_answer`).
    takes_own_answer: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Starting,
    Ready,
    Ended,
}

struct SelectionComment {
    quote: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}

struct ChatMenu {
    at: Point<Pixels>,
    quote: String,
    anchor: Point<Pixels>,
}

struct AgentCache {
    text_len: usize,
    inline_visuals: bool,
    citations: Vec<elements::Citation>,
    segments: Vec<timeline::InlineSegment>,
}

#[derive(Default)]
struct RunCache {
    entries: usize,
    cards: usize,
    inline_visuals: bool,
    ranges: Vec<Option<std::ops::Range<usize>>>,
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
    /// Running child subagents / workspace background tasks attached to this session.
    pub background: Vec<BackgroundTask>,
    provider_shells: Vec<ProviderShell>,
    background_tasks: Vec<BackgroundToolTask>,
    seen_background_tasks: HashSet<String>,
    comment_anchor: Option<(Point<Pixels>, String)>,
    selection_comment: Option<SelectionComment>,
    restore_focus: FocusHandle,
    shell_output: Option<ShellOutput>,
    /// Model from another harness staged for dynamic handoff on the next message.
    pub pending_model: Option<CatalogModel>,
    /// Model requested at session start/resume, shown until the harness applies it.
    initial_model: Option<String>,
    /// Right-click context menu snapshot in the chat view.
    chat_menu: Option<ChatMenu>,
    /// Whether native inline generative UI (` ```ui ` blocks and `show_visual` cards) is enabled.
    pub inline_visuals: bool,
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
    /// The sending end of the channel the agent's events arrive on, for `Event::TurnEnded`.
    events: Option<futures::channel::mpsc::UnboundedSender<Event>>,
    session_id: Option<String>,
    pub thread: Thread,
    phase: Phase,
    /// A prompt is out and the turn hasn't ended.
    working: bool,
    /// When the last live conversation update arrived (outside replay), so detached/post-turn streaming still shows as working.
    last_update: Option<Instant>,
    /// Messages typed during a turn, sent one at a time after it.
    queue: VecDeque<Queued>,
    queue_held: bool,
    queue_edit: Option<queue::Edit>,
    preparing: Option<Queued>,
    restarting_for_steer: bool,
    interrupting: bool,
    queue_save_error: bool,
    questions: VecDeque<Question>,
    asks: asks::AsyncAsks,
    pub composer: Entity<TextareaState>,
    attachments: crate::panels::attachments::Attachments,
    /// Where Up has got to among the messages sent before, and what was typed before it started.
    history_at: Option<usize>,
    draft: String,
    list: ListState,
    chat_glide: crate::panels::glide::SmoothList,
    screenshot: Option<std::sync::Arc<gpui::Image>>,
    screenshot_focus: FocusHandle,
    viewing: bool,
    tool_images: std::cell::RefCell<HashMap<(String, usize), std::sync::Arc<gpui::Image>>>,
    find: Option<find::Find>,
    pub(crate) usage_limit: Option<String>,
    /// How many rows the list has measured.
    rows: usize,
    /// Ctrl+O: every tool's output in full.
    pub detail: bool,
    pub(crate) subagent_edits: Option<bool>,
    /// Each message's parsed markdown, by entry. The chat keeps it rather than the frame: a row that scrolls out of
    /// the list would otherwise drop it and parse again when it comes back, and a long message (over 4 KB) parses
    /// in the background, so it would draw empty for a frame and then grow, moving everything around it.
    markdown: HashMap<usize, Entity<gpui_kit::component::text::TextViewState>>,
    /// Cached formatted length for `Entry::User` and parsed citations/segments for `Entry::Agent` so scroll frames
    /// never re-scan markdown text.
    user_md_len: HashMap<usize, usize>,
    agent_cache: HashMap<usize, AgentCache>,
    /// Cached `tool_run` ranges keyed by `(entries.len(), cards.len())` for O(1) lookup during scroll.
    run_cache: std::cell::RefCell<RunCache>,
    jump_visible: Cell<bool>,
    /// Messages steered into the running turn that the agent hasn't read yet, oldest first. They join the
    /// conversation where the agent reads them (a `user_message_chunk` marked `_meta.m4code.steer`), because what it
    /// was still writing when they were sent comes before them.
    steering: VecDeque<Queued>,
    /// The running turn is a slash command (such as `/compact`), which a steer would cut short.
    running_command: bool,
    /// What a message sent while the agent works does (Settings → General).
    pub(crate) follow_up: crate::store::FollowUp,
    pub compaction: crate::harness::compaction::Policy,
    pub(crate) canonical_history: bool,
    persist_on_drop: bool,
    pub capabilities: Value,
    agent_meta: Value,
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
    pub(super) model_scroll: crate::panels::glide::Smooth,
    pub(super) model_rows: Rc<std::cell::RefCell<Vec<Bounds<Pixels>>>>,
    /// Tool call ids manually expanded by clicking their compressed single-line header.
    expanded_tools: HashSet<String>,
    /// Shell calls closed by hand while Ctrl+O is showing tool details.
    collapsed_shell_calls: HashSet<String>,
    /// Thoughts closed by hand while Ctrl+O is showing tool details.
    collapsed_thoughts: HashSet<usize>,
    /// Per-thought `(started_at, finished_duration)` so `Reasoning` shows live streaming state and elapsed time.
    thought_timing: HashMap<usize, (Instant, Option<std::time::Duration>)>,
    effort_bounds: Rc<Cell<Bounds<Pixels>>>,
    effort_dragging: Rc<Cell<bool>>,
    last_escape: Option<Instant>,
    /// Why the harness stopped, when it said.
    error: Option<String>,
    /// True while showing locally cached entries for a `Reopen::Load` harness before its live replay arrives.
    cached_replay: bool,
    /// This view took over a conversation from another harness (an in-place hand-off), and shows it while it starts.
    took_over: bool,
    cards: HashMap<String, Card>,
    card_ui: Entity<CardUi>,
    /// The working tree as it was before each message, oldest first (`git/snapshot.rs`). Empty outside a git
    /// repository.
    pub snapshots: Vec<String>,
    turn_reviews: Vec<review::TurnReview>,
    active_turn_review: Option<review::ActiveTurn>,
    /// The slash menu's highlighted row, and its scroll.
    command_highlight: usize,
    command_scroll: crate::panels::glide::Smooth,
    permission_scroll: crate::panels::glide::Smooth,
    question_scroll: crate::panels::glide::Smooth,
    queue_scroll: crate::panels::glide::Smooth,
    shell_output_scroll: crate::panels::glide::Smooth,
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
                InputEvent::PressEnter { secondary, .. } if crate::panels::composer::sends(ev) => v.submit_with(*secondary, window, cx),
                InputEvent::Change => {
                    v.command_highlight = 0;
                    v.sync_command_highlights(cx);
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe_in(&model_search, window, |v: &mut Self, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    v.model_highlight = 0;
                    v.model_scroll.scroll_to_item(0);
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
        let mut queue = queue::load(&start.record_id);
        for item in &mut queue {
            item.order = queue::next_order();
            item.ahead = false;
            item.shown &= matches!(start.prior_entries.last(), Some(Entry::User(text)) if text == &item.text);
        }
        let queue_held = !queue.is_empty();
        if let Some(prompt) = start.prompt.clone() {
            // Show a new session's first message immediately instead of a blank screen while the agent starts.
            // A resumed session replays history during startup, so keep its prompt queued until open finishes.
            let shown = start.session_id.is_none();
            if shown {
                thread.sent(prompt.clone());
            }
            queue.push_front(Queued { startup: true, shown, ..Queued::new(prompt, start.prior_entries.is_empty()) });
        }
        let rows = thread.entries.len();
        let list = ListState::new(rows, ListAlignment::Top, px(600.0));
        list.set_follow_mode(FollowMode::Tail);
        let cached_replay = start.session_id.is_some()
            && !start.prior_entries.is_empty()
            && start.harness.adapter().reopen == Reopen::Load;
        let took_over = start.session_id.is_none() && !start.prior_entries.is_empty();
        let turn_reviews = if start.prior_entries.is_empty() { Vec::new() } else { review::load_turn_reviews(&start.record_id) };
        let canonical_history = !turn_reviews.is_empty()
            || start.prior_entries.iter().any(|entry| matches!(entry, Entry::Notice { text, error: false } if text == FORK_SEED));
        let this = cx.entity().downgrade();
        list.set_scroll_handler(move |_, _, cx| {
            // GPUI still holds the list state while it calls its scroll handler.
            let this = this.clone();
            cx.defer(move |cx| {
                let _ = this.update(cx, |v, cx| {
                    let show_jump = v.show_jump_to_latest();
                    let changed = v.jump_visible.replace(show_jump) != show_jump;
                    if changed || v.chat_menu.is_some() || v.comment_anchor.is_some() {
                        cx.notify();
                    }
                });
            });
        });
        let mut view = ThreadView {
            record_id: start.record_id.clone(),
            harness: start.harness,
            all_models: start.all_models.clone(),
            picked: start.picked.clone(),
            git: None,
            background: vec![],
            provider_shells: vec![],
            background_tasks: vec![],
            seen_background_tasks: HashSet::new(),
            comment_anchor: None,
            selection_comment: None,
            restore_focus: cx.focus_handle(),
            shell_output: None,
            pending_model: None,
            initial_model: start.options.iter().find(|(k, _)| *k == "model").map(|(_, v)| v.clone()),
            chat_menu: None,
            inline_visuals: start.inline_visuals,
            handoff_after_turn: None,
            pending_effort: start.options.iter().find(|(k, _)| *k == "thought_level").map(|(_, v)| v.clone()),
            chosen: (None, None),
            model_search,
            cwd: start.cwd.clone(),
            connection: None,
            events: None,
            session_id: start.session_id.clone(),
            thread,
            phase: Phase::Starting,
            working: false,
            last_update: None,
            queue,
            queue_held,
            queue_edit: None,
            preparing: None,
            restarting_for_steer: false,
            interrupting: false,
            queue_save_error: false,
            questions: VecDeque::new(),
            asks: asks::AsyncAsks::default(),
            composer,
            attachments: Default::default(),
            history_at: None,
            draft: String::new(),
            list,
            chat_glide: Default::default(),
            screenshot: None,
            screenshot_focus: cx.focus_handle(),
            viewing: false,
            tool_images: Default::default(),
            find: None,
            usage_limit: None,
            rows,
            detail: false,
            subagent_edits: None,
            markdown: HashMap::new(),
            user_md_len: HashMap::new(),
            agent_cache: HashMap::new(),
            run_cache: Default::default(),
            jump_visible: Cell::new(false),
            steering: VecDeque::new(),
            running_command: false,
            follow_up: crate::store::FollowUp::default(),
            compaction: Default::default(),
            canonical_history,
            persist_on_drop: true,
            capabilities: Value::Null,
            agent_meta: Value::Null,
            plan_open: false,
            picker: None,
            picker_at: crate::panels::widgets::Dropdown::at(gpui::point(px(0.0), px(0.0))),
            model_anchor: crate::panels::widgets::Anchor::default(),
            model_highlight: 0,
            mode_highlight: 0,
            model_scroll: Default::default(),
            model_rows: Default::default(),
            expanded_tools: HashSet::new(),
            collapsed_shell_calls: HashSet::new(),
            collapsed_thoughts: HashSet::new(),
            thought_timing: HashMap::new(),
            effort_bounds: Rc::new(Cell::new(Bounds::default())),
            effort_dragging: Rc::new(Cell::new(false)),
            last_escape: None,
            error: None,
            cached_replay,
            took_over,
            cards: HashMap::new(),
            card_ui,
            snapshots: Vec::new(),
            turn_reviews,
            active_turn_review: None,
            command_highlight: 0,
            command_scroll: Default::default(),
            permission_scroll: Default::default(),
            question_scroll: Default::default(),
            queue_scroll: Default::default(),
            shell_output_scroll: Default::default(),
            changed: None,
            review: None,
            _subscriptions: subscriptions,
        };
        if start.prompt.is_some() {
            view.queue_changed();
        }
        view.start(start, cx);
        view
    }

    fn start(&mut self, start: Start, cx: &mut Context<Self>) {
        let (events, mut incoming) = futures::channel::mpsc::unbounded();
        self.events = Some(events.clone());
        let harness = start.harness;
        cx.spawn(async move |this, cx| {
            // Finding the server may read the login shell's PATH, so it is off the UI thread.
            let cwd = start.cwd.clone();
            let env = start.env.clone();
            let server = start.server.clone();
            let codex_model = (harness == Harness::Codex)
                .then(|| start.options.iter().find(|(k, _)| *k == "model").map(|(_, v)| v.clone()))
                .flatten()
                .filter(|m| !m.is_empty());
            let started = cx
                .background_executor()
                .spawn(async move {
                    let (program, mut args) = match server {
                        Some(s) => s,
                        None => (harness.adapter().server)()?,
                    };
                    if let Some(model) = codex_model {
                        args.push("-c".into());
                        args.push(format!("model={model:?}"));
                    }
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
            let Some((sid, mut initial_options)) = this.update(cx, |v, cx| v.opened(opened, &start, cx)).ok().flatten() else {
                return;
            };
            // Drain any stale session/new or session/load notifications (such as default config_option_update)
            // BEFORE applying the requested model/effort so they cannot overwrite our selection.
            while let Ok(event) = incoming.try_recv() {
                if this.update(cx, |v, cx| v.on_event(event, cx)).is_err() {
                    return;
                }
            }
            // Apply model before thought_level because switching models can change available effort options.
            initial_options.sort_by_key(|(cat, _, _)| if *cat == "model" { 0 } else { 1 });
            for (category, config_id, value) in initial_options {
                if let Ok(result) = connection
                    .request("session/set_config_option", json!({"sessionId": &sid, "configId": &config_id, "value": &value}))
                    .await
                {
                    let _ = this.update(cx, |v, cx| {
                        v.thread.apply_session(&result);
                        // If the harness returned an empty/unchanged payload, keep the requested value visible.
                        if let Some(opt) = v.thread.config.iter_mut().find(|o| o.category == category)
                            && opt.current != value
                            && opt.choices.iter().any(|(c, _)| c == &value)
                        {
                            opt.current = value.clone();
                        }
                        v.report_chosen(cx);
                        cx.notify();
                    });
                }
            }
            let _ = this.update(cx, |v, cx| {
                v.initial_model = None;
                cx.notify();
            });
            while let Ok(event) = incoming.try_recv() {
                if this.update(cx, |v, cx| v.on_event(event, cx)).is_err() {
                    return;
                }
            }
            let ready = this.update(cx, |v, cx| {
                if v.phase != Phase::Starting { return false; }
                v.phase = Phase::Ready;
                v.cached_replay = false;
                v.settle_opencode_shells();
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

    /// The session is open, or failed to open. Returns the session id and any initial config options to apply before
    /// the first prompt runs.
    fn opened(&mut self, opened: Result<Opened, String>, start: &Start, cx: &mut Context<Self>) -> Option<(String, Vec<InitialOption>)> {
        let Opened { session_id, result, capabilities, agent_meta, replayed } = match opened {
            Ok(o) => o,
            Err(why) => {
                self.ended(Some(why), cx);
                return None;
            }
        };
        self.capabilities = capabilities;
        self.agent_meta = agent_meta;
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
            cx.emit(ThreadEvent::SessionId(session_id.clone()));
        }
        // A resumed session keeps the mode the harness restores; its options are the model and effort picked in it.
        if (start.session_id.is_none() || self.subagent_edits.is_some())
            && let Some(mode) = start.mode.as_deref().and_then(|m| self.listed_or_safer(m))
        {
            self.set_mode(mode, cx);
        }
        let mut initial_options = Vec::new();
        for (category, value) in &start.options {
            if let Some(option) = self.thread.option(category).filter(|o| o.current != *value && o.choices.iter().any(|(v, _)| v == value)) {
                initial_options.push((*category, option.id.clone(), value.clone()));
            }
        }
        if initial_options.is_empty() {
            self.initial_model = None;
        }
        self.report_chosen(cx);
        self.remeasure(0);
        cx.notify();
        Some((session_id, initial_options))
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Update(update) => {
                if update["sessionUpdate"] == "_m4code/session_ended" {
                    self.ended(update["error"].as_str().map(String::from), cx);
                    return;
                }
                if update["sessionUpdate"] == "_m4code/usage_limits" {
                    if let Some(limits) = crate::harness::claude::limits::parse_value(&update["limits"], crate::store::now()) {
                        cx.emit(ThreadEvent::UsageLimits(self.harness, limits));
                    }
                    return;
                }
                if update["sessionUpdate"] == "_m4code/background_shells" {
                    let mut previous = std::mem::take(&mut self.provider_shells);
                    self.provider_shells = update["shells"].as_array().into_iter().flatten().filter_map(|task| {
                        let id = task["id"].as_str()?.to_string();
                        let label = task["label"].as_str().unwrap_or("Shell command").to_string();
                        let stopping = previous.iter().position(|shell| shell.id == id)
                            .is_some_and(|at| previous.swap_remove(at).stopping);
                        Some(ProviderShell { id, label, stopping })
                    }).collect();
                    if self.shell_output.as_ref().is_some_and(|output| !self.provider_shells.iter().any(|task| task.id == output.task_id)) {
                        self.shell_output = None;
                    }
                    cx.notify();
                    return;
                }
                if self.cached_replay && !self.working && is_conversation_update(&update) {
                    if self.canonical_history { return; }
                    self.cached_replay = false;
                    self.thread.entries.clear();
                    self.markdown.clear();
                    self.user_md_len.clear();
                    self.agent_cache.clear();
                    self.run_cache.get_mut().ranges.clear();
                    for item in &mut self.queue {
                        item.shown = false;
                    }
                    self.tool_images.get_mut().clear();
                    self.thought_timing.clear();
                    self.collapsed_thoughts.clear();
                    self.remeasure(0);
                }
                if self.phase == Phase::Ready && !self.cached_replay && is_conversation_update(&update) {
                    self.last_update = Some(Instant::now());
                }
                let title = self.thread.title.clone();
                let prev_ctx = self.thread.context;
                // A steer arrives whole, and is a message of its own even right after another one.
                let at = if let Some(id) = update.pointer("/_meta/m4code/steer").and_then(Value::as_str).and_then(|id| id.parse().ok()) {
                    self.accept_steer(id)
                } else {
                    self.thread.apply(&update)
                };
                if let Some(at) = at {
                    if !self.background_tasks.is_empty() && update["sessionUpdate"] == "user_message_chunk"
                        && let Some(Entry::User(text)) = self.thread.entries.get(at)
                    {
                        let text = text.clone();
                        self.retire_finished_background_tasks(&text);
                    }
                    if let Some(Entry::Tool(call)) = self.thread.entries.get(at) {
                        self.tool_images.get_mut().retain(|(id, _), _| *id != call.id);
                        if self.harness == Harness::Antigravity && self.phase == Phase::Ready && !self.cached_replay
                            && (update.get("rawOutput").is_some() || update.get("content").is_some())
                        {
                            let started = (call.kind == "execute").then(|| background_task_from_tool(call)).flatten();
                            let texts: Vec<String> = tool_output_texts(call).into_iter().map(str::to_string).collect();
                            if let Some((task_id, label)) = started
                                && self.seen_background_tasks.insert(task_id.clone())
                            {
                                self.background_tasks.push(BackgroundToolTask { id: task_id, label });
                            }
                            for text in &texts {
                                self.retire_finished_background_tasks(text);
                            }
                        }
                    }
                    if self.phase == Phase::Ready && !self.cached_replay && is_conversation_update(&update) {
                        if matches!(self.thread.entries.get(at), Some(Entry::Thought(_))) {
                            self.thought_timing.entry(at).or_insert((Instant::now(), None));
                        } else {
                            self.finish_open_thoughts();
                        }
                    }
                    if self.harness == Harness::Codex && is_conversation_update(&update) {
                        self.asks.record_message_chunk(at, &update);
                    }
                    self.sync_markdown(at, cx);
                    self.remeasure(at);
                }
                if update.pointer("/_meta/m4code/backgroundShell").and_then(Value::as_bool) == Some(true) {
                    self.persist_entries();
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
                if self.interrupting || self.restarting_for_steer {
                    if let Some(connection) = &self.connection {
                        connection.respond(id, acp::permission_outcome(None));
                    }
                    return;
                }
                // m4code's own tools act only on what the user asked for, so their prompts are answered yes. The
                // request may leave the title out and name a call the chat already shows.
                let call = params.pointer("/toolCall/toolCallId").and_then(Value::as_str).and_then(|call| self.thread.tool_at(call));
                let shown = call.and_then(|at| match &self.thread.entries[at] { Entry::Tool(call) => Some(call.title.as_str()), _ => None });
                let title = params.pointer("/toolCall/title").and_then(Value::as_str).or(shown).unwrap_or_default();
                // A shell command or an edit is never one of m4code's tools, whatever its title says.
                let kind = params.pointer("/toolCall/kind").and_then(Value::as_str).unwrap_or_default();
                if !matches!(kind, "execute" | "edit" | "delete" | "move") && crate::mcp::is_own_tool(title) {
                    if let Some(connection) = &self.connection {
                        connection.respond(id, json!({"outcome": crate::harness::runner::permission(&params["options"], true)}));
                    }
                    return;
                }
                if let Some(edits) = self.subagent_edits && acp::is_tool_permission(&params) {
                    if let Some(connection) = &self.connection {
                        connection.respond(id, json!({"outcome": crate::harness::runner::permission(&params["options"], edits)}));
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
                let takes_own_answer = acp::takes_own_answer(&params);
                self.questions.push_back(Question { id: Uuid::new_v4(), reply: QuestionReply::Acp(id), title, options, highlight: 0, takes_own_answer });
            }
            Event::Elicitation { id, params } => {
                if self.interrupting || self.restarting_for_steer {
                    if let Some(connection) = &self.connection {
                        connection.respond(id, acp::elicitation_cancel());
                    }
                    return;
                }
                if self.harness == Harness::Codex && let Some(questions) = codex::async_questions(&params) {
                    // The adapter can only steer an answer into the original turn. Own the reply here so it can
                    // reach Codex as a follow-up even after that turn ends.
                    if let Some(connection) = &self.connection {
                        connection.respond(id, acp::elicitation_decline());
                    }
                    self.show_async_ask(questions, cx);
                    return;
                }
                let mut fields: VecDeque<acp::ElicitationField> = acp::parse_elicitation(&params).into();
                if let Some(first) = fields.pop_front() {
                    let options = first
                        .options
                        .into_iter()
                        .map(|opt| QuestionOption {
                            id: opt.value,
                            name: opt.label,
                            description: opt.description,
                            kind: "allow_once".into(),
                        })
                        .collect();
                    self.questions.push_back(Question {
                        id: Uuid::new_v4(),
                        reply: QuestionReply::Elicitation {
                            id,
                            field: first.field,
                            kind: first.kind,
                            answers: serde_json::Map::new(),
                            remaining: fields,
                        },
                        title: first.title,
                        options,
                        highlight: 0,
                        // A field's answer must be one of its schema's values.
                        takes_own_answer: false,
                    });
                } else if let Some(connection) = &self.connection {
                    connection.respond(id, acp::elicitation_decline());
                }
            }
            Event::Exited(words) => {
                let words = (!words.is_empty()).then_some(words);
                self.ended(words, cx);
                return;
            }
            Event::TurnEnded(result) => {
                self.capture_turn_review(result, cx);
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
        self.settle_opencode_shells();
        self.background_tasks.clear();
        self.preserve_handoff_message();
        self.cancel_questions();
        self.asks = Default::default();
        self.error = why;
        if let Some(item) = self.preparing.take() {
            self.queue.push_front(item);
        }
        let ids = self.steering.iter().map(|item| item.id).collect::<Vec<_>>();
        self.requeue_steers(ids);
        self.connection = None;
        cx.emit(ThreadEvent::Exited(if self.error.is_some() { Some(1) } else { Some(0) }));
        cx.notify();
    }

    fn retire_finished_background_tasks(&mut self, text: &str) {
        if self.background_tasks.is_empty() {
            return;
        }
        self.background_tasks.retain(|task| !text_retires_background_task(text, &task.id));
    }

    fn background_shell_action(&mut self, task_id: &str, stop: bool, cx: &mut Context<Self>) {
        let (Some(connection), Some(session)) = (&self.connection, &self.session_id) else { return };
        let Some(shell) = self.provider_shells.iter_mut().find(|shell| shell.id == task_id) else { return };
        if stop {
            if shell.stopping { return; }
            shell.stopping = true;
        } else {
            if self.shell_output.as_ref().is_some_and(|output| output.task_id == task_id && output.loading) {
                return;
            }
            self.shell_output = Some(ShellOutput { task_id: task_id.to_string(), text: "Loading output…".into(), loading: true });
        }
        let task_id = task_id.to_string();
        let action = if stop { "stop" } else { "output" };
        let reply = connection.request("_m4code/background_shell", json!({"sessionId": session, "taskId": task_id, "action": action}));
        cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |v, cx| {
                if stop {
                    if let Err(why) = result && let Some(shell) = v.provider_shells.iter_mut().find(|shell| shell.id == task_id) {
                        shell.stopping = false;
                        let at = v.thread.notice(format!("Could not stop background shell: {why}"), true);
                        v.remeasure(at);
                    }
                } else if let Some(output) = v.shell_output.as_mut().filter(|output| output.task_id == task_id) {
                    output.text = match result {
                        Ok(body) => match body["output"].as_str() {
                            Some("") => "The shell has produced no output yet.".into(),
                            Some(text) => text.to_string(),
                            None => "Live output is unavailable. Open the finished command to see its output.".into(),
                        },
                        Err(why) => why,
                    };
                    output.loading = false;
                }
                cx.notify();
            });
        }).detach();
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
        self.submit_with(false, window, cx);
    }

    /// Send what is typed; `the_other_way` (Cmd/Ctrl+Enter) queues it when the setting steers, and steers when it
    /// queues.
    fn submit_with(&mut self, the_other_way: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.queue_edit.is_some() {
            self.finish_queue_edit(true, window, cx);
            return;
        }
        if self.async_answer_blocked() {
            return;
        }
        let text = self.composer.read(cx).value().trim().to_string();
        if !self.can_submit_draft(cx) {
            if self.queue_held && !self.working && self.questions.is_empty() {
                self.resume_queue(cx);
            }
            return;
        }
        let has_attachments = self.attachments_enabled() && self.attachments.has_files();
        let text = if has_attachments {
            let Some(message) = self.attachments.message(&text) else {
                cx.notify();
                return;
            };
            self.attachments = Default::default();
            message
        } else {
            text
        };
        self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        if self.questions.front().is_some_and(|q| q.takes_own_answer) {
            self.answer_in_own_words(&text, cx);
            return;
        }
        if let Some(id) = self.async_answer_id() {
            self.answer_async_ask(id, Some(text), cx);
            return;
        }
        self.history_at = None;
        self.picker = None;
        if !has_attachments && self.try_run_builtin_command(&text, cx) {
            self.sync_command_highlights(cx);
            return;
        }
        let follow_up = if the_other_way { self.follow_up.other() } else { self.follow_up };
        self.send_as(text, follow_up, cx);
        self.sync_command_highlights(cx);
    }

    /// Handle m4code's own built-in slash commands (`/new`, `/clear`, `/compact`).
    /// Returns `true` when the command was handled here and should not be sent as an ordinary chat message.
    fn try_run_builtin_command(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let trimmed = text.trim();
        let Some(rest) = trimmed.strip_prefix('/') else { return false };
        let (cmd, arg) = match rest.split_once(char::is_whitespace) {
            Some((c, a)) => (c, Some(a.trim()).filter(|s| !s.is_empty())),
            None => (rest, None),
        };
        match cmd {
            "new" => {
                cx.emit(ThreadEvent::NewSession(arg.map(String::from)));
                cx.notify();
                true
            }
            "clear" => {
                if self.working {
                    self.interrupt(cx);
                }
                cx.emit(ThreadEvent::ClearSession);
                cx.notify();
                true
            }
            "compact" => {
                if self.compaction.running {
                    let at = self.thread.notice("Context compaction is already running.", false);
                    self.remeasure(at);
                    cx.notify();
                    return true;
                }
                let native = self.harness.adapter().compact_command.filter(|name| self.thread.commands.iter().any(|c| c.name == *name));
                if let Some(native_cmd) = native {
                    let prompt = match arg {
                        Some(extra) => format!("/{native_cmd} {extra}"),
                        None => format!("/{native_cmd}"),
                    };
                    let at = self.thread.notice("Compacting context…", false);
                    self.remeasure(at);
                    self.compaction.running = true;
                    self.queue.push_front(Queued { shown: true, compact: true, ..Queued::new(prompt, true) });
                    self.queue_changed();
                    if self.queue_held && !self.working {
                        self.resume_queue(cx);
                    } else {
                        self.send_next(cx);
                    }
                    cx.notify();
                    return true;
                }
                let transcript = self.thread.transcript_after(0, 400);
                if transcript.trim().is_empty() {
                    let at = self.thread.notice("Nothing to compact yet.", false);
                    self.remeasure(at);
                    cx.notify();
                    return true;
                }
                let at = self.thread.notice("Compacting context…", false);
                self.remeasure(at);
                self.compaction.running = true;
                cx.emit(ThreadEvent::Compact { transcript, instructions: arg.map(String::from) });
                cx.notify();
                true
            }
            _ if self.harness.hides_command(cmd) && self.thread.commands.iter().any(|command| command.name == cmd) => {
                let at = self.thread.notice(format!("Use the m4code controls for /{cmd}."), false);
                self.remeasure(at);
                cx.notify();
                true
            }
            _ => false,
        }
    }

    /// Whether `text` goes to the turn that is running. A slash command waits, as t3code's do, because a command
    /// steered into a turn would replace the agent's task; so does a message while the context is compacted.
    fn can_steer(&self, text: &str, follow_up: crate::store::FollowUp) -> bool {
        follow_up == crate::store::FollowUp::Steer
            && self.working
            && !self.queue_held
            && self.preparing.is_none()
            && self.handoff_after_turn.is_none()
            && self.questions.is_empty()
            && self.phase == Phase::Ready
            && !self.compaction.running
            && !self.running_command
            && !text.starts_with('/')
    }

    fn native_steering(&self) -> bool {
        matches!(self.harness, Harness::Codex | Harness::OpenCode)
            && self.agent_meta.pointer("/steering/supported").and_then(Value::as_bool) == Some(true)
    }

    fn native_codex_steering(&self) -> bool {
        self.harness == Harness::Codex && self.native_steering()
    }

    fn steer(&mut self, mut item: Queued, cx: &mut Context<Self>) {
        let (Some(connection), Some(session)) = (self.connection.clone(), self.session_id.clone()) else {
            self.queue.push_back(item);
            self.queue_changed();
            cx.notify();
            return;
        };
        let native = self.native_steering();
        if !self.harness.adapter().steers && !native {
            let at = self.queue.iter().take_while(|q| q.ahead).count();
            item.ahead = true;
            self.queue.insert(at, item);
            self.queue_changed();
            if !std::mem::replace(&mut self.restarting_for_steer, true) {
                connection.notify("session/cancel", json!({"sessionId": session}));
                self.cancel_questions();
            }
            cx.notify();
            return;
        }
        let id = item.id;
        let text = item.text.clone();
        self.steering.push_back(item);
        self.queue_changed();
        let reply = if native {
            // Require a separate prompt to start a turn. Otherwise an idle adapter could begin
            // work without a matching in-flight request or a checkpoint.
            connection.request("_session/steering", json!({
                "sessionId": session,
                "prompt": [{"type": "text", "text": text}],
                "_meta": {"steering": {"idleBehavior": "promptRequired"}}
            }))
        } else {
            connection.request("_m4code/steer", json!({"sessionId": session, "text": text, "id": id.to_string()}))
        };
        cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |v, cx| {
                if native && result.as_ref().is_ok_and(|r| r["outcome"] == "injected") {
                    if let Some(at) = v.accept_steer(id) {
                        v.remeasure(at);
                        v.persist_entries();
                    }
                } else if native || result.is_err() {
                    v.requeue_steers([id]);
                }
                v.send_next(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn accept_steer(&mut self, id: Uuid) -> Option<usize> {
        let at = self.steering.iter().position(|item| item.id == id)?;
        let item = self.steering.remove(at)?;
        self.queue_changed();
        self.thread.sent(item.text)
    }

    fn requeue_steers(&mut self, ids: impl IntoIterator<Item = Uuid>) {
        for id in ids {
            if let Some(at) = self.steering.iter().position(|item| item.id == id) {
                let item = self.steering.remove(at).unwrap();
                let at = self.queue.iter().position(|waiting| !waiting.ahead && (item.ahead || waiting.order > item.order))
                    .unwrap_or(self.queue.len());
                self.queue.insert(at, item);
            }
        }
        self.queue_changed();
    }

    /// A message m4code sends on the session's behalf, such as a subagent's report. It waits for the current turn
    /// like the user's own, but never starts or joins a hand-off the user set up in the model picker.
    pub fn deliver(&mut self, text: String, cx: &mut Context<Self>) {
        self.queue.push_back(Queued::new(text, false));
        self.send_next(cx);
        cx.notify();
    }

    /// Send `text` as the user's next message, now or after the turn that is running.
    pub fn send(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.send_as(text, self.follow_up, cx);
    }

    fn send_as(&mut self, text: String, follow_up: crate::store::FollowUp, cx: &mut Context<Self>) {
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
        if self.can_steer(&text, follow_up) {
            self.steer(Queued::new(text, true), cx);
            return;
        }
        let mut item = Queued::new(text, true);
        if self.phase == Phase::Starting
            && !self.working
            && !self.queue_held
            && self.questions.is_empty()
            && !self.queue.iter().any(|q| q.startup)
        {
            item.startup = true;
            if let Some(at) = self.thread.sent(item.text.clone()) {
                item.shown = true;
                self.sync_markdown(at, cx);
                self.remeasure(at);
                self.persist_entries();
                if self.list.is_following_tail() {
                    self.list.scroll_to_end();
                }
            }
        }
        self.queue.push_back(item);
        self.queue_changed();
        if self.queue_held && !self.working {
            self.resume_queue(cx);
        } else {
            self.send_next(cx);
        }
        cx.notify();
    }

    fn send_next(&mut self, cx: &mut Context<Self>) {
        if self.working || self.phase != Phase::Ready || !self.steering.is_empty() || !self.questions.is_empty() || self.restoring_checkpoint() {
            return;
        }
        let (Some(connection), Some(session), Some(events)) = (self.connection.clone(), self.session_id.clone(), self.events.clone()) else { return };
        let command = self.harness.adapter().compact_command.filter(|name| self.thread.commands.iter().any(|c| c.name == *name));
        let mut next = if !self.queue_held && self.compaction.take_due(self.thread.context, command.is_some()) {
            let at = self.thread.notice("Compacting context…", false);
            self.remeasure(at);
            Queued { shown: true, compact: true, ..Queued::new(format!("/{}", command.unwrap()), false) }
        } else {
            let Some(next) = self.queue.front() else { return };
            if (self.queue_held && !next.startup) || self.queue_edit.as_ref().is_some_and(|edit| edit.id == next.id) {
                return;
            }
            self.queue.pop_front().unwrap()
        };
        let id = next.id;
        let compact = next.compact;
        let seed_history = (!compact
            && matches!(self.thread.entries.last(), Some(Entry::Notice { text, error: false }) if text == FORK_SEED))
            .then(|| self.thread.transcript(400));
        let mut shown_row = if next.shown {
            self.thread.entries.len().checked_sub(1)
                .filter(|&row| matches!(&self.thread.entries[row], Entry::User(text) if text == &next.text))
        } else {
            None
        };
        if !next.shown && !compact && let Some(at) = self.thread.sent(next.text.clone()) {
            next.shown = true;
            shown_row = Some(at);
            self.cached_replay = false;
            self.sync_markdown(at, cx);
            self.remeasure(at);
            self.persist_entries();
            if self.list.is_following_tail() {
                self.list.scroll_to_end();
            }
        }
        self.preparing = Some(next);
        self.working = true;
        self.changed = None;
        self.queue_changed();
        cx.notify();
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let before_cwd = cwd.clone();
            let snapshot = if !compact {
                let session = session.clone();
                cx.background_executor().spawn(async move { crate::git::snapshot::take(&before_cwd, &session) }).await.ok()
            } else {
                None
            };
            let prompt = this.update(cx, |v, cx| {
                let next = v.take_prepared(id)?;
                if let Some(snapshot) = &snapshot {
                    v.snapshots.push(snapshot.clone());
                }
                let outgoing = match &seed_history {
                    Some(history) => crate::harness::thread::handoff_prompt(v.harness.name(), history, &next.text),
                    None => next.text.clone(),
                };
                if !next.shown && !next.compact && let Some(at) = v.thread.sent(next.text.clone()) {
                    v.sync_markdown(at, cx);
                    v.remeasure(at);
                }
                v.cached_replay = false;
                v.running_command = next.text.starts_with('/');
                v.persist_entries();
                if v.list.is_following_tail() {
                    v.list.scroll_to_end();
                }
                v.active_turn_review = snapshot.zip(shown_row.or_else(|| v.thread.entries.len().checked_sub(1))).and_then(|(before, row)| {
                    match v.thread.entries.get(row) {
                        Some(Entry::User(text)) if !v.record_id.is_empty() => Some(review::ActiveTurn {
                            id, row, before, text: text.clone(), cwd: cwd.clone(),
                        }),
                        _ => None,
                    }
                });
                let request = connection.request("session/prompt", json!({"sessionId": session, "prompt": [{"type": "text", "text": outgoing}]}));
                v.queue_changed();
                cx.notify();
                Some(request)
            });
            if let Ok(Some(prompt)) = prompt {
                let _ = events.unbounded_send(Event::TurnEnded(prompt.await));
            }
        })
        .detach();
    }

    fn take_prepared(&mut self, id: Uuid) -> Option<Queued> {
        if self.phase != Phase::Ready {
            return None;
        }
        self.preparing.take_if(|next| next.id == id)
    }

    fn finish_open_thoughts(&mut self) {
        for (started, done) in self.thought_timing.values_mut() {
            if done.is_none() {
                *done = Some(started.elapsed());
            }
        }
    }

    // OpenCode's shell calls belong to the prompt. A missing final tool update leaves no exit status.
    fn settle_opencode_shells(&mut self) {
        if self.harness != Harness::OpenCode {
            return;
        }
        let mut first_changed = None;
        for (row, entry) in self.thread.entries.iter_mut().enumerate() {
            if let Entry::Tool(call) = entry
                && call.kind == "execute" && call.status == ToolStatus::Running
            {
                call.status = match call.details.exit_code {
                    Some(0) => ToolStatus::Done,
                    Some(_) => ToolStatus::Failed,
                    None => ToolStatus::Ended,
                };
                first_changed.get_or_insert(row);
            }
        }
        if let Some(row) = first_changed {
            self.remeasure(row);
        }
    }

    fn turn_ended(&mut self, result: Result<Value, String>, cx: &mut Context<Self>) {
        self.settle_opencode_shells();
        self.working = false;
        self.interrupting = false;
        self.last_update = None;
        self.finish_open_thoughts();
        if let Ok(r) = &result
            && let Some(turn) = self.thread.record_usage(&r["usage"])
        {
            cx.emit(ThreadEvent::Usage(turn));
        }
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
        self.cancel_questions();
        self.asks.turn_ended();
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
        let restarting = std::mem::take(&mut self.restarting_for_steer);
        if outcome == "failed" || (outcome == "interrupted" && !restarting) {
            self.queue_held = true;
        }
        if self.harness.adapter().steers {
            let ids = self.steering.iter().map(|item| item.id).collect::<Vec<_>>();
            self.requeue_steers(ids);
        }
        self.persist_entries();
        if !self.queue_held && let Some((target, effort, message, by_agent)) = self.handoff_after_turn.take() {
            let model = (!target.id.is_empty()).then_some(target.id);
            cx.emit(ThreadEvent::Handoff { to: target.harness, model, message, effort, by_agent });
            cx.notify();
            return;
        }
        // Decided before `send_next`, which may start an automatic compaction: the turn's work is done all the same,
        // and its report is the reply above, not what the compaction says.
        let idle = (self.queue.is_empty() || self.queue_held) && !compacted;
        self.send_next(cx);
        if idle {
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
        self.queue_held = true;
        self.restarting_for_steer = false;
        if !self.interrupting {
            cx.emit(ThreadEvent::AbortChildren);
        }
        if let Some(mut item) = self.preparing.take() {
            item.startup = false;
            if item.compact {
                self.compaction.running = false;
            } else {
                if item.shown && matches!(self.thread.entries.last(), Some(Entry::User(text)) if text == &item.text) {
                    let at = self.thread.entries.len() - 1;
                    self.thread.entries.pop();
                    self.markdown.remove(&at);
                    self.user_md_len.remove(&at);
                    self.agent_cache.remove(&at);
                    item.shown = false;
                    self.remeasure(at);
                }
                self.queue.push_front(item);
            }
            self.working = false;
            self.queue_changed();
            self.cancel_questions();
            cx.notify();
            return;
        }
        if self.phase == Phase::Starting
            && let Some(pos) = self.queue.iter().position(|item| item.startup)
        {
            let mut item = self.queue.remove(pos).unwrap();
            item.startup = false;
            if item.shown && matches!(self.thread.entries.last(), Some(Entry::User(text)) if text == &item.text) {
                let at = self.thread.entries.len() - 1;
                self.thread.entries.pop();
                self.markdown.remove(&at);
                self.user_md_len.remove(&at);
                self.agent_cache.remove(&at);
                item.shown = false;
                self.remeasure(at);
            }
            self.queue.push_front(item);
            self.queue_changed();
            self.cancel_questions();
            cx.notify();
            return;
        }
        self.cancel_questions();
        self.last_update = None;
        if !self.interrupting && let (Some(c), Some(s)) = (&self.connection, &self.session_id) {
            if self.working {
                self.interrupting = true;
            }
            c.notify("session/cancel", json!({"sessionId": s}));
        }
        cx.notify();
    }

    /// Show a multiple-choice question from m4code's MCP `ask_user` tool (works on every harness, including ones
    /// whose native question tool isn't bridged over ACP).
    pub fn ask_bridge_question(&mut self, req_id: String, title: String, options: Vec<QuestionOption>, cx: &mut Context<Self>) {
        if self.interrupting || self.restarting_for_steer {
            crate::mcp::bridge::respond(&req_id, false, "The turn was interrupted.");
            return;
        }
        self.questions.push_back(Question { id: Uuid::new_v4(), reply: QuestionReply::Bridge(req_id), title, options, highlight: 0, takes_own_answer: true });
        cx.notify();
    }

    fn cancel_questions(&mut self) {
        for question in self.questions.drain(..) {
            match question.reply {
                QuestionReply::Bridge(id) => crate::mcp::bridge::respond(&id, false, "The user dismissed the question."),
                QuestionReply::Acp(id) => {
                    if let Some(c) = &self.connection {
                        c.respond(id, acp::permission_outcome(None));
                    }
                }
                QuestionReply::Elicitation { id, .. } => {
                    if let Some(c) = &self.connection {
                        c.respond(id, acp::elicitation_cancel());
                    }
                }
            }
        }
    }

    pub(crate) fn answer_question(&mut self, id: Uuid, choice: Option<usize>, cx: &mut Context<Self>) -> bool {
        if self.questions.front().is_some_and(|question| question.id == id) {
            match choice {
                Some(ix) => self.answer(Some(ix), cx),
                None => self.decline(cx),
            }
            return true;
        }
        if self.async_ask().is_some_and(|ask| ask.id == id) {
            match choice {
                Some(ix) => self.choose_async_ask(id, ix, cx),
                None => self.answer_async_ask(id, None, cx),
            }
            return true;
        }
        false
    }

    /// Answer the open permission/ask question with option `index` (from 0), or turn it down.
    pub fn answer(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let Some(q) = self.questions.pop_front() else { return };
        let chosen = index.and_then(|i| q.options.get(i));
        match q.reply {
            QuestionReply::Acp(id) => {
                let option = chosen.map(|o| o.id.as_str());
                if let Some(c) = &self.connection {
                    c.respond(id, acp::permission_outcome(option));
                }
            }
            QuestionReply::Elicitation { id, field, kind, mut answers, mut remaining } => {
                if let Some(opt) = chosen {
                    answers.insert(field, kind.to_json_value(&opt.id));
                    if let Some(next) = remaining.pop_front() {
                        let options = next
                            .options
                            .into_iter()
                            .map(|o| QuestionOption { id: o.value, name: o.label, description: o.description, kind: "allow_once".into() })
                            .collect();
                        self.questions.push_front(Question {
                            id: Uuid::new_v4(),
                            reply: QuestionReply::Elicitation { id, field: next.field, kind: next.kind, answers, remaining },
                            title: next.title,
                            options,
                            highlight: 0,
                            takes_own_answer: false,
                        });
                    } else if let Some(c) = &self.connection {
                        c.respond(id, acp::elicitation_accept(answers));
                    }
                } else if let Some(c) = &self.connection {
                    c.respond(id, acp::elicitation_cancel());
                }
            }
            QuestionReply::Bridge(req_id) => match chosen {
                Some(opt) => crate::mcp::bridge::respond(&req_id, true, &opt.name),
                None => crate::mcp::bridge::respond(&req_id, false, "The user dismissed the question."),
            },
        }
        self.send_next(cx);
        cx.notify();
    }

    /// Answer the open question with what the user typed rather than one of its options.
    fn answer_in_own_words(&mut self, text: &str, cx: &mut Context<Self>) {
        if !self.questions.front().is_some_and(|q| q.takes_own_answer) { return; }
        let Some(q) = self.questions.pop_front() else { return };
        match q.reply {
            QuestionReply::Acp(id) => {
                if let Some(c) = &self.connection {
                    c.respond(id, acp::own_answer(text));
                }
            }
            QuestionReply::Bridge(req_id) => crate::mcp::bridge::respond(&req_id, true, text),
            // Never marked as taking the user's own words (`takes_own_answer`).
            QuestionReply::Elicitation { .. } => {}
        }
        self.send_next(cx);
        cx.notify();
    }

    /// Move the open question's highlight to option `index`, as the mouse does.
    pub(super) fn highlight_option(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(q) = self.questions.front_mut()
            && q.highlight != index
        {
            q.highlight = index;
            cx.notify();
        }
    }

    /// Turn the question down with its own reject option when it has one.
    pub fn decline(&mut self, cx: &mut Context<Self>) {
        let reject = self.questions.front().and_then(|q| q.options.iter().position(|o| o.kind == "reject_once"));
        self.answer(reject, cx);
    }

    /// `wanted` when the session lists it. Otherwise, for one of the four modes, the nearest safer one it lists: the
    /// mode came from the cached list, and an account can have fewer modes than that (Claude's Auto).
    fn listed_or_safer(&self, wanted: &str) -> Option<String> {
        if self.thread.modes.iter().any(|(id, _)| id == wanted) {
            return Some(wanted.to_string());
        }
        let offered = permission::offered(self.harness, &self.thread.modes);
        Permission::of(self.harness, wanted)?.or_safer(&offered)?.native(self.harness).map(String::from)
    }

    /// The universal modes this session offers, each with its harness's mode id, least access first.
    pub(crate) fn permissions(&self) -> Vec<(Permission, &'static str)> {
        permission::offered(self.harness, &self.thread.modes).into_iter().filter_map(|p| Some((p, p.native(self.harness)?))).collect()
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
        if self.chat_menu.is_some() && ev.keystroke.key == "escape" {
            self.chat_menu = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.screenshot.is_some() && self.screenshot_focus.contains_focused(window, cx) && ev.keystroke.key == "escape" {
            self.screenshot = None;
            self.focus_composer(window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let modifiers = ev.keystroke.modifiers;
        if (self.composer.focus_handle(cx).contains_focused(window, cx) || window.focused(cx).is_none())
            && self.comment_anchor.is_some() && self.selection_comment.is_none() && self.review.is_none()
            && self.picker.is_none() && self.find.is_none() && self.screenshot.is_none()
            && ev.keystroke.key.eq_ignore_ascii_case("m") && modifiers.shift && !modifiers.alt
            && if cfg!(target_os = "macos") { modifiers.platform } else { modifiers.control }
        {
            self.start_selection_comment(window, cx);
            cx.stop_propagation();
            return;
        }
        if self.intercept_find(&ev.keystroke, window, cx) {
            cx.stop_propagation();
            return;
        }
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
                        crate::panels::picker::reveal_row(&self.model_scroll, &self.model_rows.borrow(), self.model_highlight);
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
                    let count = self.permissions().len();
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
                    let offered = self.permissions();
                    if let Some((_, mode_id)) = offered.get(self.mode_highlight.min(offered.len().saturating_sub(1))) {
                        self.set_mode(mode_id.to_string(), cx);
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
            working: self.can_interrupt(),
            question: self.questions.front().map_or(0, |q| q.options.len()),
            draft_empty: !self.can_submit_draft(cx) || self.async_answer_blocked(),
            in_history: self.history_at.is_some(),
            escape_just_pressed: self.last_escape.is_some_and(|t| t.elapsed() < keys::DOUBLE_PRESS),
            completing: self.completion(&draft, self.composer.read(cx).selected_range().end).is_some(),
            commands: self.matching_commands(cx).len(),
        };
        let is_escape = ev.keystroke.key == "escape";
        if is_escape && self.queue_edit.is_some() && self.questions.is_empty() && self.picker.is_none() {
            self.finish_queue_edit(false, window, cx);
            cx.stop_propagation();
            return;
        }
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
            keys::Key::ChooseHighlighted => {
                let at = self.questions.front().map(|q| q.highlight);
                self.answer(at, cx);
            }
            keys::Key::OptionUp | keys::Key::OptionDown => {
                if let Some(q) = self.questions.front_mut() {
                    let last = q.options.len().saturating_sub(1);
                    q.highlight = match (key == keys::Key::OptionDown, q.highlight.min(last)) {
                        (true, at) if at >= last => 0,
                        (true, at) => at + 1,
                        (false, 0) => last,
                        (false, at) => at - 1,
                    };
                }
            }
            keys::Key::EditLast => {
                if let Some(last) = self.thread.sent_texts().pop() {
                    self.set_draft(&last, window, cx);
                }
            }
            keys::Key::Clear => {
                self.history_at = None;
                if self.attachments_enabled() {
                    self.attachments = Default::default();
                }
                self.set_draft("", window, cx);
            }
            keys::Key::CycleMode => {
                if let Some(next) = permission::next(self.harness, &self.thread.modes, self.thread.mode.as_deref()) {
                    self.set_mode(next.to_string(), cx);
                }
            }
            keys::Key::Older | keys::Key::Newer => self.walk_history(key == keys::Key::Older, &draft, window, cx),
            keys::Key::Complete => {
                let cursor = self.composer.read(cx).selected_range().end;
                if let Some(name) = self.highlighted_command(cx).or_else(|| self.completion(&draft, cursor)) {
                    self.replace_active_command(&name, true, window, cx);
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
                    let cursor = self.composer.read(cx).selected_range().end;
                    let standalone = active_slash_token(&draft, cursor)
                        .is_some_and(|(start, end, _)| start == 0 && draft[end..].trim().is_empty());
                    if standalone {
                        self.replace_active_command(&name, false, window, cx);
                        self.submit(window, cx);
                    } else {
                        self.replace_active_command(&name, true, window, cx);
                    }
                }
            }
            keys::Key::ToggleDetail => {
                self.detail = !self.detail;
                self.collapsed_shell_calls.clear();
                self.collapsed_thoughts.clear();
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
        let values: Vec<String> = crate::harness::defaults::Effort::ALL.iter().map(|e| e.name().to_string()).collect();
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

    /// The slash command a typed `/` token completes to: the first one it starts.
    fn completion(&self, draft: &str, cursor: usize) -> Option<String> {
        if self.asks.composing_answer() {
            return None;
        }
        let (_, _, typed) = active_slash_token(draft, cursor)?;
        self.shown_commands()
            .into_iter()
            .map(|c| c.name)
            .find(|n| n.starts_with(typed) && n.as_str() != typed)
    }

    /// The commands an active `/` token matches (at start of draft or after whitespace), for the menu over the composer.
    pub(super) fn matching_commands(&self, cx: &App) -> Vec<crate::harness::thread::Command> {
        let input = self.composer.read(cx);
        let draft = input.value();
        let cursor = input.selected_range().end;
        // What is typed answers the open question, so it is never a command to complete.
        if self.queue_edit.is_some() || self.questions.front().is_some_and(|q| q.takes_own_answer) || self.asks.composing_answer() {
            return vec![];
        }
        let Some((_, _, typed)) = active_slash_token(draft.as_ref(), cursor) else { return vec![] };
        self.shown_commands().into_iter().filter(|c| c.name.starts_with(typed)).collect()
    }

    /// m4code's built-in commands (`/compact`, `/new`, `/clear`) followed by the agent's own commands, deduplicated.
    pub(super) fn shown_commands(&self) -> Vec<crate::harness::thread::Command> {
        let mut out: Vec<crate::harness::thread::Command> = BUILTIN_COMMANDS
            .iter()
            .map(|(name, description)| crate::harness::thread::Command {
                name: (*name).to_string(),
                description: (*description).to_string(),
            })
            .collect();
        for c in &self.thread.commands {
            if self.harness.hides_command(&c.name) || out.iter().any(|existing| existing.name == c.name) {
                continue;
            }
            out.push(c.clone());
        }
        out
    }

    fn highlighted_command(&self, cx: &App) -> Option<String> {
        let commands = self.matching_commands(cx);
        commands.get(self.command_highlight.min(commands.len().saturating_sub(1))).map(|c| c.name.clone())
    }

    /// Replace the active `/...` token around the cursor with `/{name}` (optionally followed by a space).
    pub(super) fn replace_active_command(&mut self, name: &str, trailing_space: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (draft, cursor) = {
            let c = self.composer.read(cx);
            (c.value().to_string(), c.selected_range().end)
        };
        let replacement = if trailing_space { format!("/{name} ") } else { format!("/{name}") };
        let (new_text, new_cursor) = if let Some((start, end, _)) = active_slash_token(&draft, cursor) {
            let text = format!("{}{}{}", &draft[..start], replacement, &draft[end..]);
            (text, start + replacement.len())
        } else {
            let len = replacement.len();
            (replacement, len)
        };
        self.composer.update(cx, |c, cx| {
            c.set_value(new_text, window, cx);
            c.set_selected_range(new_cursor..new_cursor, cx);
        });
        self.sync_command_highlights(cx);
    }

    /// Highlight recognized `/command` tokens directly inside the composer textarea.
    pub(super) fn sync_command_highlights(&self, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        let commands = self.shown_commands();
        let accent = crate::panels::theme::hsla(crate::panels::theme::CODEX);
        let mut decorations = Vec::new();
        for (range, name) in slash_command_spans(&draft) {
            let exact = commands.iter().any(|c| c.name == name);
            let prefix = !exact && commands.iter().any(|c| c.name.starts_with(name));
            if exact || prefix {
                decorations.push(gpui_kit::base::input::TextDecoration::new(
                    range,
                    gpui::HighlightStyle {
                        color: Some(accent),
                        background_color: Some(accent.opacity(if exact { 0.18 } else { 0.10 })),
                        ..Default::default()
                    },
                ).with_background_padding(px(3.0)));
            }
        }
        self.composer.update(cx, |c, cx| c.set_decorations(decorations, cx));
    }

    pub(crate) fn set_viewing(&mut self, viewing: bool, cx: &mut Context<Self>) {
        if viewing == self.viewing {
            return;
        }
        self.viewing = viewing;
        cx.notify();
    }

    fn selected_quote(&self, cx: &App) -> String {
        let mut selected = self.markdown.iter().filter_map(|(row, state)| {
            let text = state.read(cx).selected_text();
            (!text.trim().is_empty()).then_some((*row, text))
        }).collect::<Vec<_>>();
        selected.sort_by_key(|(row, _)| *row);
        selected.into_iter().map(|(_, text)| text.lines()
            .map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n"))
            .collect::<Vec<_>>().join("\n>\n")
    }

    fn start_selection_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, quote)) = self.comment_anchor.as_mut() else { return };
        let quote = std::mem::take(quote);
        let input = crate::panels::comment_box::comment_input("the selection", window, cx);
        let subscription = cx.subscribe_in(&input, window, |v, input, event, window, cx| {
            if let InputEvent::PressEnter { secondary, .. } = event {
                let text = input.read(cx).value().to_string();
                if text.trim().is_empty() {
                    return;
                }
                if let Some(comment) = v.selection_comment.take() {
                    let message = format!("{}\n\n{}", comment.quote, text.trim());
                    v.dismiss_selection_comment(window, cx);
                    if *secondary {
                        v.send(message, window, cx);
                    } else {
                        v.add_comment(&message, window, cx);
                    }
                }
            }
        });
        self.selection_comment = Some(SelectionComment { quote, input, _subscription: subscription });
        gpui_kit::base::TextSelection::clear(window, cx);
        cx.notify();
    }

    pub fn clear_comment_anchor(&mut self, cx: &mut Context<Self>) {
        if self.selection_comment.is_none() && self.comment_anchor.take().is_some() {
            cx.notify();
        }
    }

    fn dismiss_selection_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selection_comment = None;
        self.comment_anchor = None;
        self.focus_composer(window, cx);
        cx.notify();
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

    fn set_draft(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        let end = text.len();
        self.composer.update(cx, |c, cx| {
            c.set_value(text, window, cx);
            c.set_selected_range(end..end, cx);
        });
        self.sync_command_highlights(cx);
    }

    fn persist_entries(&self) {
        crate::harness::thread::save_entries(&self.record_id, &self.thread.entries);
    }

    /// Start or update the cached markdown parser for entry `ix` as soon as its text arrives, instead of waiting
    /// for the next paint frame.
    fn sync_markdown(&mut self, ix: usize, cx: &mut Context<Self>) {
        match self.thread.entries.get(ix) {
            Some(Entry::User(text)) if crate::harness::thread::parse_subagent_report(text).is_none() => {
                let formatted = render::keep_line_breaks(text);
                render::ensure_markdown_state(&mut self.markdown, ix, &formatted, cx);
            }
            Some(Entry::Agent(text)) => {
                let text = text.clone();
                render::ensure_markdown_state(&mut self.markdown, ix, &text, cx);
            }
            _ => {}
        }
    }

    /// Measure entry `from` and every one after it again; the list keeps its place above them.
    pub(crate) fn remeasure(&mut self, from: usize) {
        let rows = self.thread.entries.len();
        let from = self.group_start_before(from).min(from).min(self.rows).min(rows);
        let shared = self.rows.min(rows);
        if rows > self.rows {
            self.list.splice(self.rows..self.rows, rows - self.rows);
        } else if rows < self.rows {
            self.list.splice(rows..self.rows, 0);
        }
        if from < shared {
            self.list.remeasure_items(from..shared);
        }
        self.rows = rows;
        self.refresh_find(from);
    }

    pub(super) fn show_jump_to_latest(&self) -> bool {
        !self.list.is_following_tail()
            && self.list.max_offset_for_scrollbar().y > px(1.0)
            && self.list.is_scrolled_to_end() != Some(true)
    }

    fn open_location(&mut self, path: PathBuf, line: Option<u32>, cx: &mut Context<Self>) {
        let normalized = path
            .to_str()
            .filter(|s| s.starts_with("file:"))
            .and_then(|s| url::Url::parse(s).ok())
            .and_then(|u| u.to_file_path().ok())
            .unwrap_or(path);
        let path = if normalized.is_absolute() { normalized } else { self.cwd.join(normalized) };
        cx.emit(ThreadEvent::OpenPath(path.to_string_lossy().into_owned(), line.map(|l| l as usize)));
    }

    // What the workspace asks of a session view, as it asks a terminal.

    pub fn is_running(&self) -> bool {
        self.phase != Phase::Ended
    }

    pub fn set_compaction_policy(&mut self, enabled: bool, threshold_percent: u64, cx: &mut Context<Self>) {
        self.compaction.enabled = enabled;
        self.compaction.threshold_percent = crate::harness::compaction::clamp_threshold(threshold_percent);
        if let Some((used, size)) = self.thread.context {
            self.compaction.observe(used, size);
        }
        self.send_next(cx);
        cx.notify();
    }

    pub fn compaction_status(&self, has_persisted_context: bool) -> String {
        let Some(command) = self.harness.adapter().compact_command else { return "Unsupported".into() };
        if !self.thread.commands.iter().any(|c| c.name == command) {
            return "Unavailable".into();
        }
        let threshold = crate::harness::compaction::clamp_threshold(self.compaction.threshold_percent);
        if !self.compaction.native_controlled {
            return if self.compaction.enabled { format!("{threshold}% (fallback)") } else { "Off (fallback)".into() };
        }
        if !self.compaction.enabled { return "Off".into(); }
        if self.thread.context.is_none() && !has_persisted_context { return "Waiting…".into(); }
        format!("{threshold}%")
    }

    pub fn stop(&mut self) {
        self.preserve_handoff_message();
        self.cancel_questions();
        self.asks = Default::default();
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

    /// A turn is actively running, or an optimistic startup prompt is waiting to run as soon as the harness opens.
    pub(super) fn has_active_turn(&self) -> bool {
        self.working || (self.phase == Phase::Starting && self.queue.iter().any(|item| item.startup))
    }

    /// Whether the user can interrupt the agent right now: an active turn, an optimistic startup prompt, or a live
    /// continuation streaming updates after a turn ended.
    pub(super) fn can_interrupt(&self) -> bool {
        self.is_working()
    }

    pub fn is_working(&self) -> bool {
        self.has_active_turn()
            || (self.phase == Phase::Starting && !self.queue.is_empty())
            || (self.phase == Phase::Ready
                && !self.cached_replay
                && self.last_update.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(15)))
    }

    /// Work the agent left running when its turn ended: a background shell, a subagent, or a tool call still out.
    /// The agent usually wakes again when it finishes, so its turn ending isn't the session being done.
    pub fn has_background_work(&self) -> bool {
        !self.provider_shells.is_empty()
            || !self.background_tasks.is_empty()
            || !self.background.is_empty()
            || self.thread.entries.iter().any(|e| matches!(e, Entry::Tool(call) if call.status == ToolStatus::Running))
    }

    /// Waiting for the user to answer a permission or ask question (including async Codex asks).
    pub fn is_waiting(&self) -> bool {
        !self.questions.is_empty() || !self.asks.is_empty()
    }

    /// Snapshot of the pending question, if any, so the sidebar popover can render and answer it.
    pub fn pending_question(&self) -> Option<PendingQuestion> {
        self.questions
            .front()
            .map(|q| PendingQuestion { id: q.id, title: q.title.clone(), options: q.options.clone() })
            .or_else(|| {
                let ask = self.async_ask()?;
                let options = ask
                    .question
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, name)| QuestionOption {
                        id: format!("async-{i}"),
                        name: name.clone(),
                        description: None,
                        kind: "allow_once".into(),
                    })
                    .collect();
                Some(PendingQuestion { id: ask.id, title: ask.question.title.clone(), options })
            })
    }

    pub fn pending_question_title(&self) -> Option<String> {
        self.questions
            .front()
            .map(|q| q.title.clone())
            .or_else(|| self.async_ask().map(|ask| ask.question.title.clone()))
    }

    /// Only cross-harness hand-offs announce themselves; ordinary session startup stays invisible to the user.
    pub fn starting_label(&self) -> Option<String> {
        (self.phase == Phase::Starting && self.took_over)
            .then(|| format!("Handing over to {}…", self.harness.name()))
    }

    pub fn is_loading(&self) -> bool {
        self.phase == Phase::Starting
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

    // Queue edits and agent questions use the textarea for a separate, text-only input.
    fn attachments_enabled(&self) -> bool {
        self.queue_edit.is_none() && self.questions.is_empty() && self.async_answer_id().is_none()
    }

    fn can_submit_draft(&self, cx: &App) -> bool {
        crate::panels::composer::can_send(self.composer.read(cx).value().as_ref())
            || (self.attachments_enabled() && self.attachments.has_files())
    }

    pub fn attach_paths(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        self.attachments.add_paths(paths);
        self.focus_composer(window, cx);
        cx.notify();
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
    agent_meta: Value,
    replayed: bool,
}

async fn open(c: &Connection, harness: Harness, start: &Start) -> Result<Opened, String> {
    let initialized = c.request("initialize", acp::initialize_params()).await.map_err(|e| format!("{} didn't start: {e}", harness.name()))?;
    let capabilities = initialized["agentCapabilities"].clone();
    let agent_meta = initialized["_meta"].clone();
    let mut params = json!({"cwd": start.cwd, "mcpServers": start.mcp_servers, "_meta": start.meta});
    match &start.session_id {
        None => {
            let result = c.request("session/new", params).await.map_err(|e| format!("{} couldn't start a session: {e}", harness.name()))?;
            let id = result["sessionId"].as_str().ok_or("the agent named no session")?.to_string();
            Ok(Opened { session_id: id, result, capabilities, agent_meta, replayed: false })
        }
        Some(id) => {
            params["sessionId"] = json!(id);
            let reopen = acp::reopen_method(&capabilities, harness.adapter().reopen).map_err(|why| format!("{} cannot reopen this session: {why}.", harness.name()))?;
            let method = match reopen {
                Reopen::Load => "session/load",
                Reopen::Resume => "session/resume",
            };
            let result = c.request(method, params).await.map_err(|e| format!("{} couldn't open the session: {e}", harness.name()))?;
            Ok(Opened { session_id: id.clone(), result, capabilities, agent_meta, replayed: reopen == Reopen::Load })
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
pub(crate) fn has_saved_queue(record_id: &str) -> bool {
    !queue::load(record_id).is_empty()
}

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

    fn test_start(cwd: std::path::PathBuf, harness: Harness) -> super::Start {
        super::Start {
            record_id: String::new(), harness, all_models: vec![], picked: vec![], cwd,
            env: vec![], mcp_servers: vec![], meta: serde_json::Value::Null, session_id: None, mode: None,
            options: vec![], prompt: None, prior_entries: vec![], inline_visuals: true,
            server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        }
    }

    #[gpui::test]
    fn composer_pastes_files_and_images_and_queues_an_attachment_only_message(cx: &mut gpui::TestAppContext) {
        use gpui::{ClipboardEntry, ClipboardItem, ExternalPaths, Image, ImageFormat};
        use gpui_kit::component::input::Paste;

        let (data, _) = crate::datadir::use_throwaway_folders();
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task notes.txt");
        std::fs::write(&path, "notes").unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| {
            super::ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx)
        });
        let paste = |cx: &mut gpui::VisualTestContext, item| {
            cx.update(|window, app| {
                view.update(app, |v, cx| v.focus_composer(window, cx));
                app.write_to_clipboard(item);
            });
            cx.dispatch_action(Paste);
        };

        paste(cx, ClipboardItem::new_string("ordinary text".into()));
        cx.update(|window, app| view.update(app, |v, cx| {
            assert_eq!(v.draft(cx), "ordinary text");
            assert!(!v.attachments.has_files());
            v.set_draft("", window, cx);
        }));

        paste(cx, ClipboardItem { entries: vec![
            ClipboardEntry::ExternalPaths(ExternalPaths([path.clone()].into_iter().collect())),
        ] });
        let image = Image::from_bytes(ImageFormat::Png, include_bytes!("../../assets/m4code.png").to_vec());
        paste(cx, ClipboardItem::new_image(&image));
        cx.update(|_, app| view.update(app, |v, cx| {
            assert!(v.draft(cx).is_empty());
            assert!(v.can_submit_draft(cx));
            v.phase = super::Phase::Starting;
        }));
        cx.simulate_keystrokes("enter");
        cx.update(|_, app| view.update(app, |v, _| {
            assert!(!v.attachments.has_files());
            let message = &v.queue.front().unwrap().text;
            let paths = message.lines().skip(1).map(|line| {
                serde_json::from_str::<std::path::PathBuf>(line.strip_prefix("- ").unwrap()).unwrap()
            }).collect::<Vec<_>>();
            assert_eq!(paths.len(), 2);
            assert_eq!(paths[0], path);
            // Temp folders can be reached by more than one name (8.3 names on Windows, `/private` on macOS).
            let canonical = |p: &std::path::Path| std::fs::canonicalize(p).unwrap();
            assert_eq!(canonical(paths[1].parent().unwrap()), canonical(&data.join("attachments")));
            assert_eq!(std::fs::read(&paths[1]).unwrap(), image.bytes);
        }));
    }

    #[gpui::test]
    fn codex_async_questions_show_once_and_accept_answers_after_the_turn(cx: &mut gpui::TestAppContext) {
        use futures::StreamExt;
        use serde_json::json;
        use super::{Connection, Event, Phase, ThreadView};

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx));
        let chunk = |id: &str, text: &str| json!({"jsonrpc":"2.0","method":"session/update","params":{
            "sessionId":"codex-session", "update":{"sessionUpdate":"agent_message_chunk","messageId":id,"content":{"type":"text","text":text}}
        }});
        let frames = [
            chunk("unrelated-message", "I have a follow-up."),
            chunk("async-question-item", "Choose a branch.\n- main\n- dev\n\nWhat should I check?"),
            json!({"jsonrpc":"2.0","id":"question-form","method":"elicitation/create","params":{
                "sessionId":"codex-session", "mode":"form",
                "message":"Codex keeps working while you answer. An answer sent after it finishes is not delivered.",
                "_meta":{"codex":{"delivery":"async"}},
                "requestedSchema":{"type":"object","properties":{
                    "question_0":{"title":"Choose a branch.","type":"string","oneOf":[
                        {"const":"main","title":"main"}, {"const":"dev","title":"dev"},
                        {"const":"None of the above","title":"None of the above","description":"Provide a different answer in the note field."}
                    ]},
                    "question_0_note":{"title":"Additional answer or note","type":"string"},
                    "question_1":{"title":"What should I check?","type":"string"}
                }}
            }}),
        ];
        let mut wire = Vec::new();
        for frame in frames {
            serde_json::to_writer(&mut wire, &frame).unwrap();
            wire.push(b'\n');
        }
        let (tx, mut events) = futures::channel::mpsc::unbounded();
        let _connection = Connection::connect(std::io::Cursor::new(wire), Vec::<u8>::new(), None::<std::io::Empty>, tx);
        cx.update(|window, app| view.update(app, |v, cx| {
            v.phase = Phase::Ready;
            v.working = true;
            while let Some(event) = futures::executor::block_on(events.next()) {
                if matches!(event, Event::Exited(_)) { break; }
                v.on_event(event, cx);
            }
            assert!(matches!(&v.thread.entries[..], [Entry::Agent(text)] if text == "I have a follow-up."));
            assert!(v.questions.is_empty());
            assert!(v.is_waiting());
            assert_eq!(v.pending_question_title().as_deref(), Some("Choose a branch."));
            v.agent_meta = json!({"steering":{"supported":true}});
            assert!(v.can_steer("Ordinary message", crate::store::FollowUp::Steer));
            assert!(v.async_answer_id().is_none());
            v.send_as("Ordinary message".into(), crate::store::FollowUp::Queue, cx);
            let ask_id = v.async_ask().unwrap().id;
            v.toggle_async_answer(ask_id, window, cx);
            v.set_draft("My answer", window, cx);
            v.on_event(Event::Permission { id: json!("approval"), params: json!({
                "toolCall":{"title":"Run a command","kind":"execute"},
                "options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]
            }) }, cx);
            assert_eq!(v.pending_question_title().as_deref(), Some("Run a command"));
            assert!(v.async_ask().is_none());
            v.submit_with(false, window, cx);
            assert_eq!(v.draft(cx), "My answer");
            assert_eq!(v.queue.len(), 1);
            v.answer(Some(0), cx);
            assert_eq!(v.async_answer_id(), Some(ask_id));
            v.toggle_async_answer(ask_id, window, cx);
            v.set_draft("", window, cx);
            v.on_event(Event::TurnEnded(Ok(json!({"stopReason":"end_turn"}))), cx);
            let ask = v.async_ask().expect("the ask card survives turn completion");
            assert_eq!(ask.question.title, "Choose a branch.");
            assert_eq!(ask.question.options, ["main", "dev"]);
            v.choose_async_ask(ask.id, 1, cx);
            let ask = v.async_ask().unwrap();
            assert_eq!(ask.question.title, "What should I check?");
            v.answer_async_ask(ask.id, Some("The regression test.".into()), cx);
            assert!(!v.is_waiting());
            assert_eq!(v.queue.front().unwrap().text, "> Choose a branch.\n\ndev\n\n> What should I check?\n\nThe regression test.");
            assert_eq!(v.queue.back().unwrap().text, "Ordinary message");
            assert!(v.async_ask().is_none());
        }));
    }

    #[gpui::test]
    fn codex_async_answer_steers_the_running_turn_and_preserves_unmatched_text(cx: &mut gpui::TestAppContext) {
        use std::io::{BufRead, BufReader};
        use serde_json::{Value, json};
        use super::{Connection, Event, Phase, ThreadView};

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx));
        let (from_agent, _keep_open) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        let (tx, _events) = futures::channel::mpsc::unbounded();
        let connection = Connection::connect(from_agent, to_agent, None::<std::io::Empty>, tx);
        view.update(cx, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.session_id = Some("codex-session".into());
            v.agent_meta = json!({"steering":{"supported":true}});
            v.phase = Phase::Ready;
            v.working = true;
            v.on_event(Event::Update(json!({
                "sessionUpdate":"agent_message_chunk","messageId":"async-question-item",
                "content":{"type":"text","text":"There are two things I could investigate."}
            })), cx);
            v.on_event(Event::Elicitation { id: json!("question-form"), params: json!({
                "mode":"form", "_meta":{"codex":{"delivery":"async"}},
                "requestedSchema":{"properties":{"question_0":{"title":"Which test?","type":"string"}}}
            }) }, cx);
            assert!(matches!(&v.thread.entries[..], [Entry::Agent(text)] if text == "There are two things I could investigate."));
            v.answer_async_ask(v.async_ask().unwrap().id, Some("The unit test.".into()), cx);
            assert_eq!(v.steering.len(), 1);
            assert!(v.queue.is_empty());
        });
        let mut wire = BufReader::new(from_client).lines();
        let decline: Value = serde_json::from_str(&wire.next().unwrap().unwrap()).unwrap();
        assert_eq!(decline, json!({"jsonrpc":"2.0","id":"question-form","result":{"action":"decline"}}));
        let steer: Value = serde_json::from_str(&wire.next().unwrap().unwrap()).unwrap();
        assert_eq!(steer["method"], "_session/steering");
        assert_eq!(steer["params"]["sessionId"], "codex-session");
        assert_eq!(steer["params"]["prompt"][0]["text"], "> Which test?\n\nThe unit test.");
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn selected_chat_text_can_be_commented_on_from_the_conversation(cx: &mut gpui::TestAppContext) {
        check_selection_comment(cx, false);
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn chat_context_menu_keeps_the_quote_for_a_comment(cx: &mut gpui::TestAppContext) {
        check_selection_comment(cx, true);
    }

    #[cfg(debug_assertions)]
    fn check_selection_comment(cx: &mut gpui::TestAppContext, context_menu: bool) {
        use gpui::AppContext;

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Claude);
        start.prior_entries = vec![Entry::Agent("A line of conversation text".into())];
        let mut thread = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| super::ThreadView::new(start, window, cx));
            thread = Some(view.clone());
            gpui_kit::base::Root::new(view, window, cx)
        });
        let view = thread.unwrap();
        cx.update(|window, app| window.draw(app).clear(app));
        assert!(cx.debug_bounds("comment-on-selection").is_none());
        let text = view.read_with(cx, |v, app| v.markdown[&0].read(app).painted_text_bounds()[0]);
        let start = gpui::point(text.left() + gpui::px(1.0), text.center().y);
        let end = gpui::point(text.right() - gpui::px(1.0), text.center().y);
        let modifiers = gpui::Modifiers::default();
        cx.simulate_mouse_down(start, gpui::MouseButton::Left, modifiers);
        cx.simulate_mouse_move(end, gpui::MouseButton::Left, modifiers);
        cx.update(|window, app| window.draw(app).clear(app));
        cx.simulate_mouse_up(end, gpui::MouseButton::Left, modifiers);
        cx.update(|window, app| window.draw(app).clear(app));
        let comment = if context_menu {
            cx.simulate_mouse_down(end, gpui::MouseButton::Right, modifiers);
            cx.simulate_mouse_up(end, gpui::MouseButton::Right, modifiers);
            cx.update(|window, app| window.draw(app).clear(app));
            let bounds = cx.debug_bounds("chat-ctx-comment").expect("context menu shows Add comment");
            cx.simulate_mouse_down(bounds.center(), gpui::MouseButton::Left, modifiers);
            cx.update(|window, app| window.draw(app).clear(app));
            cx.simulate_mouse_up(bounds.center(), gpui::MouseButton::Left, modifiers);
            None
        } else {
            Some(cx.debug_bounds("comment-on-selection").expect("selection shows the comment button"))
        };
        if let Some(comment) = comment {
            cx.simulate_click(comment.center(), modifiers);
        }
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                let comment = v.selection_comment.as_ref().expect("comment input opened");
                comment.input.update(cx, |input, cx| input.set_value("Can you clarify?", window, cx));
            });
            window.draw(app).clear(app);
        });
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |v, app| {
            assert!(v.selection_comment.is_none());
            assert_eq!(v.draft(app), "> A line of conversation text\n\nCan you clarify?");
        });
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn checkpoint_restore_prompt_dismisses_with_escape(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext;
        cx.update(gpui_kit::init);
        let (data, _) = crate::datadir::use_throwaway_folders();
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Claude);
        start.record_id = crate::store::new_id();
        let id = uuid::Uuid::new_v4();
        let patch_dir = data.join("turn-reviews").join(&start.record_id);
        std::fs::create_dir_all(&patch_dir).unwrap();
        std::fs::write(patch_dir.join(format!("{id}.patch")), "").unwrap();
        let mut thread = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| super::ThreadView::new(start, window, cx));
            thread = Some(view.clone());
            gpui_kit::base::Root::new(view, window, cx)
        });
        let view = thread.unwrap();
        view.update(cx, |v, cx| {
            v.turn_reviews.push(super::review::TurnReview {
                id, row: 0, end: 1, text: "test".into(), before: "test".into(),
                cwd: dir.path().into(), file_count: 0, checkpoint: true,
            });
            v.open_turn_review(id, cx);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let restore = loop {
            cx.update(|window, app| window.draw(app).clear(app));
            if let Some(bounds) = cx.debug_bounds("review-restore") {
                break bounds;
            }
            assert!(std::time::Instant::now() < deadline, "restore action should appear");
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.run_until_parked();
        };
        cx.simulate_click(restore.center(), gpui::Modifiers::default());
        cx.update(|window, app| window.draw(app).clear(app));
        assert!(cx.debug_bounds("restore-cancel").is_some());
        cx.simulate_keystrokes("escape");
        cx.update(|window, app| window.draw(app).clear(app));
        assert!(cx.debug_bounds("restore-cancel").is_none());
        assert!(cx.debug_bounds("review-restore").is_some());
    }

    #[gpui::test]
    fn long_bullets_wrap_inside_the_message(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let long = "words that keep going on and on well past the width of the chat column so that they have to wrap onto another line rather than run off the side of the message box ".repeat(3);
        let md = format!("Intro paragraph.

- {long}
- **Bold:** with `code` {long}
  - nested {long}

1. ordered {long}
2. second

   continuation {long}");
        let mut start = test_start(dir.path().into(), Harness::Claude);
        start.prior_entries = vec![Entry::User(md.clone()), Entry::Agent(md)];
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        for _ in 0..3 {
            cx.update(|window, app| window.draw(app).clear(app));
        }
        view.read_with(cx, |view, app| {
            assert_eq!(view.markdown.len(), 2);
            for (row, state) in &view.markdown {
                let state = state.read(app);
                let frame = state.bounds();
                let painted = state.painted_text_bounds();
                assert!(!painted.is_empty(), "row {row} painted no text");
                assert!(frame.size.width <= gpui::px(super::render::COLUMN_MAX_W), "row {row}: the text is wider than the chat column: {frame:?}");
                for bounds in painted {
                    assert!(bounds.right() <= frame.right() + gpui::px(1.0), "row {row}: text ran past the message: {bounds:?} in {frame:?}");
                }
            }
        });
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn a_conversation_that_fits_never_shows_jump_to_latest(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Claude);
        start.prior_entries = vec![Entry::User("Short message".into())];
        let (_, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        cx.update(|window, app| window.draw(app).clear(app));
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: gpui::point(gpui::px(180.0), gpui::px(140.0)),
            delta: gpui::ScrollDelta::Lines(gpui::point(0.0, 5.0)),
            ..Default::default()
        });
        cx.update(|window, app| window.draw(app).clear(app));
        assert!(cx.debug_bounds("jump-to-latest").is_none());
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn wheel_scroll_releases_chat_tail_until_jump_to_latest(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Claude);
        start.prior_entries = (0..80).map(|row| Entry::User(format!("Prompt {row}"))).collect();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        cx.update(|window, app| window.draw(app).clear(app));
        assert!(view.read_with(cx, |view, _| view.list.is_following_tail()));
        assert!(view.read_with(cx, |view, _| view.list.max_offset_for_scrollbar().y) > gpui::px(2500.0),
            "the scrollbar includes rows before the viewport on first render");
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: gpui::point(gpui::px(180.0), gpui::px(140.0)),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(140.0))),
            ..Default::default()
        });
        cx.update(|window, app| window.draw(app).clear(app));
        let top = view.read_with(cx, |view, _| {
            assert!(!view.list.is_following_tail());
            view.list.logical_scroll_top().item_ix
        });
        view.update(cx, |view, _| {
            let row = view.thread.entries.len();
            view.thread.entries.push(Entry::Agent("A streamed reply".into()));
            view.remeasure(row);
        });
        cx.update(|window, app| window.draw(app).clear(app));
        assert_eq!(view.read_with(cx, |view, _| view.list.logical_scroll_top().item_ix), top);
        let jump = cx.debug_bounds("jump-to-latest").expect("Jump to latest shown while scrolled up");
        cx.simulate_click(jump.center(), gpui::Modifiers::default());
        cx.update(|window, app| window.draw(app).clear(app));
        assert!(view.read_with(cx, |view, _| view.list.is_following_tail()));
        assert!(cx.debug_bounds("jump-to-latest").is_none());

        cx.simulate_event(gpui::ScrollWheelEvent {
            position: gpui::point(gpui::px(180.0), gpui::px(140.0)),
            delta: gpui::ScrollDelta::Lines(gpui::point(0.0, 5.0)),
            ..Default::default()
        });
        cx.update(|window, app| window.draw(app).clear(app));
        let first = view.read_with(cx, |view, _| {
            assert!(!view.list.is_following_tail(), "wheel notches release tail-follow immediately");
            view.list.scroll_px_offset_for_scrollbar().y
        });
        for _ in 0..15 {
            std::thread::sleep(std::time::Duration::from_millis(12));
            cx.update(|window, app| window.draw(app).clear(app));
        }
        let last = view.read_with(cx, |view, _| view.list.scroll_px_offset_for_scrollbar().y);
        assert!(last > first + gpui::px(4.0), "the notch should glide across frames, from {first:?} to {last:?}");
    }

    #[gpui::test]
    fn slash_background_padding_preserves_text_geometry(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let start = test_start(dir.path().into(), Harness::Claude);
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.update_in(cx, |view, window, cx| view.set_draft("/new example", window, cx));
        cx.update(|window, app| window.draw(app).clear(app));
        let bounds = |cx: &gpui::VisualTestContext| {
            view.read_with(cx, |view, app| view.composer.read(app).range_to_bounds(&(5..12))).unwrap()
        };
        let decorated = bounds(cx);
        view.update(cx, |view, cx| view.composer.update(cx, |input, cx| input.set_decorations(Vec::new(), cx)));
        cx.update(|window, app| window.draw(app).clear(app));
        assert_eq!(decorated, bounds(cx), "the slash command must not change the position of the following text");
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn a_tool_file_chip_opens_its_file_and_line(cx: &mut gpui::TestAppContext) {
        use crate::harness::thread::{ToolCall, ToolContent, ToolStatus};
        use std::cell::RefCell;
        use std::rc::Rc;

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("two words.rs");
        std::fs::write(&file, "\nfn main() {}").unwrap();
        let url = url::Url::from_file_path(&file).unwrap();
        let mut start = test_start(dir.path().into(), Harness::Codex);
        start.prior_entries.push(Entry::Tool(ToolCall {
            id: "read".into(),
            title: "Read file".into(),
            kind: "read".into(),
            status: ToolStatus::Done,
            locations: vec![(url.as_str().into(), Some(2))],
            content: vec![ToolContent::Text("fn main() {}".into())],
            ..Default::default()
        }));
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        let opened = Rc::new(RefCell::new(None));
        let capture = opened.clone();
        let _sub = cx.update(|_, app| app.subscribe(&view, move |_, event, _| {
            if let super::ThreadEvent::OpenPath(path, line) = event {
                capture.replace(Some((std::path::PathBuf::from(path), *line)));
            }
        }));
        cx.update(|window, app| window.draw(app).clear(app));
        let chip = cx.debug_bounds("tool-loc-0-0").expect("file chip");
        cx.simulate_click(chip.center(), gpui::Modifiers::default());
        assert_eq!(opened.take(), Some((file, Some(2))));
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn collapsed_tool_group_keeps_multiline_commands_to_one_line(cx: &mut gpui::TestAppContext) {
        use crate::harness::thread::{ToolCall, ToolStatus};
        use gpui::Modifiers;

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Codex);
        start.prior_entries = vec![
            Entry::Thought("Run a check.".into()),
            Entry::Tool(ToolCall { id: "check".into(), title: "first line\nsecond line\nthird line".into(), status: ToolStatus::Failed, ..Default::default() }),
        ];
        let (_, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        cx.update(|window, app| window.draw(app).clear(app));
        let header = cx.debug_bounds("tool-run-0").expect("collapsed group header");
        assert!(header.size.height < gpui::px(32.0), "the collapsed header was {header:?}");
        assert!(cx.debug_bounds("tool-row-1").is_none());
        cx.simulate_click(header.center(), Modifiers::default());
        cx.update(|window, app| window.draw(app).clear(app));
        assert!(cx.debug_bounds("tool-row-1").is_some(), "expand to see the tool call");
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn shell_output_can_be_collapsed_with_tool_details_on(cx: &mut gpui::TestAppContext) {
        use crate::harness::thread::{ToolCall, ToolContent};
        use gpui::Modifiers;

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Claude);
        start.prior_entries.push(Entry::Tool(ToolCall {
            id: "shell".into(),
            title: "Print a line".into(),
            kind: "execute".into(),
            content: vec![ToolContent::Text("shell output".into())],
            ..Default::default()
        }));
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        let row = |cx: &mut gpui::VisualTestContext| {
            cx.refresh().unwrap();
            view.read_with(cx, |v, _| v.list.bounds_for_item(0).expect("shell call is visible"))
        };

        let collapsed = row(cx).size.height;
        view.update(cx, |v, cx| {
            v.detail = true;
            v.remeasure(0);
            cx.notify();
        });
        let expanded = row(cx);
        assert!(expanded.size.height > collapsed);

        let click_header = |cx: &mut gpui::VisualTestContext| {
            let header = cx.debug_bounds("tool-row-0").expect("shell header is visible");
            cx.simulate_click(header.center(), Modifiers::default());
        };
        click_header(cx);
        assert_eq!(row(cx).size.height, collapsed);
        click_header(cx);
        assert_eq!(row(cx).size.height, expanded.size.height);
    }

    #[gpui::test]
    fn interrupt_before_dispatch_preserves_the_queue_and_cancels_each_request(cx: &mut gpui::TestAppContext) {
        use std::io::{BufRead, BufReader};
        use serde_json::{Value, json};
        use super::{Connection, Event, Phase, Queued, ThreadView};

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| ThreadView::new(test_start(dir.path().into(), Harness::Claude), window, cx));
        let (from_agent, to_client) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        std::mem::forget(to_client);
        let (tx, _events) = futures::channel::mpsc::unbounded();
        let connection = Connection::connect(from_agent, to_agent, None::<std::io::PipeReader>, tx);
        view.update(cx, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.session_id = Some("s".into());
            v.phase = Phase::Ready;
            v.working = true;
            let first = Queued::new("first".into(), true);
            let id = first.id;
            v.preparing = Some(first);
            v.send_as("second".into(), crate::store::FollowUp::Queue, cx);

            for (request, title) in [(7, "first approval"), (8, "second approval")] {
                v.on_event(Event::Permission { id: json!(request), params: json!({
                    "toolCall": {"title": title, "kind": "execute"},
                    "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}]
                }) }, cx);
            }
            assert_eq!(v.pending_question_title().as_deref(), Some("first approval"));
            let shown = v.pending_question().unwrap().id;
            assert!(v.answer_question(shown, Some(0), cx));
            assert_eq!(v.pending_question_title().as_deref(), Some("second approval"));
            assert!(!v.answer_question(shown, Some(0), cx), "a click from the old question cannot approve the next");
            v.interrupt(cx);
            assert!(!v.is_waiting());
            assert!(v.queue_held);
            assert!(v.take_prepared(id).is_none(), "the snapshot completion can no longer dispatch this prompt");
            assert_eq!(v.waiting_messages().map(|(_, text)| text).collect::<Vec<_>>(), ["first", "second"]);
        });

        let mut replies = BufReader::new(from_client).lines();
        let approved: Value = serde_json::from_str(&replies.next().unwrap().unwrap()).unwrap();
        let cancelled: Value = serde_json::from_str(&replies.next().unwrap().unwrap()).unwrap();
        assert_eq!(approved["id"], 7);
        assert_eq!(approved["result"]["outcome"]["optionId"], "allow");
        assert_eq!(cancelled["id"], 8);
        assert_eq!(cancelled["result"]["outcome"]["outcome"], "cancelled");
    }

    #[gpui::test]
    fn codex_uses_the_advertised_steering_extension_without_cancelling(cx: &mut gpui::TestAppContext) {
        use std::io::{BufRead, BufReader};
        use serde_json::{Value, json};
        use super::{Connection, Phase, ThreadView};

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx));
        let (from_agent, to_client) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        std::mem::forget(to_client);
        let (tx, _events) = futures::channel::mpsc::unbounded();
        let connection = Connection::connect(from_agent, to_agent, None::<std::io::PipeReader>, tx);
        view.update(cx, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.session_id = Some("s".into());
            v.phase = Phase::Ready;
            v.working = true;
            v.agent_meta = json!({"steering": {"supported": true}});
            v.send_as("change focus".into(), crate::store::FollowUp::Steer, cx);
            assert_eq!(v.unread_steers().collect::<Vec<_>>(), ["change focus"]);
        });
        let request: Value = serde_json::from_str(&BufReader::new(from_client).lines().next().unwrap().unwrap()).unwrap();
        assert_eq!(request["method"], "_session/steering");
        assert_eq!(request["params"]["prompt"][0]["text"], "change focus");
        assert_eq!(request["params"]["_meta"]["steering"]["idleBehavior"], "promptRequired");
    }

    #[test]
    fn a_comment_goes_on_its_own_line_after_what_was_typed() {
        assert_eq!(with_comment("", "@a.rs#L1: why"), "@a.rs#L1: why");
        assert_eq!(with_comment("look at these \n", "@a.rs#L1: why"), "look at these\n@a.rs#L1: why");
    }

    #[gpui::test]
    fn a_message_from_m4code_never_starts_the_hand_off_the_user_picked(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let start = super::Start {
            record_id: String::new(), harness: Harness::Claude, all_models: vec![], picked: vec![], cwd: dir.path().to_path_buf(),
            env: vec![], mcp_servers: vec![], meta: serde_json::Value::Null, session_id: None, mode: None, options: vec![],
            prompt: None, prior_entries: vec![], inline_visuals: true, server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        };
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.update(cx, |v, cx| {
            v.pending_model = Some(crate::harness::CatalogModel { harness: Harness::Codex, id: "gpt-6".into(), name: "GPT-6".into() });
            v.deliver("[m4code subagent report]".into(), cx);
            assert!(v.pending_model.is_some(), "the user's picked model waits for the user's own message");
            assert_eq!(v.queue.len(), 1, "the report waits to be sent to this agent");
        });
    }

    #[gpui::test]
    fn a_subagent_report_folds_with_the_steps_but_a_users_message_does_not(cx: &mut gpui::TestAppContext) {
        use crate::harness::thread::{ToolCall, subagent_report_prompt};
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let start = super::Start {
            record_id: String::new(), harness: Harness::Claude, all_models: vec![], picked: vec![], cwd: dir.path().to_path_buf(),
            env: vec![], mcp_servers: vec![], meta: serde_json::Value::Null, session_id: None, mode: None, options: vec![],
            prompt: None, prior_entries: vec![], inline_visuals: true, server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        };
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.update(cx, |v, _| {
            v.thread.entries = vec![
                Entry::Agent("starting a subagent".into()),
                Entry::Tool(ToolCall { id: "t1".into(), title: "spawn_subagent".into(), ..Default::default() }),
                Entry::User(subagent_report_prompt("c1", "codex", "Codex subagent", "completed", "ok")),
                Entry::Thought("reading the report".into()),
                Entry::User("thanks".into()),
            ];
            assert_eq!((0..v.thread.entries.len()).map(|ix| v.run_kind(ix).is_some()).collect::<Vec<_>>(), [false, true, true, true, false]);
        });
    }

    #[gpui::test]
    fn queue_edits_keep_the_draft_and_queue_position(cx: &mut gpui::TestAppContext) {
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
            prompt: None,
            prior_entries: vec![], inline_visuals: true,
            server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        };
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.update_in(cx, |v, window, cx| {
            v.queue.clear();
            v.queue.push_back(super::Queued::new("report".into(), false));
            v.queue.push_back(super::Queued::new("first\nmore".into(), true));
            v.queue.push_back(super::Queued::new("second".into(), true));
            let id = v.queue[1].id;
            assert_eq!(v.waiting_messages().map(|(_, text)| text).collect::<Vec<_>>(), ["first\nmore", "second"]);
            assert!(v.unqueue(v.queue[0].id, cx).is_none(), "a report is not the user's to remove");
            v.set_draft("typed", window, cx);
            v.edit_queued(id, window, cx);
            assert_eq!(v.draft(cx), "first\nmore");
            v.set_draft("changed", window, cx);
            v.finish_queue_edit(false, window, cx);
            assert_eq!(v.draft(cx), "typed");
            assert_eq!(v.queue[1].text, "first\nmore");
            v.edit_queued(id, window, cx);
            v.set_draft("changed", window, cx);
            v.finish_queue_edit(true, window, cx);
            assert_eq!(v.draft(cx), "typed");
            assert_eq!((v.queue[1].id, v.queue[1].text.as_str()), (id, "changed"));
            v.move_queued(id, false, cx);
            assert_eq!(v.queue.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["report", "second", "changed"]);
            v.move_queued(id, true, cx);
            assert_eq!(v.unqueue(v.queue[2].id, cx).unwrap().text, "second");
        });
    }

    #[gpui::test]
    fn only_a_plain_message_mid_turn_steers(cx: &mut gpui::TestAppContext) {
        use crate::store::FollowUp;
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let start = super::Start {
            record_id: String::new(),
            harness: Harness::Codex,
            all_models: vec![],
            picked: vec![],
            cwd: dir.path().to_path_buf(),
            env: vec![],
            mcp_servers: vec![],
            meta: serde_json::Value::Null,
            session_id: None,
            mode: None,
            options: vec![],
            prompt: None,
            prior_entries: vec![], inline_visuals: true,
            server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        };
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.update(cx, |v, cx| {
            v.phase = super::Phase::Ready;
            v.agent_meta = serde_json::json!({"steering":{"supported":true}});
            assert!(!v.can_steer("hi", FollowUp::Steer), "nothing to steer while idle");
            v.working = true;
            assert!(v.can_steer("hi", FollowUp::Steer));
            assert!(!v.can_steer("hi", FollowUp::Queue));
            assert!(!v.can_steer("/compact", FollowUp::Steer), "a command waits for the turn");
            v.compaction.running = true;
            assert!(!v.can_steer("hi", FollowUp::Steer), "compaction takes no steers");
            v.compaction.running = false;
            v.running_command = true;
            assert!(!v.can_steer("hi", FollowUp::Steer), "nor does a /compact the user ran");
            v.running_command = false;
            // With no agent to reach, the message waits rather than being lost.
            v.queue.clear();
            v.send_as("hi".into(), FollowUp::Steer, cx);
            assert_eq!(v.waiting_messages().map(|(_, text)| text).collect::<Vec<_>>(), ["hi"]);
        });
    }

    #[gpui::test]
    fn a_reply_keeps_its_parsed_markdown_between_frames(cx: &mut gpui::TestAppContext) {
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
            prompt: None,
            prior_entries: vec![], inline_visuals: true,
            server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        };
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.update(cx, |v, cx| {
            let mut cache = std::mem::take(&mut v.markdown);
            super::render::markdown_of(&mut cache, 3, "**first**", cx);
            let first = cache[&3].entity_id();
            super::render::markdown_of(&mut cache, 3, "**first** and more", cx);
            assert_eq!(cache[&3].entity_id(), first, "a redraw or a streamed reply keeps the parse, so the row keeps its height");
            super::render::markdown_of(&mut cache, 4, "other", cx);
            assert_ne!(cache[&4].entity_id(), first, "each entry has its own");
        });
    }

    #[gpui::test]
    fn a_steer_joins_the_chat_where_the_agent_reads_it_not_where_it_was_sent(cx: &mut gpui::TestAppContext) {
        use crate::harness::acp::Event;
        use crate::store::FollowUp;
        use serde_json::json;
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
            prompt: None,
            prior_entries: vec![], inline_visuals: true,
            server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        };
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        // An agent that never answers: the steer request goes out and waits. Its ends stay open for the rest of the
        // run, because closing them fails the request from the connection's reader thread, which GPUI's test
        // executor rejects as activity from outside it.
        let (from_agent, to_client) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        std::mem::forget((to_client, from_client));
        let (tx, _events) = futures::channel::mpsc::unbounded();
        let connection = crate::harness::acp::Connection::connect(from_agent, to_agent, None::<std::io::PipeReader>, tx);
        let chunk = |kind: &str, text: &str| json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}});
        view.update(cx, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.session_id = Some("s1".into());
            v.phase = super::Phase::Ready;
            v.working = true;
            v.thread.entries.clear();
            v.on_event(Event::Update(chunk("agent_message_chunk", "Hello wor")), cx);
            v.send_as("stop".into(), FollowUp::Steer, cx);
            assert_eq!(v.unread_steers().collect::<Vec<_>>(), ["stop"], "waits above the message box");
            // What the agent was still writing when the steer went out.
            v.on_event(Event::Update(chunk("agent_message_chunk", "ld.")), cx);
            let mut read = chunk("user_message_chunk", "stop");
            read["_meta"] = json!({"m4code": {"steer": v.steering.front().unwrap().id.to_string()}});
            v.on_event(Event::Update(read), cx);
            v.on_event(Event::Update(chunk("agent_message_chunk", "Stopping.")), cx);
            assert_eq!(v.thread.entries, [Entry::Agent("Hello world.".into()), Entry::User("stop".into()), Entry::Agent("Stopping.".into())]);
            assert_eq!(v.unread_steers().count(), 0);
            // Two steers read one after the other stay two messages.
            v.send_as("do X".into(), FollowUp::Steer, cx);
            v.send_as("do Y".into(), FollowUp::Steer, cx);
            for text in ["do X", "do Y"] {
                let mut read = chunk("user_message_chunk", text);
                read["_meta"] = json!({"m4code": {"steer": v.steering.front().unwrap().id.to_string()}});
                v.on_event(Event::Update(read), cx);
            }
            assert_eq!(v.thread.entries[3..], [Entry::User("do X".into()), Entry::User("do Y".into())]);
            v.send_as("older".into(), FollowUp::Queue, cx);
            v.send_as("unread".into(), FollowUp::Steer, cx);
            v.send_as("later".into(), FollowUp::Queue, cx);
            v.turn_ended(Err("Claude Code stopped.".into()), cx);
            assert_eq!(v.unread_steers().count(), 0);
            assert!(v.queue_held);
            assert_eq!(v.waiting_messages().map(|(_, text)| text).collect::<Vec<_>>(), ["older", "unread", "later"]);
        });
    }

    #[gpui::test]
    fn steers_an_agent_restarts_for_go_in_the_order_sent_before_the_queue(cx: &mut gpui::TestAppContext) {
        use crate::store::FollowUp;
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let start = super::Start {
            record_id: String::new(),
            harness: Harness::Codex,
            all_models: vec![],
            picked: vec![],
            cwd: dir.path().to_path_buf(),
            env: vec![],
            mcp_servers: vec![],
            meta: serde_json::Value::Null,
            session_id: None,
            mode: None,
            options: vec![],
            prompt: None,
            prior_entries: vec![], inline_visuals: true,
            server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        };
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        // Kept open for the rest of the run, as in `a_steer_joins_the_chat_where_the_agent_reads_it_not_where_it_was_sent`.
        let (from_agent, to_client) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        std::mem::forget((to_client, from_client));
        let (tx, _events) = futures::channel::mpsc::unbounded();
        let connection = crate::harness::acp::Connection::connect(from_agent, to_agent, None::<std::io::PipeReader>, tx);
        view.update(cx, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.session_id = Some("s1".into());
            v.phase = super::Phase::Ready;
            v.working = true;
            v.queue.clear();
            v.send_as("queued".into(), FollowUp::Queue, cx);
            v.send_as("first".into(), FollowUp::Steer, cx);
            v.send_as("second".into(), FollowUp::Steer, cx);
            let order: Vec<&str> = v.queue.iter().map(|q| q.text.as_str()).collect();
            assert_eq!(order, ["first", "second", "queued"]);
            assert!(v.restarting_for_steer);
            assert!(v.steering.is_empty());
        });
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
            prior_entries: vec![], inline_visuals: true,
            server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        };
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.read_with(cx, |v, _| {
            assert_eq!(v.thread.entries, [Entry::User("hello world".into())]);
            assert_eq!(v.rows, 1);
            assert_eq!(v.queue.len(), 1);
            assert!(v.queue[0].shown);
            assert!(v.starting_label().is_none(), "normal session startup is invisible to the user");
            assert!(v.waiting_messages().next().is_none(), "already-shown startup prompt is not duplicated in the queue");
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

    #[gpui::test]
    fn optimistic_startup_shows_user_message_immediately_and_can_be_interrupted(cx: &mut gpui::TestAppContext) {
        use crate::store::FollowUp;
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), Harness::Claude), window, cx));
        view.update(cx, |v, cx| {
            v.phase = super::Phase::Starting;
            assert!(v.starting_label().is_none());
            assert!(!v.has_active_turn());
            v.send_as("first prompt".into(), FollowUp::Queue, cx);
            assert_eq!(v.thread.entries, [Entry::User("first prompt".into())]);
            assert!(v.has_active_turn());
            assert!(v.is_working());
            assert!(v.waiting_messages().next().is_none(), "shown startup prompt is not duplicated in queued list");
            v.interrupt(cx);
            assert!(v.thread.entries.is_empty());
            assert!(!v.has_active_turn());
            assert!(v.queue_held);
            assert_eq!(v.waiting_messages().map(|(_, t)| t).collect::<Vec<_>>(), ["first prompt"]);
        });
    }

    #[gpui::test]
    fn post_turn_continuation_remains_interruptible_without_getting_stuck(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), Harness::Claude), window, cx));
        view.update(cx, |v, cx| {
            v.phase = super::Phase::Ready;
            v.working = false;
            assert!(!v.can_interrupt());
            v.on_event(super::Event::Update(serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "continuing after background work" }
            })), cx);
            assert!(v.can_interrupt(), "streaming updates after a turn ended must stay interruptible");
            v.interrupt(cx);
            assert!(!v.can_interrupt());
            assert!(!v.interrupting, "interrupting a continuation without an open turn must not stick on Stopping");
        });
    }

    #[gpui::test]
    fn opencode_shells_without_a_final_status_stop_showing_as_running(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), Harness::OpenCode), window, cx));
        view.update(cx, |v, cx| {
            v.phase = super::Phase::Ready;
            v.on_event(super::Event::Update(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "shell",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": "cargo check" }
            })), cx);
            assert!(v.has_background_work());
            v.turn_ended(Ok(serde_json::json!({"stopReason": "end_turn"})), cx);
            assert!(!v.has_background_work());
            assert_eq!(v.thread.tool("shell").unwrap().status, super::ToolStatus::Ended);
        });
    }

    #[gpui::test]
    fn antigravity_background_tasks_ignore_replay_and_survive_turns_until_finished(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), Harness::Antigravity), window, cx));
        view.update(cx, |v, cx| {
            let start = serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "cmd-1",
                "title": "Run clippy",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "command": "cargo clippy --locked" },
                "rawOutput": "Tool is running as a background task with task id: conv/task-42\nTask logs are available at: ..."
            });
            v.on_event(super::Event::Update(start.clone()), cx);
            assert!(v.background_tasks.is_empty(), "replay does not create live tasks");
            v.phase = super::Phase::Ready;
            v.on_event(super::Event::Update(start), cx);
            assert_eq!(v.background_tasks.len(), 1);
            assert_eq!(v.background_tasks[0].id, "conv/task-42");
            assert_eq!(v.background_tasks[0].label, "cargo clippy --locked");
            assert!(v.has_background_work());
            // Ending the turn that spawned the task keeps it visible while waiting for it to finish.
            v.turn_ended(Ok(serde_json::json!({"stopReason": "end_turn"})), cx);
            assert_eq!(v.background_tasks.len(), 1);
            v.turn_ended(Ok(serde_json::json!({"stopReason": "end_turn"})), cx);
            assert!(v.has_background_work(), "a later turn can end while the command is still running");
            // A completion notification retires it immediately.
            v.on_event(super::Event::Update(serde_json::json!({
                "sessionUpdate": "user_message_chunk",
                "content": { "type": "text", "text": "Task id \"conv/task-42\" finished with result: Command exited with code 0." }
            })), cx);
            assert!(v.background_tasks.is_empty());
            v.on_event(super::Event::Update(serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "cmd-1",
                "content": [{ "type": "content", "content": { "type": "text", "text": "Late output" } }]
            })), cx);
            assert!(v.background_tasks.is_empty(), "late output must not resurrect a finished task");
        });
    }

    #[gpui::test]
    fn sending_in_ready_session_shows_user_message_immediately_before_snapshot(cx: &mut gpui::TestAppContext) {
        use crate::store::FollowUp;
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), Harness::Claude), window, cx));
        let (from_agent, to_client) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        std::mem::forget((to_client, from_client));
        let (tx, _events) = futures::channel::mpsc::unbounded();
        let connection = crate::harness::acp::Connection::connect(from_agent, to_agent, None::<std::io::PipeReader>, tx);
        view.update(cx, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.session_id = Some("s1".into());
            v.phase = super::Phase::Ready;
            v.send_as("instant message".into(), FollowUp::Queue, cx);
            assert_eq!(v.thread.entries, [Entry::User("instant message".into())]);
            assert!(v.working);
            assert!(v.preparing.as_ref().is_some_and(|p| p.shown));
        });
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn streaming_thought_stays_collapsed_by_default_until_clicked(cx: &mut gpui::TestAppContext) {
        use gpui::Modifiers;
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Claude);
        start.prior_entries = vec![Entry::Thought("line 1\nline 2\nline 3\nline 4\n\n".into())];
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.update(cx, |v, cx| {
            v.phase = super::Phase::Ready;
            v.working = true;
            v.thought_timing.insert(0, (std::time::Instant::now(), None));
            v.remeasure(0);
            cx.notify();
        });
        cx.update(|window, app| window.draw(app).clear(app));
        let collapsed = view.read_with(cx, |v, _| v.list.bounds_for_item(0).expect("thought row").size.height);
        assert!(collapsed < gpui::px(48.0), "streaming thought should stay collapsed by default, got {collapsed:?}");
        let center = view.read_with(cx, |v, _| v.list.bounds_for_item(0).unwrap().center());
        cx.simulate_click(center, Modifiers::default());
        cx.update(|window, app| window.draw(app).clear(app));
        let expanded = view.read_with(cx, |v, _| v.list.bounds_for_item(0).expect("thought row").size.height);
        assert!(expanded > collapsed, "clicking a collapsed thought should expand it: {expanded:?} vs {collapsed:?}");
    }

    #[test]
    fn active_slash_token_triggers_at_start_and_mid_message_but_skips_paths_and_urls() {
        assert_eq!(super::active_slash_token("/com", 4), Some((0, 4, "com")));
        assert_eq!(super::active_slash_token("please run /com", 15), Some((11, 15, "com")));
        assert_eq!(super::active_slash_token("please /comp now", 12), Some((7, 12, "comp")));
        assert_eq!(super::active_slash_token("check src/thread/mod.rs", 23), None);
        assert_eq!(super::active_slash_token("see https://example.com", 23), None);
        assert_eq!(super::active_slash_token("hello world", 11), None);
    }

    #[test]
    fn slash_command_spans_finds_all_command_tokens_and_skips_paths() {
        let spans = super::slash_command_spans("please /compact and /clear src/main.rs https://a.b/c");
        assert_eq!(spans, vec![(7..15, "compact"), (20..26, "clear")]);
    }
}
