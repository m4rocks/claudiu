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
    MouseDownEvent, ParentElement, Pixels, Point, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, anchored, deferred, div, prelude::FluentBuilder, px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use serde_json::{Value, json};

use super::acp::{self, Agent, Msg};
use super::model::{self, Command, Item, Meta, Tool};
use super::store::scratch_dir;
use crate::github::ui::markdown;
use crate::store::{RateLimit, now};
use crate::theme::{self, hsla};
use crate::widgets::{button, clawd, fade_in};

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
    /// The user tied the (still empty) chat to a project, or untied it: a new conversation in this folder.
    Cwd(PathBuf),
}

/// `picker` value that means the project list rather than one of the agent's options.
const PROJECT_PICKER: usize = usize::MAX;

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
    /// A first message to send as soon as the conversation is ready (a chat opened by another session).
    startup: Option<String>,
    pub phase: Phase,
    items: Vec<Item>,
    meta: Meta,
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
    /// Esc closed the command list; it stays closed until the text changes.
    slash_off: bool,
    picker: Option<(usize, Point<Pixels>)>,
    scroll: ScrollHandle,
    follow: bool,
    pub focus: FocusHandle,
    _subs: Vec<Subscription>,
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
                cx.notify();
            }
            _ => {}
        })];
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
        let mut view = Self {
            id,
            cwd,
            projects: vec![],
            startup: None,
            phase: Phase::Working("Starting Claude…"),
            items: vec![],
            meta: Meta::default(),
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
            slash_off: false,
            picker: None,
            scroll: ScrollHandle::new(),
            follow: true,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        view.start(window, cx);
        view
    }

    pub fn is_busy(&self) -> bool {
        self.inflight > 0
    }

    pub fn set_projects(&mut self, projects: Vec<(String, PathBuf)>) {
        self.projects = projects;
    }

    /// Send this as the first message as soon as the conversation is ready.
    pub fn send_when_ready(&mut self, text: String) {
        self.startup = Some(text);
    }

    /// Whether the chat works in a project folder (it does not by default).
    pub fn tied(&self) -> bool {
        super::store::is_tied(&self.cwd)
    }

    /// The project can still be changed while nothing has been said: a different folder means a new conversation.
    fn can_retie(&self) -> bool {
        self.resume.is_none() && self.items.is_empty() && self.inflight == 0 && self.startup.is_none() && self.phase == Phase::Ready
    }

    pub fn set_project(&mut self, project: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let cwd = project.unwrap_or_else(scratch_dir);
        if !self.can_retie() || crate::platform::path_key(&cwd) == crate::platform::path_key(&self.cwd) {
            cx.notify();
            return;
        }
        self.cwd = cwd.clone();
        self.session_id = None;
        cx.emit(ChatEvent::Cwd(cwd));
        self.start(window, cx);
    }

    /// The last `last` messages as plain text, for another session that is allowed to read this chat.
    pub fn transcript(&self, last: usize) -> String {
        let lines: Vec<String> = self
            .items
            .iter()
            .filter_map(|i| match i {
                Item::User(t) => Some(format!("User: {t}")),
                Item::Agent(t) => Some(format!("Claude: {t}")),
                Item::Tool(t) => Some(format!("[tool: {} ({})]", t.title, t.status)),
                Item::Notice { text, .. } => Some(format!("[{text}]")),
                Item::Thought(_) | Item::Plan(_) => None,
            })
            .collect();
        let text = lines[lines.len().saturating_sub(last)..].join("\n\n");
        // Keep the newest part of a long transcript.
        let skip = text.chars().count().saturating_sub(30_000);
        text.chars().skip(skip).collect()
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
        self.items.clear();
        self.sent_images.clear();
        let Some(launch) = self.launch.clone() else {
            self.phase = Phase::NoClaude;
            cx.notify();
            return;
        };
        self.phase = Phase::Working(if acp::installed() { "Starting Claude…" } else { "Setting up the Claude adapter (first time only)…" });
        cx.notify();
        let cwd = self.cwd.clone();
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
                    Agent::spawn(&launch.claude, &cwd, launch.env).map_err(Some)
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
                    let (title, context) = (self.meta.title.clone(), self.meta.context);
                    model::apply(&mut self.items, &mut self.meta, u);
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
                // The cross-session tools are pre-allowed here because Claudiu asks the user itself before it acts on one.
                let mut params = json!({
                    "cwd": self.cwd,
                    "mcpServers": crate::mcp::chat_servers(&self.id),
                    "_meta": {"claudeCode": {"options": {"allowedTools": crate::mcp::allowed_tools("chat")}}},
                });
                let method = if let Some(sid) = self.resume.clone() {
                    params["sessionId"] = sid.into();
                    self.phase = Phase::Working("Loading the conversation…");
                    "session/load"
                } else {
                    "session/new"
                };
                if let Some(a) = &mut self.agent {
                    let id = a.call(method, params);
                    self.pending.insert(id, Pending::Session);
                }
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
                self.phase = Phase::Ready;
                self.focus_input(window, cx);
                if let Some(text) = self.startup.take() {
                    self.send(text, window, cx);
                }
                self.maybe_relaunch(window, cx);
            }
            (Pending::Prompt, result) => {
                self.inflight = self.inflight.saturating_sub(1);
                if let Err(e) = result {
                    self.notice(e, true);
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

    /// Send a message (with any attached images) once the conversation is ready. False when nothing was sent.
    fn send(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (Some(sid), Phase::Ready) = (self.session_id.clone(), &self.phase) else { return false };
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
        self.items.push(Item::User(text.clone()));
        if !images.is_empty() {
            self.sent_images.insert(self.items.len() - 1, images.into_iter().map(|a| a.image).collect());
        }
        self.follow = true;
        if let Some(a) = &mut self.agent {
            let id = a.call("session/prompt", json!({"sessionId": sid, "prompt": prompt}));
            self.pending.insert(id, Pending::Prompt);
            self.inflight += 1;
        }
        cx.emit(ChatEvent::Sent(if text.is_empty() { "Image".into() } else { text }));
        cx.notify();
        true
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
        let (Some(o), Some(sid)) = (self.meta.options.get_mut(idx), self.session_id.clone()) else { return };
        let config = o.id.clone();
        o.current = value.clone();
        if let Some(a) = &mut self.agent {
            let id = a.call("session/set_config_option", json!({"sessionId": sid, "configId": config, "value": value}));
            self.pending.insert(id, Pending::SetOption);
        }
        cx.notify();
    }

    /// The commands the text box currently offers: it starts with `/` and has no space yet.
    fn slash_matches(&self, cx: &App) -> Vec<Command> {
        let text = self.input.read(cx).value();
        match text.strip_prefix('/') {
            Some(typed) if !self.slash_off && !typed.contains(char::is_whitespace) => model::matching(&self.meta.commands, typed).into_iter().take(8).cloned().collect(),
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
            "up" => self.slash_sel = (self.slash_sel + matches.len() - 1) % matches.len(),
            "down" => self.slash_sel = (self.slash_sel + 1) % matches.len(),
            "escape" => self.slash_off = true,
            "tab" | "enter" => self.pick_command(&matches[self.slash_sel.min(matches.len() - 1)], window, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// A command without arguments runs at once; one with arguments is filled in for the user to complete.
    fn pick_command(&mut self, c: &Command, window: &mut Window, cx: &mut Context<Self>) {
        let takes_args = c.hint.is_some();
        self.input.update(cx, |s, cx| s.set_value(format!("/{}{}", c.name, if takes_args { " " } else { "" }), window, cx));
        if takes_args {
            self.slash_off = true;
            self.focus_input(window, cx);
        } else {
            self.submit(window, cx);
        }
    }
}

// ---------------------------------------------------------------------------------------------- drawing

fn dim(text: impl Into<SharedString>) -> Div {
    div().text_size(px(12.0)).text_color(hsla(theme::TEXT_DIM)).child(text.into())
}

fn tool_glyph(t: &Tool) -> (&'static str, u32) {
    match t.status.as_str() {
        "completed" => ("✓", theme::OK),
        "failed" => ("✕", theme::DANGER),
        _ => ("◐", theme::ACCENT),
    }
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
            self.scroll.scroll_to_bottom();
        }
        let body: AnyElement = match self.phase.clone() {
            Phase::Ready => self.render_chat(window, cx),
            other => self.render_state(other, cx),
        };
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
        if self.items.is_empty() {
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
        for (i, item) in self.items.iter().enumerate() {
            if let Some(el) = self.render_item(i, item, i == last && busy, cx) {
                col = col.child(el);
            }
        }
        if busy && !matches!(self.items.last(), Some(Item::Thought(_))) {
            col = col.child(dim("Working…"));
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
            let mut list = div().flex().flex_col().p(px(4.0)).rounded(px(10.0)).bg(hsla(0x151515)).border_1().border_color(hsla(0x2a2a2a));
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

        let mut chips = div().flex().items_center().gap(px(4.0));
        let (tied, can_pick) = (self.tied(), self.can_retie() && !self.projects.is_empty());
        if tied || can_pick {
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
                    .child(if can_pick { format!("{name} ▾") } else { name }),
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
                    .child(format!("{} ▾", o.current_label()))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |v, ev: &MouseDownEvent, _, cx| {
                        v.picker = Some((i, ev.position));
                        cx.notify();
                    })),
            );
        }
        let context = self.meta.context.map(|(used, size)| format!("{}k / {}k", used / 1000, size / 1000));
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
                                        .child("×")
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
            .child(div().id("chat-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).on_scroll_wheel(cx.listener(|v, _, _, _| v.follow = false)).child(col))
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
                            .child(div().flex_none().text_size(px(12.0)).text_color(hsla(color)).child(glyph))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.5)).child(t.title.clone()))
                            .child(div().flex_none().text_size(px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child(if open { "▾" } else { "▸" }))
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
                        "completed" => ("✓", theme::OK),
                        "in_progress" => ("◐", theme::ACCENT),
                        _ => ("○", theme::TEXT_FAINT),
                    };
                    col = col.child(div().flex().gap(px(8.0)).text_size(px(12.5)).child(div().text_color(hsla(color)).child(glyph)).child(text.clone()));
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
                    .child(div().w(px(12.0)).flex_none().text_color(hsla(theme::ACCENT)).child(if current { "✓" } else { "" }))
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
