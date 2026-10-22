//! A chat pane: Claude Code driven through ACP instead of a terminal. The transcript is drawn natively; the agent
//! is the user's own `claude` behind Zed's adapter (see `acp.rs`). Skills and slash commands come from the agent
//! (`available_commands_update`) and are sent as ordinary prompt text, which is how Claude Code runs them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::path::PathBuf;

use base64::Engine as _;
use futures::StreamExt;
use gpui::{
    AppContext, ClipboardEntry, Image, ImageFormat, img,
    AnyElement, App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Point, Render, ScrollHandle, ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, anchored, deferred, div, prelude::FluentBuilder, px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use serde_json::{Value, json};

use super::acp::{self, Agent, Msg};
use super::model::{self, Command, Item, Meta, Tool};
use super::store::{load_cache, load_picks, save_cache, save_picks, scratch_dir};
use super::ui::markdown;
use crate::store::{RateLimit, now};
use crate::theme::{self, hsla};
use gpui_kit::assets::IconName;
use crate::widgets::{button, clawd, fade_in, icon, spinner};

/// How to run the user's Claude for this chat: their executable, plus the environment of the account to use.
#[derive(Clone)]
pub struct Launch {
    pub claude: PathBuf,
    pub env: Vec<(String, String)>,
}

/// What the pane asks the workspace to record or show.
pub enum ChatEvent {
    Toast { text: String, error: bool },
    /// Claude's id for this conversation, once it exists.
    Session(String),
    /// A title the agent chose.
    Title(String),
    /// The user sent this text.
    Sent(String),
    /// Account limits the agent reported; they belong to whichever account the chat runs under.
    Limits { five: Option<RateLimit>, seven: Option<RateLimit> },
    /// Context tokens used and the window size.
    Context(u64, u64),
    /// A turn ended (the prompt-cache timer starts from here).
    Turn,
    /// The message box now holds this (not sent).
    Draft(String),
    /// The user tied the (still empty) chat to a project, or untied it: a new conversation in this folder.
    Cwd(PathBuf),
}

/// `picker` value that means the project list rather than one of the agent's options.
const PROJECT_PICKER: usize = usize::MAX;

/// What a normal start says. It is not shown: the chat is usable at once and the agent starts behind it.
const START: &str = "Starting Claude…";
const LOADING: &str = "Loading the conversation…";

/// An image waiting to be sent with the next message.
struct Attachment {
    image: Arc<Image>,
    mime: &'static str,
}

/// Claude's image limit is 5 MB; stay under it.
const MAX_IMAGE: usize = 4_500_000;

/// A clipboard image in a form Claude accepts: PNG, JPEG, GIF and WebP go as they are, others are re-encoded as PNG.
fn attachable(image: &Image) -> Result<Attachment, String> {
    let mime = match image.format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::Webp => "image/webp",
        ImageFormat::Svg => return Err("SVG images can't be sent".into()),
        _ => "",
    };
    let (image, mime) = if mime.is_empty() {
        let decoded = ::image::load_from_memory(&image.bytes).map_err(|e| format!("Could not read the image: {e}"))?;
        let mut png = std::io::Cursor::new(vec![]);
        decoded.write_to(&mut png, ::image::ImageFormat::Png).map_err(|e| e.to_string())?;
        (Image::from_bytes(ImageFormat::Png, png.into_inner()), "image/png")
    } else {
        (image.clone(), mime)
    };
    if image.bytes.len() > MAX_IMAGE {
        return Err("That image is over 4.5 MB; Claude won't take it".into());
    }
    Ok(Attachment { image: Arc::new(image), mime })
}

/// What the clipboard offers as an image: a copied picture, or a copied image file.
fn clipboard_image(item: &gpui::ClipboardItem) -> Option<Result<Attachment, String>> {
    if item.text().is_some_and(|t| !t.is_empty()) {
        return None; // text pastes as text
    }
    item.entries().iter().find_map(|e| match e {
        ClipboardEntry::Image(i) => Some(attachable(i)),
        ClipboardEntry::ExternalPaths(p) => p.paths().iter().find_map(|path| {
            let format = ImageFormat::from_mime_type(mime_guess_ext(path)?)?;
            Some(std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| attachable(&Image::from_bytes(format, b))))
        }),
        _ => None,
    })
}

