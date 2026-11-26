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
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{App, AppContext, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, FollowMode, KeystrokeEvent, ListAlignment, ListState, Pixels, Point, Subscription, Window, px};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use serde_json::{Value, json};
use uuid::Uuid;

use self::queue::Queued;

use crate::harness::acp::{self, Connection, Event, Launch};
use crate::harness::defaults::{self, Starter};
use crate::harness::thread::{Entry, Thread, ToolStatus};
use crate::harness::permission::{self, Permission};
use crate::harness::{BackgroundTask as ProviderTask, CatalogModel, Harness, Reopen};
use crate::visual::ui::{CardUi, OpenFile, SendText};

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

fn settle_running_tools(entries: &mut [Entry]) -> Option<usize> {
    settle_running_tools_filtered(entries, false)
}

fn settle_running_tools_filtered(entries: &mut [Entry], keep_live_execute: bool) -> Option<usize> {
    let mut first_changed = None;
    for (row, entry) in entries.iter_mut().enumerate() {
        if let Entry::Tool(call) = entry
            && matches!(call.status, ToolStatus::Running | ToolStatus::Pending)
        {
            if keep_live_execute && call.kind == "execute" && call.status == ToolStatus::Running && call.details.exit_code.is_none() {
                continue;
            }
            call.status = match call.details.exit_code {
                Some(0) => ToolStatus::Done,
                Some(_) => ToolStatus::Failed,
                None => ToolStatus::Ended,
            };
            first_changed.get_or_insert(row);
        }
    }
    first_changed
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
pub(crate) const WORKTREE_SEED: &str = "Moved to a new checkout. The next message continues this conversation.";

fn needs_history_seed(entry: &Entry) -> bool {
    matches!(entry, Entry::Notice { text, error: false } if matches!(text.as_str(), FORK_SEED | WORKTREE_SEED))
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
    /// A user prompt is being submitted, including the initial startup prompt.
    PromptSent(String),
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
    /// A file a tool acted on or a markdown/UI link was clicked: its path, 1-based start line, and optional 1-based end line.
    OpenPath(String, Option<usize>, Option<usize>),
    ReconnectPermission(Permission),
    PermissionPreference(Permission),
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
    if cur <= start || !draft[start..].starts_with('/') {
        return None;
    }
    let after = &draft[cur..];
    let end = cur
        + after
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map_or(after.len(), |(i, _)| i);
    let typed = draft.get(start + 1..cur)?;
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

pub(crate) struct ComposerState {
    text: String,
    attachments: crate::panels::attachments::Attachments,
    permission: Option<Permission>,
    git: Option<GitChip>,
}

#[cfg(test)]
impl ComposerState {
    pub(crate) fn has_attachments(&self) -> bool {
        self.attachments.has_files()
    }
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
    pub mcp_tools: Option<crate::mcp::session::SessionTools>,
    pub launch_args: Vec<String>,
    pub permission: Option<Permission>,
    pub starter: Starter,
    /// Options to apply before the first prompt (`model`, `thought_level`).
    pub options: Vec<(&'static str, String)>,
    /// Sent as soon as the session is ready.
    pub prompt: Option<String>,
    /// Entries carried over visually from a previous harness during an in-chat handoff.
    pub prior_entries: Vec<Entry>,
    /// Whether inline UI blocks are enabled.
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
    pub multi_select: bool,
    pub selected: BTreeSet<usize>,
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
    AsyncAsk {
        remaining: VecDeque<crate::harness::AsyncQuestion>,
        replies: Vec<String>,
    },
}

/// A permission or multiple-choice question the agent is waiting for.
struct Question {
    id: Uuid,
    reply: QuestionReply,
    title: String,
    options: Vec<QuestionOption>,
    /// The option Enter picks (single-select) or Space toggles (multi-select); the arrows and the mouse move it.
    highlight: usize,
    /// When true, options toggle checkboxes and submit together instead of submitting on a single click.
    multi_select: bool,
    /// Indices of checked options when `multi_select` is true, kept in option order.
    selected: BTreeSet<usize>,
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
    /// What had the keyboard before the menu took it.
    return_focus: Option<FocusHandle>,
}

struct AgentCache {
    text_len: usize,
    citations: Vec<elements::Citation>,
}

#[derive(Default)]
struct RunCache {
    entries: usize,
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
    background_tasks: Vec<ProviderTask>,
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
    /// Whether inline UI blocks are enabled.
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
    /// Monotonically incremented each time a real turn starts; used to send at most one done notification per turn.
    turn_seq: u64,
    /// Turn outcome waiting for live background tasks to finish before emitting `ThreadEvent::Idle`.
    pending_idle: Option<&'static str>,
    /// When the running turn's prompt went out, for the timer on the chat's working row.
    turn_started: Option<Instant>,
    /// How long the last finished turn took, shown as "Worked for …" under it until the next one starts.
    last_turn: Option<Duration>,
    /// When the last live conversation update arrived (outside replay), so detached/post-turn streaming still shows as working.
    last_update: Option<Instant>,
    /// Messages typed during a turn, sent one at a time after it.
    queue: VecDeque<Queued>,
    queue_held: bool,
    workspace_switching: bool,
    permission_changing: bool,
    read_only_unconfirmed: bool,
    queue_edit: Option<queue::Edit>,
    preparing: Option<Queued>,
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
    pub(crate) fallback_model: Option<CatalogModel>,
    pub(crate) auto_handoff_on_limit: bool,
    runway: crate::harness::limits::RunwayTracker,
    /// How many rows the list has measured.
    rows: usize,
    /// Ctrl+O: every tool's output in full.
    pub detail: bool,
    pub(crate) subagent_edits: Option<bool>,
    pub(super) initial_permission: Option<Permission>,
    antigravity_full_access: bool,
    auto_fallback: bool,
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
    /// Up/Down/Enter in the open Model or Permission picker.
    pub(super) picker_nav: crate::panels::ui::command::Nav,
    /// Up/Down/Enter in the chat's right-click menu.
    pub(super) menu_nav: crate::panels::ui::command::Nav,
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
    card_ui: Entity<CardUi>,
    /// The working tree as it was before each message, oldest first (`git/snapshot.rs`). Empty outside a git
    /// repository.
    pub snapshots: Vec<String>,
    turn_reviews: Vec<review::TurnReview>,
    active_turn_review: Option<review::ActiveTurn>,
    /// The slash menu's highlighted row, and its scroll.
    command_highlight: usize,
    command_scroll: crate::panels::glide::Smooth,
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
            cx.subscribe_in(&card_ui, window, |v: &mut Self, ui, ev: &SendText, window, cx| {
                if v.workspace_switching || v.read_only_unconfirmed {
                    return;
                }
                ui.update(cx, |ui, cx| ui.mark_sent(&ev.key, cx));
                v.send(ev.text.clone(), window, cx);
            }),
            cx.subscribe_in(&card_ui, window, |v: &mut Self, _, ev: &OpenFile, _, cx| {
                v.open_location_range_at(ev.row.unwrap_or(v.thread.entries.len()), PathBuf::from(&ev.path), ev.line, ev.end_line, cx);
            }),
        ];
        // Before the composer's own bindings, which would take Escape, Up, Down and Shift+Tab.
        subscriptions.push(cx.intercept_keystrokes(move |ev: &KeystrokeEvent, window, cx| {
            let _ = this.update(cx, |v, cx| v.intercept(ev, window, cx));
        }));
        let mut thread = Thread::default();
        thread.entries = start.prior_entries.clone();
        // A tool call left `Running` in persisted history belonged to an earlier process (e.g. before a crash or
        // reboot) and cannot still be running in the new session.
        settle_running_tools(&mut thread.entries);
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
        // One row past the entries holds the turn's working or "Worked for" line.
        let list = ListState::new(rows + 1, ListAlignment::Top, px(600.0));
        list.set_follow_mode(FollowMode::Tail);
        let cached_replay = start.session_id.is_some()
            && !start.prior_entries.is_empty()
            && start.harness.adapter().reopen == Reopen::Load;
        let took_over = start.session_id.is_none() && !start.prior_entries.is_empty();
        let turn_reviews = if start.prior_entries.is_empty() { Vec::new() } else { review::load_turn_reviews(&start.record_id) };
        let canonical_history = !turn_reviews.is_empty()
            || start.prior_entries.iter().any(|entry| needs_history_seed(entry) || matches!(entry, Entry::WorkspaceBoundary(_)));
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
            turn_seq: 0,
            pending_idle: None,
            turn_started: None,
            last_turn: None,
            last_update: None,
            queue,
            queue_held,
            workspace_switching: false,
            permission_changing: false,
            read_only_unconfirmed: false,
            queue_edit: None,
            preparing: None,
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
            fallback_model: None,
            auto_handoff_on_limit: true,
            runway: Default::default(),
            rows,
            detail: false,
            subagent_edits: None,
            initial_permission: start.permission,
            antigravity_full_access: false,
            auto_fallback: false,
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
            picker_nav: crate::panels::ui::command::Nav::new(cx),
            menu_nav: crate::panels::ui::command::Nav::new(cx),
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
            card_ui,
            snapshots: Vec::new(),
            turn_reviews,
            active_turn_review: None,
            command_highlight: 0,
            command_scroll: Default::default(),
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

    fn start(&mut self, mut start: Start, cx: &mut Context<Self>) {
        let harness = start.harness;
        cx.spawn(async move |this, cx| {
            if let Some(mut tools) = start.mcp_tools.take() {
                let task = cx.background_executor().spawn(async move {
                    tools.load_workspace();
                    Ok(tools)
                });
                match startup_deadline("reading the workspace", task, cx).await {
                    Ok(tools) => start.mcp_tools = Some(tools),
                    Err(error) => {
                        let _ = this.update(cx, |v, cx| v.ended(Some(error), cx));
                        return;
                    }
                }
            }
            let mut full_access = harness.adapter().full_access_relaunch
                && start.permission == Some(Permission::FullAccess)
                && start.starter != Starter::OtherSession;
            let (connection, mut incoming, opened) = loop {
                let (events, incoming) = futures::channel::mpsc::unbounded();
                let mut plan = crate::harness::SessionPlan {
                    env: start.env.clone(),
                    args: start.launch_args.clone(),
                    mcp_servers: start.mcp_servers.clone(),
                    ..Default::default()
                };
                if let Some(tools) = &start.mcp_tools
                    && let Err(error) = (harness.adapter().configure_mcp)(tools, &mut plan)
                {
                    let _ = this.update(cx, |v, cx| v.ended(Some(error), cx));
                    return;
                }
                let launched = startup_deadline("launch", launch_session(&start, &plan, full_access, events.clone(), cx), cx).await;
                let connection = match launched {
                    Ok(c) => Rc::new(c),
                    Err(why) => {
                        let _ = this.update(cx, |v, cx| v.ended(Some(why), cx));
                        return;
                    }
                };
                if this.update(cx, |v, _| {
                    v.connection = Some(connection.clone());
                    v.events = Some(events);
                }).is_err() {
                    return;
                }
                let opened = startup_deadline("opening the session", open(&connection, harness, &start, &plan.mcp_servers), cx).await;
                if harness.adapter().full_access_relaunch && !full_access && start.permission == Some(Permission::Auto)
                    && start.starter != Starter::OtherSession && let Ok(session) = &opened
                {
                    let mut thread = Thread::default();
                    thread.apply_session(&session.result);
                    if Permission::Auto.listed(harness, &thread.modes).is_none()
                        && Permission::FullAccess.listed(harness, &thread.modes).is_some()
                    {
                        connection.terminate();
                        if let Some(tools) = &mut start.mcp_tools {
                            tools.renew();
                        }
                        full_access = true;
                        continue;
                    }
                }
                break (connection, incoming, opened);
            };
            let _ = this.update(cx, |v, _| v.antigravity_full_access = full_access);
            let Some(sid) = this.update(cx, |v, cx| v.opened(opened, &start, cx)).ok().flatten() else {
                return;
            };
            let mut options = start.options.iter().collect::<Vec<_>>();
            // A model change can replace the effort option and its available values.
            options.sort_by_key(|(category, _)| *category != "model");
            for (category, value) in options {
                let Ok(config_id) = this.update(cx, |v, cx| {
                    while let Ok(event) = incoming.try_recv() {
                        v.on_event(event, cx);
                    }
                    v.initial_config_id(category, value, cx)
                }) else { return };
                let Some(config_id) = config_id else { continue };
                let response = startup_deadline(
                    "applying session options",
                    connection.request("session/set_config_option", json!({"sessionId": &sid, "configId": &config_id, "value": value})),
                    cx,
                ).await;
                let Ok(()) = this.update(cx, |v, cx| {
                    while let Ok(event) = incoming.try_recv() {
                        v.on_event(event, cx);
                    }
                    match response {
                        Ok(result) => {
                            v.thread.apply_session(&result);
                            if result.get("configOptions").is_none()
                                && let Some(option) = v.thread.config.iter_mut().find(|o| o.id == config_id)
                            {
                                option.current.clone_from(value);
                            }
                            v.report_chosen(cx);
                            if v.thread.option(category).is_none_or(|option| option.current != *value) {
                                v.config_error(category, value, "The adapter kept a different value.", cx);
                            }
                        }
                        Err(why) => v.config_error(category, value, &why, cx),
                    }
                    cx.notify();
                }) else { return };
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
            if let Some(wanted) = start.permission {
                let Ok((chosen, mode, config, current, reported_modes)) = this.update(cx, |v, _| {
                    let offered = v.permissions();
                    let chosen = if full_access {
                        Some(Permission::FullAccess)
                    } else {
                        defaults::initial_permission(wanted, &offered, start.starter)
                    };
                    let mode = chosen.and_then(|p| p.listed(harness, &v.thread.modes));
                    let config = v.thread.option("mode").map(|opt| opt.id.clone());
                    (chosen, mode, config, v.thread.mode.clone(), !v.thread.modes.is_empty())
                }) else { return };
                let response = match mode {
                    Some(mode) if current.as_deref() == Some(mode) => Ok(Value::Null),
                    Some(mode) => startup_deadline("applying permissions", request_mode(&connection, &sid, mode, config.as_deref()), cx).await,
                    None => Err(format!("{} couldn't apply {} permissions.", harness.name(), wanted.label())),
                };
                let Ok(applied) = this.update(cx, |v, cx| {
                    while let Ok(event) = incoming.try_recv() {
                        v.on_event(event, cx);
                    }
                    response.and_then(|result| {
                        let mut reported = Thread::default();
                        reported.apply_session(&result);
                        let confirmed = reported.mode.as_deref().or_else(|| {
                            (v.thread.mode != current).then_some(v.thread.mode.as_deref()).flatten()
                        });
                        if confirmed.is_some() && confirmed != mode
                            || confirmed.is_none() && config.is_some() && current.as_deref() != mode
                        {
                            return Err(format!("{} did not confirm {} permissions.", harness.name(), wanted.label()));
                        }
                        v.thread.apply_session(&result);
                        v.thread.mode = mode.map(String::from);
                        v.auto_fallback = wanted == Permission::Auto && chosen == Some(Permission::FullAccess);
                        Ok(())
                    })
                }) else { return };
                if let Err(why) = applied
                    && (wanted == Permission::ReadOnly || reported_modes || matches!(start.starter, Starter::Handoff | Starter::OtherSession) || full_access)
                {
                    connection.terminate();
                    let _ = this.update(cx, |v, cx| v.ended(Some(why), cx));
                    return;
                }
            }
            if let Some(tools) = start.mcp_tools.take().filter(|t| start.server.is_none() && t.can_signal_ready()) {
                let result = wait_for_mcp(&tools.ready).await;
                if let Err(why) = result {
                    connection.terminate();
                    let _ = this.update(cx, |v, cx| v.ended(Some(why), cx));
                    return;
                }
                while let Ok(event) = incoming.try_recv() {
                    if this.update(cx, |v, cx| v.on_event(event, cx)).is_err() {
                        return;
                    }
                }
            }
            let ready = this.update(cx, |v, cx| {
                if v.phase != Phase::Starting { return false; }
                v.phase = Phase::Ready;
                v.initial_permission = None;
                v.cached_replay = false;
                v.settle_running_tools();
                cx.emit(ThreadEvent::Ready(v.session_id.clone().unwrap_or_default()));
                if let Some(wanted) = start.permission
                    && v.handoff_permission() == Some(wanted)
                {
                    cx.emit(ThreadEvent::PermissionPreference(wanted));
                }
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

    /// The session is open, or failed to open. Returns the session ID on success.
    fn opened(&mut self, opened: Result<Opened, String>, start: &Start, cx: &mut Context<Self>) -> Option<String> {
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
        self.report_chosen(cx);
        self.remeasure(0);
        cx.notify();
        Some(session_id)
    }

    fn initial_config_id(&mut self, category: &str, value: &str, cx: &mut Context<Self>) -> Option<String> {
        if self.phase != Phase::Starting {
            return None;
        }
        match self.thread.option(category) {
            Some(option) if option.current == value => None,
            Some(option) if option.choices.iter().any(|(choice, _)| choice == value) => Some(option.id.clone()),
            None if category == "thought_level" && value == defaults::Effort::default().name() => None,
            _ => {
                self.config_error(category, value, "The adapter did not offer this value.", cx);
                None
            }
        }
    }

    fn config_error(&mut self, category: &str, value: &str, why: &str, cx: &mut Context<Self>) {
        let label = if category == "thought_level" { "effort" } else { category };
        let at = self.thread.notice(format!("Could not set {label} to {value}. {why}"), true);
        self.remeasure(at);
        cx.notify();
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        if event.session_id().is_some_and(|id| Some(id) != self.session_id.as_deref()) {
            if let Some(connection) = &self.connection {
                match event {
                    Event::Permission { id, .. } => connection.respond(id, acp::permission_outcome(None)),
                    Event::Elicitation { id, .. } => connection.respond(id, acp::elicitation_cancel()),
                    _ => {}
                }
            }
            return;
        }
        match event {
            Event::Update { update, .. } => {
                let protocol = &self.harness.adapter().protocol;
                if (protocol.ignore_update)(&update) {
                    return;
                }
                if update["sessionUpdate"] == "_m4code/session_ended" {
                    self.ended(update["error"].as_str().map(String::from), cx);
                    return;
                }
                if update["sessionUpdate"] == "_m4code/usage_limits" {
                    if let Some(limits) = (protocol.usage_limits)(&update["limits"], crate::store::now()) {
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
                let was_live_bg = self.has_live_background_tasks();
                // A tool finishing, or a late update to one already settled, is not the agent being busy again.
                let is_tool_terminal = update["sessionUpdate"] == "tool_call_update"
                    && (matches!(update["status"].as_str(), Some("completed" | "failed"))
                        || update["toolCallId"].as_str().and_then(|id| self.thread.tool(id)).is_some_and(|call| call.status != ToolStatus::Running));
                if self.phase == Phase::Ready && !self.cached_replay && is_conversation_update(&update)
                    && (self.working || !is_tool_terminal)
                {
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
                        let call_id = call.id.clone();
                        self.tool_images.get_mut().retain(|(id, _), _| *id != call_id);
                        if self.phase == Phase::Ready && !self.cached_replay
                            && let Some(hooks) = &protocol.background
                        {
                            let background = (hooks.from_update)(call, &update);
                            if let Some(task) = background.started
                                && self.seen_background_tasks.insert(task.id.clone())
                            {
                                self.background_tasks.push(task);
                            }
                            for id in &background.finished {
                                self.retire_background_task(id);
                            }
                            for text in background.output {
                                self.retire_finished_background_tasks(&text);
                            }
                        }
                        if self.shell_output.as_ref().is_some_and(|out| out.task_id == call_id) {
                            self.open_tool_output(&call_id, cx);
                        }
                    }
                    if self.phase == Phase::Ready && !self.cached_replay && is_conversation_update(&update) {
                        if matches!(self.thread.entries.get(at), Some(Entry::Thought(_))) {
                            self.thought_timing.entry(at).or_insert((Instant::now(), None));
                        } else {
                            self.finish_open_thoughts();
                        }
                    }
                    if protocol.async_ask.is_some() && is_conversation_update(&update) {
                        self.asks.record_message_chunk(at, &update);
                    }
                    self.sync_markdown(at, cx);
                    self.remeasure(at);
                }
                if was_live_bg && !self.has_live_background_tasks() && !self.working {
                    self.last_update = None;
                    if let Some(outcome) = self.pending_idle.take()
                        && (self.queue.is_empty() || self.queue_held)
                    {
                        cx.emit(ThreadEvent::Idle(outcome));
                    }
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
                if self.interrupting {
                    if let Some(connection) = &self.connection {
                        connection.respond(id, acp::permission_outcome(None));
                    }
                    return;
                }
                // m4code's own tools act only on what the user asked for, so their prompts are answered yes. The
                // request may leave the title out and name a call the chat already shows.
                let call = params.pointer("/toolCall/toolCallId").and_then(Value::as_str).and_then(|call| self.thread.tool_at(call));
                let shown = call.and_then(|at| match &self.thread.entries[at] { Entry::Tool(call) => Some(call.title.as_str()), _ => None });
                let shown_kind = call.and_then(|at| match &self.thread.entries[at] { Entry::Tool(call) => Some(call.kind.as_str()), _ => None });
                let title = params.pointer("/toolCall/title").and_then(Value::as_str).or(shown).unwrap_or_default();
                // A shell command or an edit is never one of m4code's tools, whatever its title says.
                let kind = params.pointer("/toolCall/kind").and_then(Value::as_str).or(shown_kind).unwrap_or_default();
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
                let multi_select = acp::is_multi_select(&params);
                self.questions.push_back(Question {
                    id: Uuid::new_v4(),
                    reply: QuestionReply::Acp(id),
                    title,
                    options,
                    highlight: 0,
                    multi_select,
                    selected: BTreeSet::new(),
                    takes_own_answer,
                });
            }
            Event::Elicitation { id, params } => {
                if self.interrupting {
                    if let Some(connection) = &self.connection {
                        connection.respond(id, acp::elicitation_cancel());
                    }
                    return;
                }
                if let Some(decode) = self.harness.adapter().protocol.async_ask
                    && let Some(ask) = decode(&params) {
                    // The adapter can only steer an answer into the original turn. Own the reply here so it can
                    // reach Codex as a follow-up even after that turn ends.
                    if let Some(connection) = &self.connection {
                        connection.respond(id, acp::elicitation_decline());
                    }
                    self.show_async_ask(ask, cx);
                    return;
                }
                let mut fields: VecDeque<acp::ElicitationField> = acp::parse_elicitation(&params).into();
                if let Some(first) = fields.pop_front() {
                    let multi_select = first.kind == acp::ElicitationKind::MultiChoice;
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
                        multi_select,
                        selected: BTreeSet::new(),
                        takes_own_answer: true,
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
        self.settle_running_tools();
        self.background_tasks.clear();
        self.pending_idle = None;
        self.shell_output = None;
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

    fn retire_background_task(&mut self, task_id: &str) {
        if self.background_tasks.is_empty() {
            return;
        }
        self.background_tasks.retain(|task| task.id != task_id);
        self.prune_closed_shell_output();
    }

    fn retire_finished_background_tasks(&mut self, text: &str) {
        if self.background_tasks.is_empty() {
            return;
        }
        if let Some(hooks) = &self.harness.adapter().protocol.background {
            self.background_tasks.retain(|task| !(hooks.finished)(text, &task.id));
        }
        self.prune_closed_shell_output();
    }

    fn prune_closed_shell_output(&mut self) {
        if self.shell_output.as_ref().is_some_and(|out| {
            !self.provider_shells.iter().any(|s| s.id == out.task_id)
                && !self.background_tasks.iter().any(|t| t.id == out.task_id)
        }) {
            self.shell_output = None;
        }
    }

    fn open_tool_output(&mut self, call_id: &str, cx: &mut Context<Self>) {
        let Some(Entry::Tool(call)) = self.thread.entries.iter().find(|e| matches!(e, Entry::Tool(c) if c.id == call_id)) else {
            return;
        };
        let mut parts: Vec<&str> = Vec::new();
        if let Some(out) = call.details.output.as_deref().filter(|s| !s.trim().is_empty()) {
            parts.push(out);
        }
        if let Some(out) = call.details.stdout.as_deref().filter(|s| !s.trim().is_empty()) {
            parts.push(out);
        }
        if let Some(out) = call.details.stderr.as_deref().filter(|s| !s.trim().is_empty()) {
            parts.push(out);
        }
        for content in &call.content {
            if let crate::harness::thread::ToolContent::Text(t) = content
                && !t.trim().is_empty()
            {
                parts.push(t.as_str());
            }
        }
        let text = if parts.is_empty() {
            "Running in background — output will appear here when available.".into()
        } else {
            let joined = parts.join("\n");
            const LIMIT: usize = 16 * 1024;
            if joined.len() > LIMIT {
                let mut start = joined.len() - LIMIT;
                while !joined.is_char_boundary(start) {
                    start += 1;
                }
                joined[start..].to_string()
            } else {
                joined
            }
        };
        self.shell_output = Some(ShellOutput { task_id: call_id.to_string(), text, loading: false });
        cx.notify();
    }

    fn open_background_task_output(&mut self, task_id: &str, cx: &mut Context<Self>) {
        let Some(path) = self.background_tasks.iter().find(|t| t.id == task_id).and_then(|t| t.log_path.clone()) else {
            return;
        };
        let text = match std::fs::read(&path) {
            Ok(bytes) if bytes.is_empty() => "The task has produced no output yet.".into(),
            Ok(bytes) => {
                const LIMIT: usize = 16 * 1024;
                let tail = if bytes.len() > LIMIT { &bytes[bytes.len() - LIMIT..] } else { &bytes[..] };
                String::from_utf8_lossy(tail).into_owned()
            }
            Err(_) => "Live output is unavailable. Open the finished command to see its output.".into(),
        };
        self.shell_output = Some(ShellOutput { task_id: task_id.to_string(), text, loading: false });
        cx.notify();
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
        if self.workspace_switching || self.read_only_unconfirmed {
            return;
        }
        if self.queue_edit.is_some() {
            self.finish_queue_edit(true, window, cx);
            return;
        }
        let text = self.composer.read(cx).value().trim().to_string();
        if !self.can_submit_draft(cx) {
            if self.queue_held && !self.working && !self.is_waiting() {
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
        if self.active_question().is_some_and(|q| q.takes_own_answer) {
            self.answer_in_own_words(&text, cx);
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
            && (self.harness.adapter().steers || self.native_steering())
            && self.working
            && !self.queue_held
            && !self.workspace_switching
            && !self.permission_changing
            && !self.read_only_unconfirmed
            && self.preparing.is_none()
            && self.handoff_after_turn.is_none()
            && self.questions.is_empty()
            && self.phase == Phase::Ready
            && !self.compaction.running
            && !self.running_command
            && !text.starts_with('/')
    }

    fn native_steering(&self) -> bool {
        !self.harness.adapter().steers
            && self.agent_meta.pointer("/steering/supported").and_then(Value::as_bool) == Some(true)
    }

    fn steer(&mut self, item: Queued, cx: &mut Context<Self>) {
        let (Some(connection), Some(session)) = (self.connection.clone(), self.session_id.clone()) else {
            self.queue.push_back(item);
            self.queue_changed();
            cx.notify();
            return;
        };
        let native = self.native_steering();
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
        if item.hidden {
            return None;
        }
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
        if self.working || self.phase != Phase::Ready || self.workspace_switching || self.permission_changing || self.read_only_unconfirmed
            || !self.steering.is_empty() || self.is_waiting() || self.restoring_checkpoint()
        {
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
            && self.thread.entries.last().is_some_and(needs_history_seed))
            .then(|| self.thread.transcript(400));
        let mut shown_row = if next.shown {
            self.thread.entries.iter().rposition(|entry| matches!(entry, Entry::User(text) if text == &next.text))
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
        self.turn_seq = self.turn_seq.wrapping_add(1);
        self.pending_idle = None;
        self.turn_started = Some(Instant::now());
        self.last_turn = None;
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
                if !next.compact && !next.text.starts_with('/') {
                    cx.emit(ThreadEvent::PromptSent(next.text.clone()));
                }
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

    // Tool calls in the transcript belong to the turn (real background work lives in `provider_shells`,
    // `background_tasks`, or `background`). A missing final tool update leaves no exit status.
    fn settle_running_tools(&mut self) {
        if let Some(row) = settle_running_tools(&mut self.thread.entries) {
            self.remeasure(row);
        }
    }

    fn turn_ended(&mut self, result: Result<Value, String>, cx: &mut Context<Self>) {
        let keep_live_execute = result.as_ref().is_ok_and(|r| r["stopReason"] != "cancelled")
            && self.harness.adapter().protocol.background.as_ref().is_some_and(|b| b.live_execute_tools);
        if let Some(row) = settle_running_tools_filtered(&mut self.thread.entries, keep_live_execute) {
            self.remeasure(row);
        }
        self.working = false;
        self.last_turn = self.turn_started.take().map(|t| t.elapsed());
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
        if let Some((text, error)) = &notice {
            let at = self.thread.notice(text.clone(), *error);
            self.remeasure(at);
        }
        let hit_limit_reply = notice.as_ref().is_some_and(|(t, _)| crate::harness::limits::looks_like_usage_limit_error(t))
            || matches!(self.thread.entries.last(), Some(Entry::Agent(text)) if crate::harness::limits::looks_like_usage_limit_error(text));
        if hit_limit_reply && self.auto_handoff_on_limit && self.phase == Phase::Ready
            && let Some(target) = self.fallback_model.clone()
        {
            let at = self.thread.notice(format!("Usage limit reached on {} — handing off to {}…", self.harness.name(), target.name), false);
            self.remeasure(at);
            cx.emit(ThreadEvent::Handoff {
                to: target.harness,
                model: (!target.id.is_empty()).then_some(target.id),
                message: String::new(),
                effort: None,
                by_agent: true,
            });
        }
        if matches!(outcome, "failed" | "interrupted") {
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
        let waiting_on_background = idle && self.has_live_background_tasks();
        self.send_next(cx);
        if waiting_on_background && !self.working {
            self.pending_idle = Some(outcome);
        } else if idle && !waiting_on_background {
            self.pending_idle = None;
            cx.emit(ThreadEvent::Idle(outcome));
        } else if !self.working {
            self.pending_idle = None;
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
            self.turn_started = None;
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
        self.pending_idle = None;
        if !self.working && self.has_live_background_tasks() {
            self.settle_running_tools();
            self.background_tasks.clear();
            self.prune_closed_shell_output();
        }
        if !self.interrupting && let (Some(c), Some(s)) = (&self.connection, &self.session_id) {
            if self.working {
                self.interrupting = true;
            }
            c.notify("session/cancel", json!({"sessionId": s}));
        }
        cx.notify();
    }

    /// The active question shown in the composer: turn-scoped questions (permissions, sync elicitations, MCP bridge)
    /// always take priority over turn-surviving Codex async asks.
    fn active_question(&self) -> Option<&Question> {
        self.questions.front().or_else(|| self.asks.front())
    }

    fn active_question_mut(&mut self) -> Option<&mut Question> {
        if !self.questions.is_empty() {
            self.questions.front_mut()
        } else {
            self.asks.front_mut()
        }
    }

    fn pop_active_question(&mut self) -> Option<Question> {
        self.questions.pop_front().or_else(|| self.asks.pop_front())
    }

    pub(super) fn pending_question_count(&self) -> usize {
        self.questions.len() + self.asks.len()
    }

    /// Show a multiple-choice question from m4code's MCP `ask_user` tool (works on every harness, including ones
    /// whose native question tool isn't bridged over ACP).
    pub fn ask_bridge_question(&mut self, req_id: String, title: String, options: Vec<QuestionOption>, multi_select: bool, cx: &mut Context<Self>) {
        if self.interrupting {
            crate::mcp::bridge::respond(&req_id, false, "The turn was interrupted.");
            return;
        }
        self.questions.push_back(Question {
            id: Uuid::new_v4(),
            reply: QuestionReply::Bridge(req_id),
            title,
            options,
            highlight: 0,
            multi_select,
            selected: BTreeSet::new(),
            takes_own_answer: true,
        });
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
                QuestionReply::AsyncAsk { .. } => {}
            }
        }
    }

    pub(crate) fn answer_question(&mut self, id: Uuid, choice: Option<usize>, cx: &mut Context<Self>) -> bool {
        let Some(q) = self.active_question() else { return false };
        if q.id != id {
            return false;
        }
        match choice {
            Some(ix) if q.multi_select => self.toggle_question_option(id, ix, cx),
            Some(ix) => {
                self.answer(Some(ix), cx);
                true
            }
            None => {
                self.decline(cx);
                true
            }
        }
    }

    /// Toggle a checkbox option on the active multi-select question.
    pub(crate) fn toggle_question_option(&mut self, id: Uuid, index: usize, cx: &mut Context<Self>) -> bool {
        let Some(q) = self.active_question_mut() else { return false };
        if q.id != id || !q.multi_select || index >= q.options.len() {
            return false;
        }
        if !q.selected.remove(&index) {
            q.selected.insert(index);
        }
        q.highlight = index;
        cx.notify();
        true
    }

    /// Submit all checked options on the active multi-select question.
    pub(crate) fn submit_multi_question(&mut self, id: Uuid, cx: &mut Context<Self>) -> bool {
        if !self.active_question().is_some_and(|q| q.id == id && q.multi_select && !q.selected.is_empty()) {
            return false;
        }
        self.submit_selected(None, cx);
        true
    }

    fn submit_selected(&mut self, extra_text: Option<&str>, cx: &mut Context<Self>) {
        let Some(q) = self.pop_active_question() else { return };
        let chosen: Vec<&QuestionOption> = q.selected.iter().filter_map(|&i| q.options.get(i)).collect();
        let extra = extra_text.map(str::trim).filter(|s| !s.is_empty());
        let joined = || {
            let mut parts: Vec<&str> = chosen.iter().map(|o| o.name.as_str()).collect();
            if let Some(t) = extra {
                parts.push(t);
            }
            parts.join(", ")
        };
        match q.reply {
            QuestionReply::Acp(id) => {
                let ids: Vec<String> = chosen.iter().map(|o| o.id.clone()).collect();
                if let Some(c) = &self.connection {
                    c.respond(id, acp::multi_answer(&ids, extra));
                }
            }
            QuestionReply::Elicitation { id, field, kind, mut answers, mut remaining } => {
                if extra.is_some() {
                    if let Some(c) = &self.connection {
                        c.respond(id, acp::elicitation_decline());
                    }
                    let text = joined();
                    if !text.is_empty() {
                        self.queue_quoted_answer(&q.title, &text, cx);
                    }
                } else {
                    let ids: Vec<&str> = chosen.iter().map(|o| o.id.as_str()).collect();
                    answers.insert(field, kind.to_json_values(&ids));
                    if let Some(next) = remaining.pop_front() {
                        let multi_select = next.kind == acp::ElicitationKind::MultiChoice;
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
                            multi_select,
                            selected: BTreeSet::new(),
                            takes_own_answer: true,
                        });
                    } else if let Some(c) = &self.connection {
                        c.respond(id, acp::elicitation_accept(answers));
                    }
                }
            }
            QuestionReply::Bridge(req_id) => crate::mcp::bridge::respond(&req_id, true, joined()),
            QuestionReply::AsyncAsk { remaining, replies } => {
                let text = joined();
                self.finish_async_ask(q.title, remaining, replies, (!text.is_empty()).then_some(text), cx);
            }
        }
        self.send_next(cx);
        cx.notify();
    }

    /// Answer the open permission/ask question with option `index` (from 0), or turn it down.
    pub fn answer(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let Some(q) = self.pop_active_question() else { return };
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
                    answers.insert(field, kind.to_json_values(&[&opt.id]));
                    if let Some(next) = remaining.pop_front() {
                        let multi_select = next.kind == acp::ElicitationKind::MultiChoice;
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
                            multi_select,
                            selected: BTreeSet::new(),
                            takes_own_answer: true,
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
            QuestionReply::AsyncAsk { remaining, replies } => {
                self.finish_async_ask(q.title, remaining, replies, chosen.map(|o| o.name.clone()), cx);
            }
        }
        self.send_next(cx);
        cx.notify();
    }

    /// Answer the open question with what the user typed rather than one of its options.
    fn answer_in_own_words(&mut self, text: &str, cx: &mut Context<Self>) {
        if !self.active_question().is_some_and(|q| q.takes_own_answer) { return; }
        if self.active_question().is_some_and(|q| q.multi_select && !q.selected.is_empty()) {
            self.submit_selected(Some(text), cx);
            return;
        }
        let Some(q) = self.pop_active_question() else { return };
        match q.reply {
            QuestionReply::Acp(id) => {
                if let Some(c) = &self.connection {
                    c.respond(id, acp::own_answer(text));
                }
            }
            QuestionReply::Bridge(req_id) => crate::mcp::bridge::respond(&req_id, true, text),
            QuestionReply::AsyncAsk { remaining, replies } => {
                self.finish_async_ask(q.title, remaining, replies, Some(text.to_string()), cx);
            }
            QuestionReply::Elicitation { id, .. } => {
                if let Some(c) = &self.connection {
                    c.respond(id, acp::elicitation_decline());
                }
                self.queue_quoted_answer(&q.title, text, cx);
            }
        }
        self.send_next(cx);
        cx.notify();
    }

    fn queue_quoted_answer(&mut self, title: &str, answer: &str, cx: &mut Context<Self>) {
        let quoted = title.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n");
        let mut item = Queued::new(format!("{quoted}\n\n{answer}"), true);
        item.ahead = true;
        if self.native_steering() && self.can_steer(&item.text, crate::store::FollowUp::Steer) {
            self.steer(item, cx);
        } else {
            let at = self.queue.iter().take_while(|entry| entry.ahead).count();
            self.queue.insert(at, item);
            self.queue_changed();
        }
    }

    /// Move the open question's highlight to option `index`, as the mouse does.
    pub(super) fn highlight_option(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(q) = self.active_question_mut()
            && q.highlight != index
        {
            q.highlight = index;
            cx.notify();
        }
    }

    /// Turn the question down with its own reject option when it has one.
    pub fn decline(&mut self, cx: &mut Context<Self>) {
        let reject = self.active_question().and_then(|q| q.options.iter().position(|o| o.kind == "reject_once"));
        self.answer(reject, cx);
    }

    pub(crate) fn permissions(&self) -> Vec<Permission> {
        if self.subagent_edits.is_some() { return Vec::new(); }
        permission::offered(self.harness, &self.thread.modes)
    }

    pub(crate) fn permission(&self) -> Option<Permission> {
        if self.antigravity_full_access {
            Some(Permission::FullAccess)
        } else {
            Permission::of(self.harness, self.thread.mode.as_deref()?)
        }
    }

    pub(crate) fn handoff_permission(&self) -> Option<Permission> {
        match self.permission()? {
            Permission::FullAccess if self.auto_fallback => Some(Permission::Auto),
            permission => Some(permission),
        }
    }

    pub(crate) fn composer_state(&self, cx: &App) -> ComposerState {
        ComposerState {
            text: self.composer.read(cx).value().to_string(),
            attachments: self.attachments.clone(),
            permission: self.permission(),
            git: self.git.clone(),
        }
    }

    pub(crate) fn restore_composer(&mut self, state: ComposerState, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |composer, cx| composer.set_value(state.text, window, cx));
        self.attachments = state.attachments;
        if self.phase == Phase::Starting {
            self.initial_permission = self.initial_permission.or(state.permission);
            self.git = state.git;
        }
        cx.notify();
    }

    pub(crate) fn hold_workspace_switch(&mut self, holding: bool, cx: &mut Context<Self>) {
        self.workspace_switching = holding;
        if !holding {
            self.send_next(cx);
        }
        cx.notify();
    }

    pub(crate) fn can_move_workspace(&self) -> bool {
        self.phase == Phase::Ready && !self.has_active_turn() && !self.is_waiting() && !self.has_background_work()
            && !self.permission_changing
    }

    pub(crate) fn permission_for_relaunch(&self) -> Option<Permission> {
        if self.read_only_unconfirmed {
            return Some(Permission::ReadOnly);
        }
        let permission = self.permission()?;
        if self.antigravity_full_access || permission.native(self.harness) == self.thread.mode.as_deref() {
            self.handoff_permission()
        } else {
            None
        }
    }

    pub(crate) fn antigravity_full_access(&self) -> bool {
        self.antigravity_full_access
    }

    pub(crate) fn permission_error(&mut self, why: String, cx: &mut Context<Self>) {
        let at = self.thread.notice(why, true);
        self.remeasure(at);
        cx.notify();
    }

    pub(crate) fn set_permission(&mut self, wanted: Permission, cx: &mut Context<Self>) {
        if self.phase != Phase::Ready || self.subagent_edits.is_some()
            || self.workspace_switching || self.permission_changing
        {
            return;
        }
        if self.has_active_turn() || self.is_waiting() || self.has_background_work() {
            self.permission_error("Wait for the current work to finish before changing permissions.".into(), cx);
            return;
        }
        let Some(permission) = wanted.resolve(&self.permissions()) else { return };
        let Some(mode) = permission.listed(self.harness, &self.thread.modes) else { return };
        let (Some(connection), Some(session)) = (self.connection.clone(), self.session_id.clone()) else { return };
        if self.harness.adapter().full_access_relaunch && self.antigravity_full_access != (permission == Permission::FullAccess) {
            self.require_read_only(wanted, cx);
            cx.emit(ThreadEvent::ReconnectPermission(wanted));
            return;
        }
        let auto_fallback = wanted == Permission::Auto && permission == Permission::FullAccess;
        if self.thread.mode.as_deref() == Some(mode) {
            self.auto_fallback = auto_fallback;
            self.read_only_unconfirmed = false;
            cx.emit(ThreadEvent::PermissionPreference(wanted));
            self.send_next(cx);
            return;
        }
        let current = self.thread.mode.clone();
        let config = self.thread.option("mode").map(|opt| opt.id.clone());
        let reply = request_mode(&connection, &session, mode, config.as_deref());
        self.permission_changing = true;
        self.require_read_only(wanted, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = startup_deadline("changing permissions", reply, cx).await;
            let _ = this.update(cx, |v, cx| {
                v.permission_changing = false;
                match result {
                    Ok(result) => {
                        let mut reported = Thread::default();
                        reported.apply_session(&result);
                        let confirmed = reported.mode.or_else(|| (v.thread.mode != current).then(|| v.thread.mode.clone()).flatten());
                        v.thread.apply_session(&result);
                        if confirmed.as_deref().is_some_and(|id| id != mode) || (confirmed.is_none() && config.is_some()) {
                            v.thread.mode = confirmed.or(current);
                            v.queue_held = true;
                            v.permission_error(format!("{} did not confirm {} permissions.", v.harness.name(), wanted.label()), cx);
                        } else {
                            v.thread.mode = Some(mode.into());
                            v.auto_fallback = auto_fallback;
                            v.read_only_unconfirmed = false;
                            cx.emit(ThreadEvent::PermissionPreference(wanted));
                            v.send_next(cx);
                        }
                    }
                    Err(why) => {
                        v.queue_held = true;
                        v.permission_error(why, cx);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    fn require_read_only(&mut self, wanted: Permission, cx: &mut Context<Self>) {
        if wanted == Permission::ReadOnly {
            self.read_only_unconfirmed = true;
            cx.emit(ThreadEvent::PermissionPreference(wanted));
        }
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
            self.close_chat_menu(window, cx);
            cx.stop_propagation();
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
        // Left/Right step the Model picker's effort even while its search box has the keyboard. Up, Down and Enter
        // are the list's own (`ui::command::Choices`).
        if self.picker == Some(ThreadPicker::Model)
            && !ev.keystroke.modifiers.modified()
            && matches!(ev.keystroke.key.as_str(), "left" | "right")
        {
            self.step_effort(if ev.keystroke.key == "left" { -1 } else { 1 }, cx);
            cx.stop_propagation();
            cx.notify();
            return;
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
            question: self.active_question().map_or(0, |q| q.options.len()),
            question_open: self.active_question().is_some(),
            question_multi: self.active_question().is_some_and(|q| q.multi_select),
            draft_empty: !self.can_submit_draft(cx),
            in_history: self.history_at.is_some(),
            escape_just_pressed: self.last_escape.is_some_and(|t| t.elapsed() < keys::DOUBLE_PRESS),
            completing: self.completion(&draft, self.composer.read(cx).selected_range().end).is_some(),
            commands: self.matching_commands(cx).len(),
        };
        let is_escape = ev.keystroke.key == "escape";
        if is_escape && self.queue_edit.is_some() && !self.is_waiting() && self.picker.is_none() {
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
            keys::Key::Choose(n) => {
                if let Some(q) = self.active_question()
                    && q.multi_select
                {
                    let id = q.id;
                    self.toggle_question_option(id, n - 1, cx);
                } else {
                    self.answer(Some(n - 1), cx);
                }
            }
            keys::Key::ToggleHighlighted => {
                if let Some(q) = self.active_question()
                    && q.multi_select
                {
                    let (id, at) = (q.id, q.highlight);
                    self.toggle_question_option(id, at, cx);
                }
            }
            keys::Key::ChooseHighlighted => {
                if let Some(q) = self.active_question() {
                    if q.multi_select {
                        if !q.selected.is_empty() {
                            self.submit_selected(None, cx);
                        }
                    } else {
                        let at = q.highlight;
                        self.answer(Some(at), cx);
                    }
                }
            }
            keys::Key::OptionUp | keys::Key::OptionDown => {
                if let Some(q) = self.active_question_mut() {
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
                if self.phase != Phase::Ready || self.workspace_switching || self.permission_changing
                    || self.has_active_turn() || self.is_waiting() || self.has_background_work()
                {
                    return;
                }
                if let Some(next) = permission::next(&self.permissions(), self.permission()) {
                    self.set_permission(next, cx);
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
            let current = models
                .iter()
                .position(|m| match &self.pending_model {
                    Some(p) => p.harness == m.harness && p.id == m.id,
                    None => m.harness == self.harness && current_id == Some(m.id.as_str()),
                })
                .unwrap_or(0);
            self.picker_nav.reset_to(current);
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
        if self.active_question().is_some_and(|q| q.takes_own_answer) {
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
        if self.queue_edit.is_some() || self.active_question().is_some_and(|q| q.takes_own_answer) {
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
        let accent = crate::panels::theme::theme().codex;
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

    pub(crate) fn persist_entries(&self) {
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
        self.run_cache.get_mut().ranges.clear();
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
            && !self.list.is_scrolled_to_end().unwrap_or_else(|| self.list.max_offset_for_scrollbar().y <= px(1.0))
    }

    fn open_location_range_at(&mut self, row: usize, path: PathBuf, line: Option<u32>, end_line: Option<u32>, cx: &mut Context<Self>) {
        let normalized = render::normalize_tool_path(&path);
        let path = if normalized.is_absolute() {
            normalized
        } else {
            timeline::workspace_at(&self.thread.entries, row, &self.cwd).join(normalized)
        };
        cx.emit(ThreadEvent::OpenPath(path.to_string_lossy().into_owned(), line.map(|l| l as usize), end_line.map(|l| l as usize)));
    }

    // What the workspace asks of a session view, as it asks a terminal.

    pub fn is_running(&self) -> bool {
        self.phase != Phase::Ended
    }

    /// Send an internal runway notice without consuming any user-staged model switch or adding a user bubble.
    fn nudge_model(&mut self, text: String, cx: &mut Context<Self>) {
        if self.workspace_switching || self.read_only_unconfirmed {
            return;
        }
        if self.can_steer(&text, crate::store::FollowUp::Steer) {
            self.steer(Queued::hidden(text), cx);
        } else {
            self.queue.push_back(Queued::hidden(text));
            self.send_next(cx);
            cx.notify();
        }
    }

    pub(crate) fn schedule_handoff(&mut self, target: CatalogModel, by_agent: bool, cx: &mut Context<Self>) {
        self.handoff_after_turn = Some((target, None, String::new(), by_agent));
        cx.notify();
    }

    pub(crate) fn is_read_only_unconfirmed(&self) -> bool {
        self.read_only_unconfirmed
    }

    pub(crate) fn is_workspace_switching(&self) -> bool {
        self.workspace_switching
    }

    pub fn observe_usage_limits(&mut self, usage: &crate::store::UsageLimits, auto_handoff: bool, cx: &mut Context<Self>) {
        self.auto_handoff_on_limit = auto_handoff;
        if self.phase != Phase::Ready || self.workspace_switching || self.read_only_unconfirmed {
            return;
        }
        let now = crate::store::now();
        let cumulative_tokens = self.thread.cache_usage.input_tokens
            .saturating_add(self.thread.cache_usage.output_tokens)
            .max(self.thread.context.map(|(used, _)| used).unwrap_or(0));
        if self.is_working()
            && let Some(nudge) = self.runway.observe(usage, cumulative_tokens, auto_handoff, now)
        {
            let at = self.thread.notice(
                format!("Approaching {} usage limit ({}%) — notified the model.", nudge.window_label, nudge.percent_used),
                false,
            );
            self.remeasure(at);
            self.nudge_model(nudge.message, cx);
            cx.notify();
        }
        if auto_handoff
            && let Some(target) = self.fallback_model.clone()
            && self.runway.take_exhausted_handoff(usage, now)
        {
            let at = self.thread.notice(
                format!("Usage limit reached on {} — handing off to {}…", self.harness.name(), target.name),
                false,
            );
            self.remeasure(at);
            if self.has_active_turn() {
                self.schedule_handoff(target, true, cx);
            } else {
                cx.emit(ThreadEvent::Handoff {
                    to: target.harness,
                    model: (!target.id.is_empty()).then_some(target.id),
                    message: String::new(),
                    effort: None,
                    by_agent: true,
                });
            }
            cx.notify();
        }
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

    pub fn turn_seq(&self) -> u64 {
        self.turn_seq
    }

    pub fn is_working(&self) -> bool {
        self.has_active_turn()
            || (self.phase == Phase::Starting && !self.queue.is_empty())
            || (self.phase == Phase::Ready
                && !self.cached_replay
                && (self.has_live_background_tasks()
                    || self.last_update.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(15))))
    }

    /// Provider background tasks or live running tool calls still in flight across turns.
    pub(crate) fn has_live_background_tasks(&self) -> bool {
        !self.background_tasks.is_empty()
            || self.thread.entries.iter().any(|e| matches!(e, Entry::Tool(call) if call.status == ToolStatus::Running))
    }

    /// Work the agent left running when its turn ended: a background shell, a subagent, or a tool call still out.
    /// The agent usually wakes again when it finishes, so its turn ending isn't the session being done.
    pub fn has_background_work(&self) -> bool {
        !self.provider_shells.is_empty()
            || !self.background.is_empty()
            || self.has_live_background_tasks()
    }

    /// Waiting for the user to answer a permission or ask question (including async Codex asks).
    pub fn is_waiting(&self) -> bool {
        !self.questions.is_empty() || !self.asks.is_empty()
    }

    /// Snapshot of the pending question, if any, so the sidebar popover can render and answer it.
    pub fn pending_question(&self) -> Option<PendingQuestion> {
        self.active_question().map(|q| PendingQuestion {
            id: q.id,
            title: q.title.clone(),
            options: q.options.clone(),
            multi_select: q.multi_select,
            selected: q.selected.clone(),
        })
    }

    pub fn pending_question_title(&self) -> Option<String> {
        self.active_question().map(|q| q.title.clone())
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
        self.queue_edit.is_none() && self.active_question().is_none()
    }

    fn can_submit_draft(&self, cx: &App) -> bool {
        if self.workspace_switching || self.read_only_unconfirmed {
            return false;
        }
        crate::panels::composer::can_send(self.composer.read(cx).value().as_ref())
            || (self.attachments_enabled() && self.attachments.has_files())
    }

    pub fn attach_paths(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        if !self.attachments_enabled() {
            return;
        }
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

async fn wait_for_mcp(proof: &crate::mcp::session::Ready) -> Result<(), String> {
    if proof.observed() {
        return Ok(());
    }
    let path = proof.path().to_path_buf();
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let until = Instant::now() + crate::mcp::session::TIMEOUT;
        while !path.is_file() {
            if tx.is_canceled() {
                return;
            }
            if Instant::now() >= until {
                let _ = tx.send(Err("The adapter did not discover m4code's MCP tools before the startup deadline.".into()));
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = tx.send(Ok(()));
    });
    rx.await.unwrap_or_else(|_| Err("The adapter did not discover m4code's MCP tools before the startup deadline.".into()))
}

async fn startup_deadline<T>(step: &str, request: impl Future<Output = Result<T, String>>, cx: &gpui::AsyncApp) -> Result<T, String> {
    use futures::future::{Either, select};
    let timer = cx.background_executor().timer(Duration::from_secs(60));
    match select(std::pin::pin!(request), std::pin::pin!(timer)).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => Err(format!("Timed out while {step}.")),
    }
}

async fn launch_session(start: &Start, plan: &crate::harness::SessionPlan, full_access: bool, events: futures::channel::mpsc::UnboundedSender<Event>, cx: &gpui::AsyncApp) -> Result<Connection, String> {
    let cwd = start.cwd.clone();
    let env = plan.env.clone();
    let server = start.server.clone();
    let harness = start.harness;
    let model = start.options.iter().find(|(k, _)| *k == "model").map(|(_, v)| v.as_str()).filter(|m| !m.is_empty());
    let mut launch_args = plan.args.clone();
    launch_args.extend((harness.adapter().launch_args)(model, full_access));
    // Finding the server can read PATH through the login shell.
    cx.background_executor().spawn(async move {
        let (program, mut args) = match server {
            Some(s) => s,
            None => (harness.adapter().server)()?,
        };
        args.extend(launch_args);
        Connection::start(&Launch { program, args, cwd, env }, events)
    }).await
}

fn request_mode(connection: &Connection, session: &str, mode: &str, config: Option<&str>) -> impl Future<Output = Result<Value, String>> + use<> {
    let (method, params) = if let Some(id) = config {
        ("session/set_config_option", json!({"sessionId": session, "configId": id, "value": mode}))
    } else {
        ("session/set_mode", json!({"sessionId": session, "modeId": mode}))
    };
    connection.request(method, params)
}

fn check_workspace(expected: &std::path::Path, result: &Value) -> Result<(), String> {
    let Some(cwd) = result["cwd"].as_str().or_else(|| result.pointer("/_meta/m4code/cwd").and_then(Value::as_str)) else {
        return Ok(());
    };
    let actual = std::path::Path::new(cwd);
    let expected = dunce::canonicalize(expected).unwrap_or_else(|_| expected.to_path_buf());
    let actual = dunce::canonicalize(actual).unwrap_or_else(|_| actual.to_path_buf());
    if crate::platform::path_key(&expected) != crate::platform::path_key(&actual) {
        return Err(format!("The adapter opened {} instead of {}.", actual.display(), expected.display()));
    }
    Ok(())
}

async fn open(c: &Connection, harness: Harness, start: &Start, mcp_servers: &[Value]) -> Result<Opened, String> {
    let initialized = c.request("initialize", acp::initialize_params()).await.map_err(|e| format!("{} didn't start: {e}", harness.name()))?;
    let capabilities = initialized["agentCapabilities"].clone();
    let agent_meta = initialized["_meta"].clone();
    let mut params = json!({"cwd": start.cwd, "mcpServers": mcp_servers, "_meta": start.meta});
    match &start.session_id {
        None => {
            let result = c.request("session/new", params).await.map_err(|e| format!("{} couldn't start a session: {e}", harness.name()))?;
            check_workspace(&start.cwd, &result)?;
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
            check_workspace(&start.cwd, &result)?;
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

    fn update_event(update: serde_json::Value) -> super::Event {
        super::Event::Update { session_id: None, update }
    }

    fn test_start(cwd: std::path::PathBuf, harness: Harness) -> super::Start {
        super::Start {
            record_id: String::new(), harness, all_models: vec![], picked: vec![], cwd,
            env: vec![], mcp_servers: vec![], mcp_tools: None, launch_args: vec![], meta: serde_json::Value::Null, session_id: None, permission: None, starter: crate::harness::defaults::Starter::User,
            options: vec![], prompt: None, prior_entries: vec![], inline_visuals: true,
            server: Some(("definitely-not-a-real-program-m4code".into(), vec![])),
        }
    }

    #[gpui::test]
    fn shift_tab_keeps_busy_antigravity_composer_unchanged(cx: &mut gpui::TestAppContext) {
        use gpui::Focusable;
        crate::datadir::use_throwaway_folders();
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| {
            super::ThreadView::new(test_start(dir.path().into(), Harness::Antigravity), window, cx)
        });
        cx.update(|window, app| view.update(app, |v, cx| {
            v.phase = super::Phase::Ready;
            v.working = true;
            v.thread.modes = vec![("plan".into(), "Plan".into()), ("default".into(), "Default".into())];
            v.thread.mode = Some("plan".into());
            v.set_draft("Keep this draft", window, cx);
            v.focus_composer(window, cx);
        }));
        cx.simulate_keystrokes("shift-tab");
        cx.update(|window, app| view.update(app, |v, cx| {
            assert_eq!(v.draft(cx), "Keep this draft");
            assert_eq!(v.thread.mode.as_deref(), Some("plan"));
            assert!(v.thread.entries.is_empty(), "disabled cycling must not add an error notice");
            assert!(v.composer.focus_handle(cx).is_focused(window));
        }));
    }

    #[gpui::test]
    fn composer_accepts_picked_dropped_and_pasted_files(cx: &mut gpui::TestAppContext) {
        use gpui::{ClipboardEntry, ClipboardItem, ExternalPaths, FileDropEvent, Image, ImageFormat};

        let (data, _) = crate::datadir::use_throwaway_folders();
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task notes.txt");
        std::fs::write(&path, "notes").unwrap();
        let dropped_path = dir.path().join("drop.txt");
        std::fs::write(&dropped_path, "dropped").unwrap();
        let picked_path = dir.path().join("picked.txt");
        std::fs::write(&picked_path, "picked").unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| {
            super::ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx)
        });
        let paste = |cx: &mut gpui::VisualTestContext, item| {
            cx.update(|window, app| {
                view.update(app, |v, cx| v.focus_composer(window, cx));
                app.write_to_clipboard(item);
            });
            cx.simulate_keystrokes(if cfg!(target_os = "macos") { "cmd-v" } else { "ctrl-v" });
        };

        paste(cx, ClipboardItem::new_string("ordinary text".into()));
        cx.update(|window, app| view.update(app, |v, cx| {
            assert_eq!(v.draft(cx), "ordinary text");
            assert!(!v.attachments.has_files());
            v.set_draft("", window, cx);
        }));

        let button = cx.debug_bounds("composer-attach").unwrap().center();
        cx.simulate_click(button, Default::default());
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(options.files && options.multiple && !options.directories);
            Some(vec![picked_path.clone()])
        });
        cx.run_until_parked();

        paste(cx, ClipboardItem { entries: vec![
            ClipboardEntry::ExternalPaths(ExternalPaths([path.clone()].into_iter().collect())),
        ] });
        let position = cx.debug_bounds("composer-file-drop").unwrap().center();
        cx.simulate_event(FileDropEvent::Entered {
            position,
            paths: ExternalPaths([dropped_path.clone()].into_iter().collect()),
        });
        cx.simulate_event(FileDropEvent::Submit { position });
        cx.simulate_event(FileDropEvent::Ended);
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
            assert_eq!(paths.len(), 4);
            assert_eq!(&paths[..3], &[picked_path, path, dropped_path]);
            // Temp folders can be reached by more than one name (8.3 names on Windows, `/private` on macOS).
            let canonical = |p: &std::path::Path| std::fs::canonicalize(p).unwrap();
            assert_eq!(canonical(paths[3].parent().unwrap()), canonical(&data.join("attachments")));
            assert_eq!(std::fs::read(&paths[3]).unwrap(), image.bytes);
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
            v.session_id = Some("codex-session".into());
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
            v.send_as("Ordinary message".into(), crate::store::FollowUp::Queue, cx);
            let ask_id = v.active_question().unwrap().id;
            v.set_draft("My answer", window, cx);
            v.on_event(Event::Permission { id: json!("approval"), params: json!({
                "toolCall":{"title":"Run a command","kind":"execute"},
                "options":[{"optionId":"allow","name":"Allow","kind":"allow_once"}]
            }) }, cx);
            assert_eq!(v.pending_question_title().as_deref(), Some("Run a command"));
            assert_ne!(v.active_question().unwrap().id, ask_id);
            v.answer(Some(0), cx);
            assert_eq!(v.active_question().unwrap().id, ask_id);
            v.set_draft("", window, cx);
            v.on_event(Event::TurnEnded(Ok(json!({"stopReason":"end_turn"}))), cx);
            let ask = v.active_question().expect("the ask card survives turn completion");
            assert_eq!(ask.title, "Choose a branch.");
            let names: Vec<&str> = ask.options.iter().map(|o| o.name.as_str()).collect();
            assert_eq!(names, ["main", "dev"]);
            assert!(v.answer_question(ask_id, Some(1), cx));
            let next = v.active_question().unwrap();
            assert_eq!(next.title, "What should I check?");
            v.set_draft("The regression test.", window, cx);
            v.submit_with(false, window, cx);
            assert!(!v.is_waiting());
            assert_eq!(v.queue.front().unwrap().text, "> Choose a branch.\n\ndev\n\n> What should I check?\n\nThe regression test.");
            assert_eq!(v.queue.back().unwrap().text, "Ordinary message");
            assert!(v.active_question().is_none());
        }));
    }

    #[gpui::test]
    fn codex_async_answer_steers_the_running_turn_and_preserves_unmatched_text(cx: &mut gpui::TestAppContext) {
        use std::io::{BufRead, BufReader};
        use serde_json::{Value, json};
        use super::{Connection, Event, Phase, ThreadView};

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (from_agent, _keep_open) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        let (tx, _events) = futures::channel::mpsc::unbounded();
        let connection = Connection::connect(from_agent, to_agent, None::<std::io::Empty>, tx);
        let (view, cx) = cx.add_window_view(|window, cx| ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx));
        cx.update(|window, app| view.update(app, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.session_id = Some("codex-session".into());
            v.agent_meta = json!({"steering":{"supported":true}});
            v.phase = Phase::Ready;
            v.working = true;
            v.on_event(update_event(json!({
                "sessionUpdate":"agent_message_chunk","messageId":"async-question-item",
                "content":{"type":"text","text":"There are two things I could investigate."}
            })), cx);
            v.on_event(Event::Elicitation { id: json!("question-form"), params: json!({
                "mode":"form", "_meta":{"codex":{"delivery":"async"}},
                "requestedSchema":{"properties":{"question_0":{"title":"Which test?","type":"string"}}}
            }) }, cx);
            assert!(matches!(&v.thread.entries[..], [Entry::Agent(text)] if text == "There are two things I could investigate."));
            v.set_draft("The unit test.", window, cx);
            v.submit_with(false, window, cx);
            assert_eq!(v.steering.len(), 1);
            assert!(v.queue.is_empty());
        }));
        let mut wire = BufReader::new(from_client).lines();
        let decline: Value = serde_json::from_str(&wire.next().unwrap().unwrap()).unwrap();
        assert_eq!(decline, json!({"jsonrpc":"2.0","id":"question-form","result":{"action":"decline"}}));
        let steer: Value = serde_json::from_str(&wire.next().unwrap().unwrap()).unwrap();
        assert_eq!(steer["method"], "_session/steering");
        assert_eq!(steer["params"]["sessionId"], "codex-session");
        assert_eq!(steer["params"]["prompt"][0]["text"], "> Which test?\n\nThe unit test.");
        std::mem::forget(_keep_open);
    }

    #[gpui::test]
    fn multi_select_checkboxes_work_across_acp_elicitation_and_codex(cx: &mut gpui::TestAppContext) {
        use std::io::{BufRead, BufReader};
        use serde_json::{Value, json};
        use super::{Connection, Event, Phase, ThreadView};

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (from_agent, _keep_open) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        let (tx, _events) = futures::channel::mpsc::unbounded();
        let connection = Connection::connect(from_agent, to_agent, None::<std::io::Empty>, tx);
        let (view, cx) = cx.add_window_view(|window, cx| ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx));
        cx.update(|window, app| view.update(app, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.session_id = Some("s1".into());
            v.phase = Phase::Ready;
            v.working = true;

            // 1. Claude-style ACP permission multi-select with optional free-text.
            v.on_event(Event::Permission { id: json!("claude-multi"), params: json!({
                "_meta": {"m4code": {"freeText": true, "multiSelect": true}},
                "toolCall": {"title": "Pick topics", "kind": "other"},
                "options": [
                    {"optionId": "0", "name": "Rust", "kind": "allow_once"},
                    {"optionId": "1", "name": "Go", "kind": "allow_once"},
                    {"optionId": "2", "name": "Zig", "kind": "allow_once"}
                ]
            }) }, cx);
            let q = v.pending_question().unwrap();
            assert!(q.multi_select);
            assert!(!v.submit_multi_question(q.id, cx), "cannot submit with nothing checked");
            assert!(v.toggle_question_option(q.id, 2, cx));
            assert!(v.toggle_question_option(q.id, 0, cx));
            assert_eq!(v.pending_question().unwrap().selected.iter().copied().collect::<Vec<_>>(), vec![0, 2]);
            v.set_draft("OCaml", window, cx);
            v.submit_with(false, window, cx);
            assert!(v.active_question().is_none());

            // 2. Standard ACP form elicitation multi-select (Antigravity / sync elicitation).
            v.on_event(Event::Elicitation { id: json!("elicit-multi"), params: json!({
                "mode": "form",
                "message": "Select languages",
                "requestedSchema": {
                    "type": "object",
                    "properties": {
                        "langs": {
                            "type": "array",
                            "items": {
                                "anyOf": [
                                    {"const": "rs", "title": "Rust", "description": "Systems"},
                                    {"const": "ts", "title": "TypeScript"},
                                    {"const": "py", "title": "Python"}
                                ]
                            }
                        }
                    }
                }
            }) }, cx);
            let q = v.pending_question().unwrap();
            assert!(q.multi_select);
            assert_eq!(q.options[0].description.as_deref(), Some("Systems"));
            assert!(v.toggle_question_option(q.id, 0, cx));
            assert!(v.toggle_question_option(q.id, 1, cx));
            assert!(v.toggle_question_option(q.id, 1, cx)); // uncheck
            assert!(v.toggle_question_option(q.id, 2, cx));
            assert!(v.submit_multi_question(q.id, cx));
            assert!(v.active_question().is_none());

            // 3. Codex async multi-select elicitation.
            v.on_event(Event::Elicitation { id: json!("codex-multi"), params: json!({
                "mode": "form",
                "_meta": {"codex": {"delivery": "async"}},
                "requestedSchema": {
                    "type": "object",
                    "properties": {
                        "question_0": {
                            "title": "Which targets?",
                            "type": "array",
                            "items": {
                                "anyOf": [
                                    {"const": "macOS", "title": "macOS", "description": "Apple Silicon"},
                                    {"const": "Linux", "title": "Linux"}
                                ]
                            }
                        }
                    }
                }
            }) }, cx);
            let q = v.pending_question().unwrap();
            assert!(q.multi_select);
            assert_eq!(q.options[0].description.as_deref(), Some("Apple Silicon"));
            assert!(v.toggle_question_option(q.id, 0, cx));
            assert!(v.toggle_question_option(q.id, 1, cx));
            assert!(v.submit_multi_question(q.id, cx));
            assert_eq!(v.queue.front().unwrap().text, "> Which targets?\n\nmacOS, Linux");
        }));

        let mut wire = BufReader::new(from_client).lines();
        let claude_reply: Value = serde_json::from_str(&wire.next().unwrap().unwrap()).unwrap();
        assert_eq!(claude_reply["id"], "claude-multi");
        assert_eq!(claude_reply["result"]["_meta"]["m4code"]["optionIds"], json!(["0", "2"]));
        assert_eq!(claude_reply["result"]["_meta"]["m4code"]["text"], "OCaml");

        let elicit_reply: Value = serde_json::from_str(&wire.next().unwrap().unwrap()).unwrap();
        assert_eq!(elicit_reply["id"], "elicit-multi");
        assert_eq!(elicit_reply["result"], json!({"action": "accept", "content": {"langs": ["rs", "py"]}}));

        let codex_decline: Value = serde_json::from_str(&wire.next().unwrap().unwrap()).unwrap();
        assert_eq!(codex_decline["id"], "codex-multi");
        assert_eq!(codex_decline["result"], json!({"action": "decline"}));
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
            if let super::ThreadEvent::OpenPath(path, line, _) = event {
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
        let row = cx.debug_bounds("tool-row-1").expect("expand to see the tool call");
        assert!(row.size.height < gpui::px(32.0), "a multi-line command stays on one line: {row:?}");
    }

    #[gpui::test]
    fn a_turn_ends_before_its_changes_are_captured(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Claude);
        start.prior_entries = vec![Entry::User("go".into())];
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.update(cx, |v, cx| {
            v.phase = super::Phase::Ready;
            v.working = true;
            v.active_turn_review = Some(super::review::ActiveTurn {
                id: uuid::Uuid::new_v4(), row: 0, text: "go".into(), before: String::new(), cwd: dir.path().into(),
            });
            v.capture_turn_review(Ok(serde_json::json!({"stopReason": "end_turn"})), cx);
            assert!(!v.is_working(), "git working out the turn's changes doesn't hold the turn open");
        });
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn turn_status_shows_working_under_the_steps_then_how_long_it_took(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Claude);
        start.prior_entries = vec![Entry::User("go".into())];
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        let status_row = |view: &gpui::Entity<super::ThreadView>, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, app| window.draw(app).clear(app));
            view.read_with(cx, |v, _| v.list.bounds_for_item(v.thread.entries.len()).expect("status row").size.height)
        };
        view.update(cx, |v, _| v.phase = super::Phase::Ready);
        assert_eq!(status_row(&view, cx), gpui::px(0.0), "nothing under a session that hasn't worked");
        view.update(cx, |v, cx| {
            v.working = true;
            v.turn_started = Some(std::time::Instant::now());
            cx.notify();
        });
        assert!(status_row(&view, cx) > gpui::px(0.0));
        assert!(cx.debug_bounds("turn-working").is_some());
        view.update(cx, |v, cx| v.turn_ended(Ok(serde_json::json!({"stopReason": "end_turn"})), cx));
        assert!(status_row(&view, cx) > gpui::px(0.0));
        assert!(cx.debug_bounds("turn-working").is_none());
        assert!(cx.debug_bounds("turn-done").is_some(), "the turn's time stays after it ends");
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
    fn unconfirmed_read_only_keeps_the_draft_when_send_is_pressed(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx));
        cx.update(|window, app| view.update(app, |view, cx| {
            view.phase = super::Phase::Ready;
            view.require_read_only(super::Permission::ReadOnly, cx);
            view.set_draft("inspect permissions", window, cx);
            view.submit(window, cx);
            assert_eq!(view.draft(cx), "inspect permissions");
            assert_eq!(view.waiting_messages().count(), 0);
            assert_eq!(view.permission_for_relaunch(), Some(super::Permission::ReadOnly));
        }));
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
            env: vec![], mcp_servers: vec![], mcp_tools: None, launch_args: vec![], meta: serde_json::Value::Null, session_id: None, permission: None, starter: crate::harness::defaults::Starter::User, options: vec![],
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
            env: vec![], mcp_servers: vec![], mcp_tools: None, launch_args: vec![], meta: serde_json::Value::Null, session_id: None, permission: None, starter: crate::harness::defaults::Starter::User, options: vec![],
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
            mcp_servers: vec![], mcp_tools: None, launch_args: vec![],
            meta: serde_json::Value::Null,
            session_id: None,
            permission: None, starter: crate::harness::defaults::Starter::User,
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
            mcp_servers: vec![], mcp_tools: None, launch_args: vec![],
            meta: serde_json::Value::Null,
            session_id: None,
            permission: None, starter: crate::harness::defaults::Starter::User,
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
            mcp_servers: vec![], mcp_tools: None, launch_args: vec![],
            meta: serde_json::Value::Null,
            session_id: None,
            permission: None, starter: crate::harness::defaults::Starter::User,
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
            mcp_servers: vec![], mcp_tools: None, launch_args: vec![],
            meta: serde_json::Value::Null,
            session_id: None,
            permission: None, starter: crate::harness::defaults::Starter::User,
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
            v.on_event(update_event(chunk("agent_message_chunk", "Hello wor")), cx);
            v.send_as("stop".into(), FollowUp::Steer, cx);
            assert_eq!(v.unread_steers().collect::<Vec<_>>(), ["stop"], "waits above the message box");
            // What the agent was still writing when the steer went out.
            v.on_event(update_event(chunk("agent_message_chunk", "ld.")), cx);
            let mut read = chunk("user_message_chunk", "stop");
            read["_meta"] = json!({"m4code": {"steer": v.steering.front().unwrap().id.to_string()}});
            v.on_event(update_event(read), cx);
            v.on_event(update_event(chunk("agent_message_chunk", "Stopping.")), cx);
            assert_eq!(v.thread.entries, [Entry::Agent("Hello world.".into()), Entry::User("stop".into()), Entry::Agent("Stopping.".into())]);
            assert_eq!(v.unread_steers().count(), 0);
            // Two steers read one after the other stay two messages.
            v.send_as("do X".into(), FollowUp::Steer, cx);
            v.send_as("do Y".into(), FollowUp::Steer, cx);
            for text in ["do X", "do Y"] {
                let mut read = chunk("user_message_chunk", text);
                read["_meta"] = json!({"m4code": {"steer": v.steering.front().unwrap().id.to_string()}});
                v.on_event(update_event(read), cx);
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
    fn steer_without_live_support_queues_in_order_and_leaves_the_turn_running(cx: &mut gpui::TestAppContext) {
        use crate::store::FollowUp;
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        for harness in [Harness::Codex, Harness::OpenCode, Harness::Antigravity] {
            let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), harness), window, cx));
            let (from_agent, to_client) = std::io::pipe().unwrap();
            let (from_client, to_agent) = std::io::pipe().unwrap();
            let (tx, _events) = futures::channel::mpsc::unbounded();
            let connection = super::Connection::connect(from_agent, to_agent, None::<std::io::PipeReader>, tx);
            view.update(cx, |v, cx| {
                v.connection = Some(std::rc::Rc::new(connection));
                v.session_id = Some("s1".into());
                v.phase = super::Phase::Ready;
                v.working = true;
                assert!(!v.can_steer("first", FollowUp::Steer));
                v.send_as("queued".into(), FollowUp::Queue, cx);
                v.send_as("first".into(), FollowUp::Steer, cx);
                v.send_as("second".into(), FollowUp::Steer, cx);
                let order: Vec<&str> = v.waiting_messages().map(|(_, text)| text).collect();
                assert_eq!(order, ["queued", "first", "second"]);
                assert!(v.working);
                assert!(v.steering.is_empty());
            });
            drop((to_client, from_client));
        }
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
            mcp_servers: vec![], mcp_tools: None, launch_args: vec![],
            meta: serde_json::Value::Null,
            session_id: None,
            permission: None, starter: crate::harness::defaults::Starter::User,
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
            v.on_event(update_event(serde_json::json!({
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
            v.on_event(update_event(serde_json::json!({
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
            v.on_event(update_event(start.clone()), cx);
            assert!(v.background_tasks.is_empty(), "replay does not create live tasks");
            v.phase = super::Phase::Ready;
            v.on_event(update_event(start), cx);
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
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "user_message_chunk",
                "content": { "type": "text", "text": "Task id \"conv/task-42\" finished with result: Command exited with code 0." }
            })), cx);
            assert!(v.background_tasks.is_empty());
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "cmd-1",
                "content": [{ "type": "content", "content": { "type": "text", "text": "Late output" } }]
            })), cx);
            assert!(v.background_tasks.is_empty(), "late output must not resurrect a finished task");

            // A server restart notice or empty manage_task(list) response clears all tracked tasks.
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "cmd-2",
                "title": "Run tests",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "command": "cargo test" },
                "rawOutput": "Tool is running as a background task with task id: conv/task-99"
            })), cx);
            assert_eq!(v.background_tasks.len(), 1);
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "list-1",
                "title": "Manage task",
                "kind": "other",
                "status": "completed",
                "rawOutput": "No background tasks are currently running."
            })), cx);
            assert!(v.background_tasks.is_empty(), "empty task list clears stale background tasks");
        });
    }

    #[gpui::test]
    fn reopened_session_settles_stale_running_tools_from_prior_entries(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let mut start = test_start(dir.path().into(), Harness::Antigravity);
        start.prior_entries = vec![Entry::Tool(crate::harness::thread::ToolCall {
            id: "stale-cmd".into(),
            title: "Run tests".into(),
            kind: "execute".into(),
            status: super::ToolStatus::Running,
            content: vec![],
            locations: vec![],
            details: Box::new(crate::harness::thread::ToolDetails {
                command: Some("cargo test -- thread::".into()),
                ..Default::default()
            }),
            subagent: None,
        })];
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(start, window, cx));
        view.read_with(cx, |v, _| {
            assert!(!v.has_background_work(), "stale running tools from before a restart must not show as background work");
            assert_eq!(v.thread.tool("stale-cmd").unwrap().status, super::ToolStatus::Ended);
        });
    }

    #[gpui::test]
    fn antigravity_execute_tool_stays_live_across_turn_end_while_other_tools_settle(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), Harness::Antigravity), window, cx));
        view.update(cx, |v, cx| {
            v.phase = super::Phase::Ready;
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "read-1",
                "kind": "read",
                "status": "in_progress"
            })), cx);
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "cmd-1",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": "cargo test --locked" }
            })), cx);
            assert!(v.has_background_work());
            v.turn_ended(Ok(serde_json::json!({"stopReason": "end_turn"})), cx);
            assert_eq!(v.thread.tool("read-1").unwrap().status, super::ToolStatus::Ended);
            assert_eq!(v.thread.tool("cmd-1").unwrap().status, super::ToolStatus::Running);
            assert!(v.has_background_work(), "backgrounded execute tool stays visible across turns");
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "cmd-1",
                "status": "completed",
                "content": [{ "type": "content", "content": { "type": "text", "text": "Command exited with code 0." } }]
            })), cx);
            assert_eq!(v.thread.tool("cmd-1").unwrap().status, super::ToolStatus::Done);
            assert!(!v.has_background_work());
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
        let connection = crate::harness::acp::Connection::connect(from_agent, to_agent, None::<std::io::PipeReader>, tx.clone());
        view.update(cx, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.events = Some(tx);
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
        assert_eq!(super::active_slash_token("/com", 1), Some((0, 4, "")));
        assert_eq!(super::active_slash_token("/com", 0), None, "cursor before the leading slash must not panic");
        assert_eq!(super::active_slash_token("please run /com", 15), Some((11, 15, "com")));
        assert_eq!(super::active_slash_token("please run /com", 11), None, "cursor before a mid-message slash must not panic");
        assert_eq!(super::active_slash_token("please /comp now", 12), Some((7, 12, "comp")));
        assert_eq!(super::active_slash_token("check src/thread/mod.rs", 23), None);
        assert_eq!(super::active_slash_token("see https://example.com", 23), None);
        assert_eq!(super::active_slash_token("hello world", 11), None);
    }

    #[gpui::test]
    fn antigravity_elicitation_accepts_free_text_and_background_task_reads_log_output(cx: &mut gpui::TestAppContext) {
        use std::io::{BufRead, BufReader};
        use serde_json::{Value, json};
        use super::{Connection, Event, Phase, ThreadView};

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let log_file = dir.path().join("task.log");
        std::fs::write(&log_file, "line 1\nline 2\n").unwrap();
        let (from_agent, _keep_open) = std::io::pipe().unwrap();
        let (from_client, to_agent) = std::io::pipe().unwrap();
        let (tx, _events) = futures::channel::mpsc::unbounded();
        let connection = Connection::connect(from_agent, to_agent, None::<std::io::Empty>, tx);
        let (view, cx) = cx.add_window_view(|window, cx| ThreadView::new(test_start(dir.path().into(), Harness::Antigravity), window, cx));
        cx.update(|window, app| view.update(app, |v, cx| {
            v.connection = Some(std::rc::Rc::new(connection));
            v.session_id = Some("agy-1".into());
            v.phase = Phase::Ready;
            v.working = true;
            v.agent_meta = json!({"steering": {"supported": true}});
            assert!(v.native_steering(), "Antigravity uses native steering when advertised");

            // Free-text answer on an ACP elicitation declines the form and delivers the typed answer.
            v.on_event(Event::Elicitation { id: json!("agy-q"), params: json!({
                "mode": "form",
                "message": "Pick a color",
                "requestedSchema": {
                    "type": "object",
                    "properties": {
                        "color": { "type": "string", "enum": ["red", "blue"] }
                    }
                }
            }) }, cx);
            assert!(v.active_question().is_some_and(|q| q.takes_own_answer));
            v.set_draft("green", window, cx);
            v.submit_with(false, window, cx);
            assert!(v.active_question().is_none());

            // Background task parses log_path and opens log tail into shell_output.
            v.on_event(update_event(json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "bg-1",
                "title": "Run build",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "command": "cargo build" },
                "rawOutput": format!("Tool is running as a background task with task id: conv/task-7\nTask logs are available at: file://{}", log_file.display())
            })), cx);
            assert_eq!(v.background_tasks.len(), 1);
            assert_eq!(v.background_tasks[0].log_path.as_deref(), Some(log_file.as_path()));
            v.open_background_task_output("conv/task-7", cx);
            assert_eq!(v.shell_output.as_ref().map(|o| o.text.as_str()), Some("line 1\nline 2\n"));
        }));
        let mut wire = BufReader::new(from_client).lines();
        let decline: Value = serde_json::from_str(&wire.next().unwrap().unwrap()).unwrap();
        assert_eq!(decline, json!({"jsonrpc": "2.0", "id": "agy-q", "result": {"action": "decline"}}));
        let steer: Value = serde_json::from_str(&wire.next().unwrap().unwrap()).unwrap();
        assert_eq!(steer["method"], "_session/steering");
        assert_eq!(steer["params"]["prompt"][0]["text"], "> Pick a color\n\ngreen");
        std::mem::forget(_keep_open);
    }

    #[test]
    fn slash_command_spans_finds_all_command_tokens_and_skips_paths() {
        let spans = super::slash_command_spans("please /compact and /clear src/main.rs https://a.b/c");
        assert_eq!(spans, vec![(7..15, "compact"), (20..26, "clear")]);
    }

    #[gpui::test]
    fn codex_background_execute_tool_stays_working_until_completion_and_emits_idle_once(cx: &mut gpui::TestAppContext) {
        use std::cell::RefCell;
        use std::rc::Rc;

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx));
        let idles = Rc::new(RefCell::new(Vec::<&'static str>::new()));
        let sink = idles.clone();
        let _sub = cx.update(|_, app| app.subscribe(&view, move |_, ev: &super::ThreadEvent, _| {
            if let super::ThreadEvent::Idle(outcome) = ev {
                sink.borrow_mut().push(outcome);
            }
        }));

        view.update(cx, |v, cx| {
            v.phase = super::Phase::Ready;
            v.working = true;
            v.turn_seq = 1;
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "read-1",
                "kind": "read",
                "status": "in_progress"
            })), cx);
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "exec-1",
                "title": "Run workspace tests",
                "kind": "execute",
                "status": "in_progress",
                "rawInput": { "command": "cargo test --workspace" }
            })), cx);
            assert!(v.is_working());
            assert!(v.has_background_work());

            // Turn ends while the background shell is still running: read settles, execute stays live and working.
            v.turn_ended(Ok(serde_json::json!({"stopReason": "end_turn"})), cx);
            assert_eq!(v.thread.tool("read-1").unwrap().status, super::ToolStatus::Ended);
            assert_eq!(v.thread.tool("exec-1").unwrap().status, super::ToolStatus::Running);
            assert!(v.has_background_work());
            assert!(v.is_working(), "Codex must stay working while a background execute tool is still running");
            assert!(idles.borrow().is_empty(), "turn_ended with a live background tool must not emit Idle yet");

            // Streaming progress updates keep the session working with no idle transitions.
            for chunk in ["compiling...\n", "running 120 tests...\n"] {
                v.on_event(update_event(serde_json::json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "exec-1",
                    "_meta": { "terminal_output_delta": { "data": chunk, "stream": "stdout" } }
                })), cx);
                assert!(v.is_working());
                assert!(v.has_background_work());
                assert!(idles.borrow().is_empty());
            }

            // Completion finishes the background task, ends working immediately, and emits Idle exactly once.
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "exec-1",
                "status": "completed",
                "rawOutput": { "exit_code": 0, "stdout": "compiling...\nrunning 120 tests...\nok\n" }
            })), cx);
            assert_eq!(v.thread.tool("exec-1").unwrap().status, super::ToolStatus::Done);
            assert!(!v.has_background_work());
            assert!(!v.is_working(), "completing the last background task ends working immediately");
        });
        // Events reach subscribers once the update returns.
        assert_eq!(*idles.borrow(), vec!["completed"]);
        view.update(cx, |v, cx| {

            // A late terminal update must not flip back to working or emit Idle again.
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "exec-1",
                "status": "completed",
                "content": [{ "type": "content", "content": { "type": "text", "text": "late flush" } }]
            })), cx);
            assert!(!v.is_working());
        });
        assert_eq!(*idles.borrow(), vec!["completed"], "a late update sends no second Idle");
    }

    #[gpui::test]
    fn codex_unified_exec_background_session_tracks_through_turn_end_and_write_stdin_exit(cx: &mut gpui::TestAppContext) {
        use std::cell::RefCell;
        use std::rc::Rc;

        cx.update(gpui_kit::init);
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| super::ThreadView::new(test_start(dir.path().into(), Harness::Codex), window, cx));
        let idles = Rc::new(RefCell::new(Vec::<&'static str>::new()));
        let sink = idles.clone();
        let _sub = cx.update(|_, app| app.subscribe(&view, move |_, ev: &super::ThreadEvent, _| {
            if let super::ThreadEvent::Idle(outcome) = ev {
                sink.borrow_mut().push(outcome);
            }
        }));

        view.update(cx, |v, cx| {
            let start_bg = serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "exec-77",
                "title": "Run dev server",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "cmd": "npm run dev" },
                "rawOutput": "Wall time: 5.0s\nProcess running with session ID 77\nOutput:\nstarting..."
            });
            // Replay before Phase::Ready must not create live background tasks.
            v.on_event(update_event(start_bg.clone()), cx);
            assert!(v.background_tasks.is_empty());

            v.phase = super::Phase::Ready;
            v.working = true;
            v.turn_seq = 1;
            v.on_event(update_event(start_bg), cx);
            assert_eq!(v.background_tasks.len(), 1);
            assert_eq!(v.background_tasks[0].id, "77");
            assert_eq!(v.background_tasks[0].label, "npm run dev");

            // Turn ends while unified_exec session 77 is still running.
            v.turn_ended(Ok(serde_json::json!({"stopReason": "end_turn"})), cx);
            assert!(v.has_background_work());
            assert!(v.is_working());
            assert!(idles.borrow().is_empty());

            // Poll update while still running does not duplicate or finish the task.
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "stdin-poll",
                "title": "write_stdin",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "session_id": 77, "chars": "" },
                "rawOutput": "Process running with session ID 77\nOutput:\nlistening on 3000"
            })), cx);
            assert_eq!(v.background_tasks.len(), 1);
            assert!(v.is_working());
            assert!(idles.borrow().is_empty());

            // Exit update retires session 77, drops working, and emits Idle once.
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "stdin-exit",
                "title": "write_stdin",
                "kind": "execute",
                "status": "completed",
                "rawInput": { "session_id": 77, "chars": "" },
                "rawOutput": "Process exited with code 0\nOutput:\nstopped"
            })), cx);
            assert!(v.background_tasks.is_empty());
            assert!(!v.has_background_work());
            assert!(!v.is_working());
        });
        // Events reach subscribers once the update returns.
        assert_eq!(*idles.borrow(), vec!["completed"]);
        view.update(cx, |v, cx| {

            // Late update for the same session id must not resurrect it.
            v.on_event(update_event(serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "exec-77",
                "rawOutput": "Process running with session ID 77"
            })), cx);
            assert!(v.background_tasks.is_empty());
            assert!(!v.is_working());
        });
        assert_eq!(*idles.borrow(), vec!["completed"], "a late update sends no second Idle");
    }
}