fn mime_guess_ext(path: &std::path::Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Installing the adapter or starting the session, with what to tell the user.
    Working(&'static str),
    NoNode,
    NoClaude,
    Failed(String),
    Ready,
}

/// What a row of the pop-up list does.
#[derive(Clone)]
enum Choice {
    Option(usize, String),
    Project(Option<PathBuf>),
}

/// The earlier conversation of a chat whose project was changed: compacted into a summary that goes along with the
/// next message, so the new conversation knows what came before.
enum Carry {
    None,
    /// The project changed; the summary is made when the next message is sent, so a misclick costs nothing.
    Pending(String),
    Summarizing,
    Ready(String),
}

const COMPACT_SYSTEM: &str = "You compact conversations so another assistant can carry on in a new session. Write a summary of: the goal, decisions made, key facts (files, names, numbers, commands), what is done and what is still open. At most 400 words, plain text, no preamble.";

enum Pending {
    Init,
    Session,
    Prompt,
    SetOption,
}

/// A tool call the agent wants approved; `options` are (id, label, allows).
struct Perm {
    rpc_id: Value,
    title: String,
    options: Vec<(String, String, bool)>,
}

pub struct ChatView {
    /// The chat's id in Claudiu, which the MCP helper uses to say who is asking.
    id: String,
    pub cwd: PathBuf,
    /// Projects a new chat can be tied to, as (name, folder).
    projects: Vec<(String, PathBuf)>,
    /// What the context figure ends at: the auto-compact switch and threshold the mod reported (`claude::context_limit`).
    auto_compact: Option<bool>,
    compact_at: Option<u64>,
    /// A project the model asked for (`set_chat_project`); applied when the turn in progress ends.
    tie_next: Option<PathBuf>,
    /// A first message to send as soon as the conversation is ready (a chat opened by another session).
    startup: Option<String>,
    /// Messages sent before the conversation was ready: (text, images, prompt blocks), delivered in order once it is.
    queued: Vec<(String, Vec<Arc<Image>>, Vec<Value>)>,
    carry: Carry,
    pub phase: Phase,
    items: Vec<Item>,
    meta: Meta,
    /// Model or effort picked before the session was ready, as (option id, value); applied once it is.
    early: Vec<(String, String)>,
    launch: Option<Launch>,
    /// Claude's session id to load instead of starting a new conversation.
    resume: Option<String>,
    session_id: Option<String>,
    agent: Option<Agent>,
    /// Counts `start`s; the receive loop of an earlier start stops when this moves on.
    epoch: u64,
    pending: std::collections::HashMap<u64, Pending>,
    /// Prompts sent and not yet answered; the agent queues the ones that arrive mid-turn.
    inflight: usize,
    perms: Vec<Perm>,
    expanded: HashSet<String>,
    input: Entity<TextareaState>,
    attachments: Vec<Attachment>,
    /// Images sent with a message, by the index of that message in `items`, to draw them again.
    sent_images: HashMap<usize, Vec<Arc<Image>>>,
    /// The account changed: restart the agent (same conversation) as soon as the chat is idle.
    relaunch: bool,
    slash_sel: usize,
    slash_scroll: ScrollHandle,
    /// Esc closed the command list; it stays closed until the text changes.
    slash_off: bool,
    picker: Option<(usize, Point<Pixels>)>,
    scroll: ScrollHandle,
    smooth: crate::glide::Smooth,
    follow: bool,
    /// `items` is the saved copy of the conversation, shown until the agent replays the real one.
    cached_shown: bool,
    /// The conversation being replayed while the saved copy is still on screen; it replaces the copy when done.
    replay: Vec<Item>,
    pub focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl Drop for ChatView {
    fn drop(&mut self) {
        // The adapter outlives the chat, so its conversation is ended there.
        if let (Some(agent), Some(session)) = (&self.agent, &self.session_id) {
            agent.close(session);
        }
    }
}

impl EventEmitter<ChatEvent> for ChatView {}

impl Focusable for ChatView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ChatView {
    pub fn new(id: String, cwd: PathBuf, launch: Option<Launch>, resume: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(1, 10).submit_on_enter(true).placeholder("Message Claude, / for skills and commands, paste an image")
        });
        let mut subs = vec![cx.subscribe_in(&input, window, |v: &mut Self, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => v.submit(window, cx),
            InputEvent::Change => {
                v.slash_sel = 0;
                v.slash_off = false;
                cx.emit(ChatEvent::Draft(v.input.read(cx).value().to_string()));
                cx.notify();
            }
            _ => {}
        })];
        // The command list follows the cursor, which moves without a Change event.
        subs.push(cx.observe(&input, |_, _, cx| cx.notify()));
        // Arrow keys and Enter are text-box actions that run before any key listener, so the command list
        // picks them up first, and only while it is open and the box has the focus.
        let this = cx.entity().downgrade();
        subs.push(cx.intercept_keystrokes(move |ev, window, cx| {
            let (m, key) = (ev.keystroke.modifiers, ev.keystroke.key.as_str());
            let paste = key == "v" && m.secondary() && !m.shift && !m.alt;
            let plain = !(m.control || m.alt || m.shift || m.platform);
            if !paste && !plain {
                return;
            }
            let Some(view) = this.upgrade() else { return };
            if view.update(cx, |v, cx| if paste { v.paste_image(window, cx) } else { v.slash_key(key, window, cx) }) {
                cx.stop_propagation();
            }
        }));
        let (options, commands) = load_picks();
        let smooth = crate::glide::Smooth::default();
        let mut view = Self {
            id,
            cwd,
            projects: vec![],
            tie_next: None,
            auto_compact: None,
            compact_at: None,
            startup: None,
            queued: vec![],
            carry: Carry::None,
            phase: Phase::Working(START),
            items: vec![],
            meta: Meta { options, commands, ..Meta::default() },
            early: vec![],
            launch,
            resume,
            session_id: None,
            agent: None,
            epoch: 0,
            pending: Default::default(),
            inflight: 0,
            perms: vec![],
            expanded: HashSet::new(),
            input,
            attachments: vec![],
            sent_images: HashMap::new(),
            relaunch: false,
            slash_sel: 0,
            slash_scroll: ScrollHandle::new(),
            slash_off: false,
            picker: None,
            scroll: smooth.handle(),
            smooth,
            follow: true,
            cached_shown: false,
            replay: vec![],
            focus: cx.focus_handle(),
            _subs: subs,
        };
        view.start(window, cx);
        view
    }

    /// Put back what was typed before. The cursor goes to the end.
    pub fn set_draft(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| {
            s.set_value(text.to_string(), window, cx);
            s.set_selected_range(text.len()..text.len(), cx);
        });
    }

    /// Keep the model, effort and command choices for the next chat to show while it starts.
    fn save_picks(&self, cx: &mut Context<Self>) {
        let (options, commands) = (self.meta.options.clone(), self.meta.commands.clone());
        cx.background_executor().spawn(async move { save_picks(&options, &commands) }).detach();
    }

    fn save_cache(&self, cx: &mut Context<Self>) {
        let (id, items) = (self.id.clone(), self.items.clone());
        cx.background_executor().spawn(async move { save_cache(&id, &items) }).detach();
    }

    pub fn is_busy(&self) -> bool {
        self.inflight > 0
    }

    pub fn set_projects(&mut self, projects: Vec<(String, PathBuf)>) {
        self.projects = projects;
    }

    pub fn set_compact(&mut self, auto_compact: Option<bool>, compact_at: Option<u64>, cx: &mut Context<Self>) {
        (self.auto_compact, self.compact_at) = (auto_compact, compact_at);
        cx.notify();
    }

    /// Tie the chat to a project once the turn that asked for it is over (restarting the agent now would cut it off).
    pub fn tie_after_turn(&mut self, project: PathBuf) {
        self.tie_next = Some(project);
    }

    /// Send this as the first message as soon as the conversation is ready.
    pub fn send_when_ready(&mut self, text: String) {
        self.startup = Some(text);
    }

    /// Whether the chat works in a project folder (it does not by default).
    pub fn tied(&self) -> bool {
        super::store::is_tied(&self.cwd)
    }

    /// The project can be changed any time the chat is idle (not mid-turn, not waiting on an approval or loading an
    /// old conversation). A different folder means a new Claude conversation there.
    fn can_retie(&self) -> bool {
        let idle = !matches!(self.carry, Carry::Summarizing) && self.inflight == 0 && self.perms.is_empty() && self.startup.is_none() && self.queued.is_empty();
        idle && match self.phase {
            Phase::Ready => true,
            Phase::Working(_) => self.resume.is_none(),
            _ => false,
        }
    }

    pub fn set_project(&mut self, project: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let cwd = project.unwrap_or_else(scratch_dir);
        if !self.can_retie() || crate::platform::path_key(&cwd) == crate::platform::path_key(&self.cwd) {
            cx.notify();
            return;
        }
        // A conversation already under way stays on screen above a notice; Claude itself starts afresh in the folder
        // (Claude Code keeps a session with the folder it began in).
        let shown = (!self.items.is_empty()).then(|| (std::mem::take(&mut self.items), std::mem::take(&mut self.sent_images)));
        let place = if super::store::is_tied(&cwd) { cwd.file_name().map_or_else(|| cwd.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned()) } else { "no project".into() };
        self.cwd = cwd.clone();
        self.session_id = None;
        self.resume = None;
        cx.emit(ChatEvent::Cwd(cwd));
        self.start(window, cx);
        if let Some((items, images)) = shown {
            let mut said = model::transcript(&items, 200);
            if let Carry::Ready(earlier) = &self.carry {
                said = format!("Summary of what came before:
{earlier}

{said}");
            }
            self.items = items;
            self.sent_images = images;
            self.notice(format!("Now working in {place}. This is a new conversation; a summary of the messages above goes along with your next message."), false);
            self.carry = Carry::Pending(said);
        }
    }

    /// Compacts the earlier conversation (Haiku through the user's `claude`) for the message about to be sent.
    fn summarize(&mut self, said: String, cx: &mut Context<Self>) {
        let Some(claude) = self.launch.as_ref().map(|l| l.claude.clone()) else { return };
        self.carry = Carry::Summarizing;
        cx.spawn(async move |this, cx| {
            let summary = cx
                .background_executor()
                .spawn(async move {
                    // The summary is Haiku's; if that fails the tail of the conversation itself goes instead.
                    crate::commit::generate(&claude, COMPACT_SYSTEM, &format!("Conversation:

{said}

Write the summary.")).unwrap_or_else(|_| {
                        let skip = said.chars().count().saturating_sub(6000);
                        said.chars().skip(skip).collect()
                    })
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.carry = Carry::Ready(summary);
                v.flush_queue();
                cx.notify();
            });
        })
        .detach();
    }

    /// The last `last` messages as plain text, for another session that is allowed to read this chat.
    pub fn transcript(&self, last: usize) -> String {
        model::transcript(&self.items, last)
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    // ------------------------------------------------------------------------------------------ the adapter

    /// Install the adapter if needed, start it, then run the handshake. Everything slow happens off the UI thread.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent = None;
        self.epoch += 1;
        let epoch = self.epoch;
        self.pending.clear();
        self.inflight = 0;
        self.perms.clear();
        self.replay.clear();
        // A conversation being loaded shows what was last seen (the saved copy, or what a restart interrupted).
        if self.resume.is_some() {
            if self.items.is_empty() {
                self.items = load_cache(&self.id);
            }
            self.cached_shown = !self.items.is_empty();
        } else {
            self.items.clear();
            self.sent_images.clear();
            self.cached_shown = false;
        }
        let Some(launch) = self.launch.clone() else {
            self.phase = Phase::NoClaude;
            cx.notify();
            return;
        };
        self.phase = Phase::Working(if acp::installed() { START } else { "Setting up the Claude adapter (first time only)…" });
        cx.notify();
        let chat = self.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move {
                    if !acp::has_node() {
                        return Err(None);
                    }
                    if !acp::installed() {
                        acp::install().map_err(Some)?;
                    }
                    Agent::attach(&launch.claude, launch.env, &chat).map_err(Some)
                })
                .await;
            // `this` stays weak: closing the chat drops the view, which drops the agent and ends this loop.
            // A newer `start` (Try again) bumps `epoch`, which retires this one.
            let rx = this.update(cx, |v, cx| {
                if v.epoch != epoch {
                    return None;
                }
                match started {
                    Ok((mut agent, rx)) => {
                        let id = agent.call("initialize", json!({"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}}}));
                        v.pending.insert(id, Pending::Init);
                        v.agent = Some(agent);
                        Some(rx)
                    }
                    Err(None) => {
                        v.phase = Phase::NoNode;
                        cx.notify();
                        None
                    }
                    Err(Some(e)) => {
                        v.phase = Phase::Failed(e);
                        cx.notify();
                        None
                    }
                }
            });
            let Ok(Some(mut rx)) = rx else { return };
            while let Some(msg) = rx.next().await {
                match this.update_in(cx, |v, window, cx| (v.epoch == epoch).then(|| v.on_msg(msg, window, cx))) {
                    Ok(Some(())) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    fn near_bottom(&self) -> bool {
        self.scroll.offset().y <= -self.scroll.max_offset().y + px(48.0)
    }

    fn notice(&mut self, text: impl Into<String>, error: bool) {
        self.items.push(Item::Notice { text: text.into(), error });
    }

    fn on_msg(&mut self, msg: Msg, window: &mut Window, cx: &mut Context<Self>) {
        let near = self.near_bottom();
        match msg {
            Msg::Notification { method, params } => {
                if method == "session/update"
                    && let Some(u) = params.get("update")
                {
                    let (title, context, commands) = (self.meta.title.clone(), self.meta.context, self.meta.commands.len());
                    model::apply(if self.cached_shown { &mut self.replay } else { &mut self.items }, &mut self.meta, u);
                    if self.meta.commands.len() != commands {
                        self.save_picks(cx);
                    }
                    if self.meta.title != title
                        && let Some(t) = self.meta.title.clone()
                    {
                        cx.emit(ChatEvent::Title(t));
                    }
                    if self.meta.context != context
                        && let Some((used, size)) = self.meta.context
                    {
                        cx.emit(ChatEvent::Context(used, size));
                    }
                    // The 5-hour and 7-day windows ride along on usage updates (utilization is 0..1).
                    if let Some(w) = u.pointer("/_meta/_claude~1rateLimit/unifiedWindows") {
                        let t = now();
                        let limit = |k: &str| {
                            let w = w.get(k)?;
                            Some(RateLimit {
                                utilization: Some(w.get("utilization")?.as_f64()?.clamp(0.0, 1.0) as f32),
                                resets_at: w.get("resetsAt").and_then(Value::as_i64),
                                seen_at: t,
                            })
                        };
                        cx.emit(ChatEvent::Limits { five: limit("five_hour"), seven: limit("seven_day") });
                    }
                }
            }
            Msg::Request { id, method, params } if method == "session/request_permission" => {
                let tool = params.get("toolCall");
                let title = tool.and_then(|t| t.get("title")).and_then(Value::as_str).unwrap_or("Claude wants to use a tool").to_string();
                let options = params
                    .get("options")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|o| {
                        let kind = o.get("kind").and_then(Value::as_str).unwrap_or_default();
                        Some((o.get("optionId")?.as_str()?.to_string(), o.get("name")?.as_str()?.to_string(), kind.starts_with("allow")))
                    })
                    .collect();
                self.perms.push(Perm { rpc_id: id, title, options });
            }
            // Files and terminals are not offered to the agent, so nothing else is expected; never leave one hanging.
            Msg::Request { id, .. } => {
                if let Some(a) = &self.agent {
                    a.respond(id, Err("not supported by Claudiu"));
                }
            }
            Msg::Reply { id, result } => self.on_reply(id, result, window, cx),
            Msg::Exit(tail) => {
                self.agent = None;
                self.inflight = 0;
                self.pending.clear();
                self.perms.clear();
                let why = tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("The Claude adapter stopped.").to_string();
                if self.phase == Phase::Ready {
                    self.notice(format!("The Claude adapter stopped: {why}"), true);
                } else {
                    self.phase = Phase::Failed(why);
                }
            }
        }
        self.follow = near;
        cx.notify();
    }

    fn on_reply(&mut self, id: u64, result: Result<Value, String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.pending.remove(&id) else { return };
        match (kind, result) {
            (Pending::Init, Ok(_)) => {
                // The cross-session tools are pre-allowed here because Claudiu asks the user itself before it acts on one;
                // the file and shell tools are denied (a chat is read-only, see `acp::READ_ONLY_DENY`).
                let mut params = json!({
                    "cwd": self.cwd,
                    "mcpServers": crate::mcp::chat_servers(&self.id),
                    "systemPrompt": {"mode": "append", "content": [{"type": "text", "text": format!("{}\n\n{}", super::ui::PROMPT, super::acp::handoff_prompt(&self.id, self.tied()))}]},
                    "_meta": {"claudeCode": {"options": super::acp::session_options(&self.id)}},
                });
                let method = if let Some(sid) = self.resume.clone() {
                    params["sessionId"] = sid.into();
                    self.phase = Phase::Working(LOADING);
                    "session/load"
                } else {
                    "session/new"
                };
                if let Some(a) = &mut self.agent {
                    let id = a.call(method, params);
                    self.pending.insert(id, Pending::Session);
                }
            }
            // Claude has no file for a session nothing was said in yet (a project was picked and the app closed): start afresh.
            (Pending::Session, Err(e)) if self.resume.is_some() && e.to_lowercase().contains("not found") => {
                let keep = (std::mem::take(&mut self.items), std::mem::take(&mut self.sent_images));
                self.resume = None;
                self.start(window, cx);
                (self.items, self.sent_images) = keep;
            }
            (Pending::Init | Pending::Session, Err(e)) => self.phase = Phase::Failed(e),
            (Pending::Session, Ok(r)) => {
                let sid = r.get("sessionId").and_then(Value::as_str).map(String::from).or_else(|| self.resume.clone());
                if let Some(sid) = sid {
                    if self.session_id.is_none() && self.resume.as_deref() != Some(sid.as_str()) {
                        cx.emit(ChatEvent::Session(sid.clone()));
                    }
                    self.session_id = Some(sid);
                }
                self.meta.options = model::parse_options(r.get("configOptions").unwrap_or(&Value::Null));
                self.save_picks(cx);
                if std::mem::take(&mut self.cached_shown) {
                    self.items = std::mem::take(&mut self.replay);
                    self.sent_images.clear();
                    self.save_cache(cx);
                }
                self.phase = Phase::Ready;
                for (config, value) in std::mem::take(&mut self.early) {
                    if let Some(i) = self.meta.options.iter().position(|o| o.id == config && o.current != value && o.choices.iter().any(|(v, _)| *v == value)) {
                        self.set_option(i, value, cx);
                    }
                }
                self.flush_queue();
                if let Some(text) = self.startup.take() {
                    self.send(text, window, cx);
                }
                self.maybe_relaunch(window, cx);
            }
            (Pending::Prompt, result) => {
                self.inflight = self.inflight.saturating_sub(1);
                cx.emit(ChatEvent::Turn);
                if let Err(e) = result {
                    self.notice(e, true);
                }
                if self.inflight == 0 {
                    self.save_cache(cx);
                    self.save_picks(cx);
                    if let Some(project) = self.tie_next.take() {
                        self.set_project(Some(project), window, cx);
                    }
                }
                self.maybe_relaunch(window, cx);
            }
            (Pending::SetOption, Ok(r)) => {
                if let Some(o) = r.get("configOptions") {
                    self.meta.options = model::parse_options(o);
                }
            }
            (Pending::SetOption, Err(e)) => cx.emit(ChatEvent::Toast { text: e, error: true }),
        }
    }

    // ------------------------------------------------------------------------------------------ user actions

    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if self.send(text, window, cx) {
            self.input.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }

    /// Send a message (with any attached images). While the conversation is still starting it is queued and goes out
    /// the moment it is ready. False when nothing was sent.
    fn send(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let live = self.phase == Phase::Ready && self.session_id.is_some();
        if !live && !matches!(self.phase, Phase::Working(_)) {
            return false;
        }
        if let Carry::Pending(said) = std::mem::replace(&mut self.carry, Carry::None) {
            if text.is_empty() && self.attachments.is_empty() {
                self.carry = Carry::Pending(said);
            } else {
                self.summarize(said, cx);
            }
        }
        // Still compacting the earlier conversation: wait for it, like for the agent.
        let ready = live && !matches!(self.carry, Carry::Summarizing);
        if text.is_empty() && self.attachments.is_empty() {
            return false;
        }
        let images = std::mem::take(&mut self.attachments);
        let mut prompt: Vec<Value> = images
            .iter()
            .map(|a| json!({"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&a.image.bytes), "mimeType": a.mime}))
            .collect();
        if !text.is_empty() {
            prompt.push(json!({"type": "text", "text": text}));
        }
        let images: Vec<Arc<Image>> = images.into_iter().map(|a| a.image).collect();
        if ready {
            self.items.push(Item::User(text.clone()));
            if !images.is_empty() {
                self.sent_images.insert(self.items.len() - 1, images);
            }
            self.dispatch(prompt);
        } else {
            self.queued.push((text.clone(), images, prompt));
        }
        self.follow = true;
        cx.emit(ChatEvent::Sent(if text.is_empty() { "Image".into() } else { text }));
        cx.notify();
        true
    }

    /// Deliver the messages that waited for the conversation (or for the summary of the earlier one).
    fn flush_queue(&mut self) {
        if self.phase != Phase::Ready || matches!(self.carry, Carry::Summarizing) {
            return;
        }
        for (text, images, prompt) in std::mem::take(&mut self.queued) {
            self.items.push(Item::User(text));
            if !images.is_empty() {
                self.sent_images.insert(self.items.len() - 1, images);
            }
            self.dispatch(prompt);
        }
    }

    fn dispatch(&mut self, mut prompt: Vec<Value>) {
        let Some(sid) = self.session_id.clone() else { return };
        if let Carry::Ready(summary) = std::mem::replace(&mut self.carry, Carry::None) {
            prompt.insert(0, json!({"type": "text", "text": format!("(Context: this chat continues from an earlier conversation in another folder. Summary of it:

{summary}

The user's message follows.)")}));
        }
        if let Some(a) = &mut self.agent {
            let id = a.call("session/prompt", json!({"sessionId": sid, "prompt": prompt}));
            self.pending.insert(id, Pending::Prompt);
            self.inflight += 1;
        }
    }

    /// Ctrl/Cmd+V with a picture (or an image file) on the clipboard attaches it. Text still pastes as text.
    fn paste_image(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.input.read(cx).focus_handle(cx).is_focused(window) {
            return false;
        }
        let Some(found) = cx.read_from_clipboard().as_ref().and_then(clipboard_image) else { return false };
        match found {
            Ok(a) => self.attachments.push(a),
            Err(e) => cx.emit(ChatEvent::Toast { text: e, error: true }),
        }
        cx.notify();
        true
    }

    /// The account changed: the next idle moment, restart the agent under the new login and load the same conversation.
    pub fn relaunch(&mut self, launch: Option<Launch>, window: &mut Window, cx: &mut Context<Self>) {
        self.launch = launch;
        self.relaunch = true;
        self.maybe_relaunch(window, cx);
    }

    fn maybe_relaunch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settled = matches!(self.phase, Phase::Ready | Phase::Failed(_) | Phase::NoClaude);
        if self.relaunch && settled && self.inflight == 0 && self.perms.is_empty() {
            self.relaunch = false;
            self.resume = self.session_id.clone().or(self.resume.take());
            self.start(window, cx);
        }
    }

    /// Stop the turn: the agent finishes the prompt with a "cancelled" answer, and open approvals are declined.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        for p in self.perms.drain(..) {
            if let Some(a) = &self.agent {
                a.respond(p.rpc_id, Ok(json!({"outcome": {"outcome": "cancelled"}})));
            }
        }
        if let (Some(a), Some(sid)) = (&self.agent, &self.session_id) {
            a.notify("session/cancel", json!({"sessionId": sid}));
        }
        cx.notify();
    }

    fn answer(&mut self, idx: usize, option: String, cx: &mut Context<Self>) {
        if idx < self.perms.len() {
            let p = self.perms.remove(idx);
            if let Some(a) = &self.agent {
                a.respond(p.rpc_id, Ok(json!({"outcome": {"outcome": "selected", "optionId": option}})));
            }
            cx.notify();
        }
    }

    fn set_option(&mut self, idx: usize, value: String, cx: &mut Context<Self>) {
        self.picker = None;
        let Some(o) = self.meta.options.get_mut(idx) else { return };
        let config = o.id.clone();
        o.current = value.clone();
        let Some(sid) = self.session_id.clone() else {
            // The options shown are the last session's: remember the pick and apply it when the session is up.
            self.early.retain(|(id, _)| *id != config);
            self.early.push((config, value));
            cx.notify();
            return;
        };
        if let Some(a) = &mut self.agent {
            let id = a.call("session/set_config_option", json!({"sessionId": sid, "configId": config, "value": value}));
            self.pending.insert(id, Pending::SetOption);
        }
        cx.notify();
    }

    /// The commands the text box currently offers: it starts with `/` and has no space yet.
    /// The `/word` the cursor is in: it starts the text or follows whitespace. Returns its byte range and what
    /// was typed after the slash.
    fn slash_token(&self, cx: &App) -> Option<(std::ops::Range<usize>, String)> {
        let state = self.input.read(cx);
        model::slash_token(&state.value(), state.cursor())
    }

    fn slash_matches(&self, cx: &App) -> Vec<Command> {
        match self.slash_token(cx) {
            Some((_, typed)) if !self.slash_off => model::matching(&self.meta.commands, &typed).into_iter().cloned().collect(),
            _ => vec![],
        }
    }

    /// A key while the command list may be open. Returns true when the list used it.
    fn slash_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.input.read(cx).focus_handle(cx).is_focused(window) {
            return false;
        }
        let matches = self.slash_matches(cx);
        if matches.is_empty() {
            return false;
        }
        match key {
            "up" => {
                self.slash_sel = (self.slash_sel.min(matches.len() - 1) + matches.len() - 1) % matches.len();
                self.slash_scroll.scroll_to_item(self.slash_sel);
            }
            "down" => {
                self.slash_sel = (self.slash_sel.min(matches.len() - 1) + 1) % matches.len();
                self.slash_scroll.scroll_to_item(self.slash_sel);
            }
            "escape" => self.slash_off = true,
            "tab" | "enter" => self.pick_command(&matches[self.slash_sel.min(matches.len() - 1)], window, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Completes the `/word` at the cursor. When it is the whole message and the command takes no arguments it
    /// runs at once; otherwise the name is filled in (with a space after) for the user to go on typing.
    fn pick_command(&mut self, c: &Command, window: &mut Window, cx: &mut Context<Self>) {
        let takes_args = c.hint.is_some();
        let text = self.input.read(cx).value().to_string();
        let Some((range, _)) = self.slash_token(cx) else { return };
        let whole = range.start == 0 && range.end == text.len();
        if whole && !takes_args {
            self.input.update(cx, |s, cx| s.set_value(format!("/{}", c.name), window, cx));
            self.submit(window, cx);
            return;
        }
        let rest = &text[range.end..];
        let space = if rest.starts_with(char::is_whitespace) { "" } else { " " };
        let head = format!("{}/{}{space}", &text[..range.start], c.name);
        // The cursor goes after the space, which may be the one that was already there.
        let cursor = head.len() + if space.is_empty() { rest.chars().next().map_or(0, char::len_utf8) } else { 0 };
        self.input.update(cx, |s, cx| {
            s.set_value(format!("{head}{rest}"), window, cx);
            s.set_selected_range(cursor..cursor, cx);
        });
        self.slash_off = true;
        self.focus_input(window, cx);
        cx.notify();
    }
}

// ---------------------------------------------------------------------------------------------- drawing

fn dim(text: impl Into<SharedString>) -> Div {
    div().text_size(px(12.0)).text_color(hsla(theme::TEXT_DIM)).child(text.into())
}

fn tool_glyph(t: &Tool) -> (IconName, u32) {
    match t.status.as_str() {
        "completed" => (IconName::Check, theme::OK),
        "failed" => (IconName::X, theme::DANGER),
        _ => (IconName::LoaderCircle, theme::ACCENT),
    }
}

/// A status mark: the loader turns, the other icons stay still.
fn status_mark(id: impl Into<gpui::ElementId>, name: IconName, size: f32) -> AnyElement {
    if name == IconName::LoaderCircle { spinner(id, size).into_any_element() } else { icon(name, size).into_any_element() }
}

/// A line saying something is going on, with the turning loader before it.
fn working(text: impl Into<SharedString>) -> Div {
    div().flex().items_center().gap(px(6.0)).text_size(px(12.0)).text_color(hsla(theme::TEXT_DIM)).child(spinner("chat-working", 13.0)).child(text.into())
}

/// The last lines of a tool's output: enough to see what happened without a wall of text.
fn tail(output: &str, lines: usize) -> String {
    let all: Vec<&str> = output.trim_end().lines().collect();
    let skipped = all.len().saturating_sub(lines);
    let mut out = String::new();
    if skipped > 0 {
        out.push_str(&format!("… {skipped} earlier lines\n"));
    }
    out.push_str(&all[skipped..].join("\n"));
    out
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.follow {
            self.smooth.cancel();
            self.scroll.scroll_to_bottom();
        }
        let body = self.render_chat(window, cx);
        div()
            .id("chat")
            .key_context("Chat")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(theme::APP_BG))
            .text_color(hsla(theme::TEXT))
            .text_size(px(13.5))
            .child(body)
            .children(self.render_picker(cx))
    }
}

impl ChatView {
    fn render_state(&self, phase: Phase, cx: &mut Context<Self>) -> AnyElement {
        let (title, text, actions): (&str, String, Vec<(&str, Option<&str>)>) = match phase {
            Phase::Working(text) => ("", text.to_string(), vec![]),
            Phase::NoClaude => ("Claude Code not found", "Chat runs your own `claude` command. Install Claude Code or set its path in Settings, then try again.".into(), vec![("Try again", None)]),
            Phase::NoNode => (
                "Node.js not found",
                "Chat talks to Claude Code through a small Node.js program. Install Node.js (nodejs.org), then try again.".into(),
                vec![("Get Node.js", Some("https://nodejs.org")), ("Try again", None)],
            ),
            Phase::Failed(msg) => ("Could not start the chat", msg, vec![("Try again", None)]),
            Phase::Ready => return div().into_any_element(),
        };
        let mut row = div().flex().gap(px(8.0)).mt(px(14.0));
        for (i, (label, url)) in actions.into_iter().enumerate() {
            row = row.child(button(SharedString::from(format!("chat-state-{i}")), label, i == 0).on_click(cx.listener(move |v, _: &ClickEvent, window, cx| match url {
                Some(url) => cx.open_url(url),
                None => v.start(window, cx),
            })));
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(24.0))
            .child(clawd(44.0))
            .when(!title.is_empty(), |d| d.child(div().mt(px(14.0)).text_size(px(15.0)).font_weight(gpui::FontWeight::SEMIBOLD).child(title.to_string())))
            .child(div().mt(px(6.0)).max_w(px(460.0)).text_center().child(dim(text)))
            .child(row)
            .into_any_element()
    }

    fn render_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.inflight > 0;
        let mut col = div().flex().flex_col().gap(px(14.0)).w_full().max_w(px(780.0)).mx_auto().px(px(20.0)).py(px(18.0));
        if self.items.is_empty() && self.queued.is_empty() {
            col = col.child(
                div()
                    .mt(px(80.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.0))
                    .child(clawd(44.0))
                    .child(dim(if self.tied() { "Ask Claude anything about this project. Type / for skills and commands." } else { "Ask Claude anything. Type / for skills and commands, or tie the chat to a project below." })),
            );
        }
        let last = self.items.len().saturating_sub(1);
        // A failed start replaces the conversation, not the message box: the box stays where it was.
        let state = (!matches!(self.phase, Phase::Ready | Phase::Working(_))).then(|| self.render_state(self.phase.clone(), cx));
        for (i, item) in self.items.iter().enumerate() {
            if let Some(el) = self.render_item(i, item, i == last && busy, cx) {
                col = col.child(el);
            }
        }
        // Messages sent while the conversation was starting wait here in order.
        for (k, (text, _, _)) in self.queued.iter().enumerate() {
            if let Some(el) = self.render_item(self.items.len() + k, &Item::User(text.clone()), false, cx) {
                col = col.child(el);
            }
        }
        // A plain start is invisible; setting up the adapter or loading an old conversation says so.
        if matches!(self.carry, Carry::Summarizing) && !self.queued.is_empty() {
            col = col.child(working("Carrying over the earlier conversation…"));
        } else if let Phase::Working(what) = self.phase
            && what != START
            && !(what == LOADING && self.cached_shown)
        {
            col = col.child(working(what));
        } else if busy && !matches!(self.items.last(), Some(Item::Thought(_))) {
            col = col.child(working("Working…"));
        }

        let mut footer = div().flex().flex_col().gap(px(8.0)).w_full().max_w(px(780.0)).mx_auto().px(px(20.0)).pb(px(14.0));
        for (i, p) in self.perms.iter().enumerate() {
            let mut buttons = div().flex().flex_wrap().gap(px(6.0)).mt(px(8.0));
            for (opt, label, allow) in &p.options {
                let opt = opt.clone();
                buttons = buttons.child(button(SharedString::from(format!("perm-{i}-{opt}")), label.clone(), *allow).on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.answer(i, opt.clone(), cx))));
            }
            footer = footer.child(
                div()
                    .p(px(12.0))
                    .rounded(px(10.0))
                    .bg(hsla(0x15110f))
                    .border_1()
                    .border_color(hsla(0x3a2a24))
                    .child(div().text_size(px(12.5)).text_color(hsla(theme::ACCENT)).child("Claude asks permission"))
                    .child(div().mt(px(4.0)).text_size(px(13.0)).child(p.title.clone()))
                    .child(buttons),
            );
        }
        let matches = self.slash_matches(cx);
        if !matches.is_empty() {
            let sel = self.slash_sel.min(matches.len() - 1);
            // Every skill and command is in the list: it scrolls (wheel, or Up/Down with the selection kept in view).
            let mut list = div()
                .id("slash-list")
                .flex()
                .flex_col()
                .max_h(px(280.0))
                .overflow_y_scroll()
                .track_scroll(&self.slash_scroll)
                .p(px(4.0))
                .rounded(px(10.0))
                .bg(hsla(0x151515))
                .border_1()
                .border_color(hsla(0x2a2a2a));
            for (i, c) in matches.into_iter().enumerate() {
                let hint = c.hint.clone().filter(|h| !h.is_empty());
                list = list.child(
                    div()
                        .id(SharedString::from(format!("slash-{i}")))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .px(px(10.0))
                        .py(px(5.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .when(i == sel, |d| d.bg(hsla(theme::ROW_ACTIVE)))
                        .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
                        .child(div().flex_none().text_size(px(12.5)).text_color(hsla(theme::ACCENT)).child(format!("/{}", c.name)))
                        .children(hint.map(|h| div().flex_none().text_size(px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(h)))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(11.5)).text_color(hsla(theme::TEXT_DIM)).child(c.description.clone()))
                        .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| v.pick_command(&c, window, cx))),
                );
            }
            footer = footer.child(list);
        }

        // Fixed height, so the row does not jump when the model and effort chips arrive.
        let mut chips = div().flex().items_center().gap(px(4.0)).min_h(px(26.0));
        let (tied, can_pick) = (self.tied(), self.can_retie() && !self.projects.is_empty());
        {
            let name = if tied {
                let key = crate::platform::path_key(&self.cwd);
                self.projects
                    .iter()
                    .find(|(_, p)| crate::platform::path_key(p) == key)
                    .map(|(n, _)| n.clone())
                    .or_else(|| self.cwd.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "Project".into())
            } else {
                "No project".into()
            };
            chips = chips.child(
                div()
                    .id("chat-project")
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .text_size(px(11.5))
                    .text_color(hsla(if tied { theme::ACCENT } else { theme::TEXT_DIM }))
                    .when(can_pick, |d| {
                        d.cursor_pointer().hover(|s| s.bg(hsla(0x222222))).on_mouse_down(MouseButton::Left, cx.listener(|v, ev: &MouseDownEvent, _, cx| {
                            v.picker = Some((PROJECT_PICKER, ev.position));
                            cx.notify();
                        }))
                    })
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .child(name)
                    // Always drawn (dimmed when the project can't change) so the chip keeps its width.
                    .child(div().opacity(if can_pick { 1.0 } else { 0.35 }).child(icon(IconName::ChevronDown, 12.0))),
            );
        }
        for (i, o) in self.meta.options.iter().enumerate() {
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("chat-opt-{i}")))
                    .px(px(8.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .text_size(px(11.5))
                    .text_color(hsla(theme::TEXT_DIM))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(0x222222)).text_color(hsla(theme::TEXT)))
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .child(o.current_label().to_string())
                    .child(icon(IconName::ChevronDown, 12.0))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
                        v.picker = Some((i, ev.position));
                        cx.notify();
                    })),
            );
        }
        let context = self.meta.context.map(|(used, size)| {
            let full = crate::claude::context_limit(Some(size), self.compact_at, self.auto_compact).unwrap_or(size);
            format!("{}k / {}k", used / 1000, full / 1000)
        });
        let send = if busy {
            button("chat-stop", "Stop", false).on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.stop(cx)))
        } else {
            button("chat-send", "Send", true).on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.submit(window, cx)))
        };
        footer = footer.child(
            div()
                .p(px(10.0))
                .rounded(px(12.0))
                .bg(hsla(0x131313))
                .border_1()
                .border_color(hsla(0x262626))
                .flex()
                .flex_col()
                .gap(px(6.0))
                .when(!self.attachments.is_empty(), |d| {
                    let mut row = div().flex().flex_wrap().gap(px(8.0));
                    for (i, a) in self.attachments.iter().enumerate() {
                        row = row.child(
                            div()
                                .relative()
                                .child(img(a.image.clone()).w(px(64.0)).h(px(64.0)).rounded(px(8.0)))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("chat-att-{i}")))
                                        .absolute()
                                        .top(px(-6.0))
                                        .right(px(-6.0))
                                        .size(px(18.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(hsla(0x2c2c2c))
                                        .text_size(px(11.0))
                                        .text_color(hsla(theme::TEXT))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(hsla(theme::DANGER)))
                                        .child(icon(IconName::X, 12.0))
                                        .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                            v.attachments.remove(i);
                                            cx.notify();
                                        })),
                                ),
                        );
                    }
                    d.child(row)
                })
                .child(Textarea::new(&self.input))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(chips)
                        .child(div().flex().items_center().gap(px(10.0)).children(context.map(|c| div().text_size(px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(c))).child(send)),
                ),
        );
        let _ = window;
        div()
            .size_full()
            .flex()
            .flex_col()
            // Scrolling by hand ends following; the next message resumes it if the view is back at the bottom.
            .child(match state {
                Some(state) => div().flex_1().min_h_0().child(state).into_any_element(),
                None => self.smooth.wire(div().id("chat-scroll").flex_1().min_h_0(), window).on_scroll_wheel(cx.listener(|v, _: &ScrollWheelEvent, _, _| v.follow = false)).child(col).into_any_element(),
            })
            .child(footer)
            .into_any_element()
    }

    fn render_item(&self, i: usize, item: &Item, live: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        Some(match item {
            Item::User(text) => div()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(6.0))
                .children(self.sent_images.get(&i).map(|images| {
                    images.iter().fold(div().flex().flex_wrap().justify_end().gap(px(6.0)), |row, image| {
                        row.child(img(image.clone()).max_w(px(220.0)).max_h(px(160.0)).rounded(px(10.0)))
                    })
                }))
                .when(!text.is_empty(), |d| {
                    d.child(div().max_w(px(600.0)).px(px(13.0)).py(px(9.0)).rounded(px(12.0)).bg(hsla(0x1b1b1b)).child(text.clone()))
                })
                .into_any_element(),
            Item::Agent(text) => div().child(markdown(format!("chat-md-{i}"), text)).into_any_element(),
            Item::Thought(_) if live => div().child(dim("Thinking…")).into_any_element(),
            Item::Thought(_) => return None,
            Item::Tool(t) => {
                let (glyph, color) = tool_glyph(t);
                let id = t.id.clone();
                let open = self.expanded.contains(&t.id);
                let mut card = div()
                    .rounded(px(8.0))
                    .bg(hsla(0x111112))
                    .border_1()
                    .border_color(hsla(0x222223))
                    .child(
                        div()
                            .id(SharedString::from(format!("chat-tool-{i}")))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .px(px(10.0))
                            .py(px(6.0))
                            .cursor_pointer()
                            .child(div().flex_none().text_color(hsla(color)).child(status_mark(SharedString::from(format!("tool-spin-{}", t.id)), glyph, 14.0)))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.5)).child(t.title.clone()))
                            .child(div().flex_none().text_color(hsla(theme::TEXT_FAINT)).child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                if !v.expanded.remove(&id) {
                                    v.expanded.insert(id.clone());
                                }
                                cx.notify();
                            })),
                    );
                if open && !t.output.is_empty() {
                    card = card.child(
                        div()
                            .px(px(10.0))
                            .pb(px(8.0))
                            .font_family(crate::terminal_view::FONT)
                            .text_size(px(11.5))
                            .text_color(hsla(theme::TEXT_DIM))
                            .child(tail(&t.output, 14)),
                    );
                }
                card.into_any_element()
            }
            Item::Plan(entries) => {
                let mut col = div().flex().flex_col().gap(px(3.0)).p(px(10.0)).rounded(px(8.0)).bg(hsla(0x111112)).border_1().border_color(hsla(0x222223));
                for (text, status) in entries {
                    let (glyph, color) = match status.as_str() {
                        "completed" => (IconName::CircleCheck, theme::OK),
                        "in_progress" => (IconName::LoaderCircle, theme::ACCENT),
                        _ => (IconName::Circle, theme::TEXT_FAINT),
                    };
                    col = col.child(div().flex().gap(px(8.0)).text_size(px(12.5)).child(div().pt(px(2.0)).text_color(hsla(color)).child(status_mark(SharedString::from(format!("plan-spin-{text}")), glyph, 13.0))).child(text.clone()));
                }
                col.into_any_element()
            }
            Item::Notice { text, error } => div().text_size(px(12.0)).text_color(hsla(if *error { theme::DANGER } else { theme::TEXT_FAINT })).child(text.clone()).into_any_element(),
        })
    }

    fn render_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (idx, pos) = self.picker?;
        // (label, is current, what choosing it does)
        let rows: Vec<(String, bool, Choice)> = if idx == PROJECT_PICKER {
            let key = crate::platform::path_key(&self.cwd);
            std::iter::once(("No project".to_string(), !self.tied(), Choice::Project(None)))
                .chain(self.projects.iter().map(|(n, p)| (n.clone(), self.tied() && crate::platform::path_key(p) == key, Choice::Project(Some(p.clone())))))
                .collect()
        } else {
            let o = self.meta.options.get(idx)?;
            o.choices.iter().map(|(v, l)| (l.clone(), *v == o.current, Choice::Option(idx, v.clone()))).collect()
        };
        let mut list = div().id("chat-pop-list").flex().flex_col().max_h(px(320.0)).overflow_y_scroll();
        for (n, (label, current, choice)) in rows.into_iter().enumerate() {
            list = list.child(
                div()
                    .id(SharedString::from(format!("chat-pop-{n}")))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(28.0))
                    .px(px(10.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .hover(|s| s.bg(hsla(0x252525)))
                    .child(div().w(px(12.0)).flex_none().text_color(hsla(theme::ACCENT)).children(current.then(|| icon(IconName::Check, 12.0))))
                    .child(label)
                    .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| match choice.clone() {
                        Choice::Option(i, value) => v.set_option(i, value, cx),
                        Choice::Project(p) => v.set_project(p, window, cx),
                    })),
            );
        }
        let card = div().id("chat-popup").occlude().w(px(240.0)).p(px(4.0)).rounded(px(8.0)).bg(hsla(0x181818)).border_1().border_color(hsla(0x2c2c2c)).shadow_lg().child(list);
        let close = cx.listener(|v: &mut ChatView, _: &MouseDownEvent, _, cx| {
            v.picker = None;
            cx.notify();
        });
        Some(
            deferred(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, close)
                    .child(anchored().position(pos).snap_to_window_with_margin(px(8.0)).child(fade_in(card, format!("chat-pop-in-{idx}"), 110, 0.0))),
            )
            .with_priority(5)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
impl ChatView {
    pub fn probe(&self) -> (Phase, Vec<Item>, usize) {
        (self.phase.clone(), self.items.clone(), self.meta.commands.len())
    }

    pub fn send_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.set_value(text, window, cx));
        self.submit(window, cx);
    }
}

#[cfg(test)]
impl ChatView {
    pub fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.paste_image(window, cx)
    }

    pub fn attached(&self) -> usize {
        self.attachments.len()
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}
