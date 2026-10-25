//! GPUI view over a `Terminal`: paints the grid, turns GPUI input into VT input.
//! Children: `typing` (keys, paste, IME), `mouse` (selection, reporting, scrolling), `element` (the paint pass).

mod element;
mod mouse;
mod typing;

use element::TerminalElement;

use std::ops::Range;
use std::time::{Duration, Instant};

use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{RenderableCursor, TermMode};
use alacritty_terminal::vte::ansi::CursorShape;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    KeyDownEvent, LayoutId, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ContentMask, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, ShapedLine, StrikethroughStyle, Style,
    TextRun, UTF16Selection, UnderlineStyle, Window, div, fill, outline, point, prelude::*, px, relative, size,
};

use gpui_kit::component::input;

use crate::terminal::glyphs;
use crate::terminal::keys;
use crate::terminal::skeleton::{self, Draft, Step};
use crate::terminal::{SpawnSpec, Terminal};
use crate::panels::theme;

pub enum TerminalEvent {
    Exited(Option<i32>),
    Bell,
}

#[derive(Clone, Copy)]
struct Metrics {
    cell_w: Pixels,
    line_h: Pixels,
    origin: Point<Pixels>,
    /// How far the rows are drawn below their cells while scrolling between two lines.
    shift: Pixels,
}

pub struct TerminalView {
    pub terminal: Terminal,
    focus: FocusHandle,
    metrics: Option<Metrics>,
    selecting: bool,
    scroll_px: f32,
    /// Smooth scrollback: the view stands `frac` of a line above the line alacritty shows (0 at rest), and a wheel
    /// notch eases there (`panels/glide.rs`). Sub-line positions are drawn by shifting the rows.
    frac: f32,
    glide: Option<crate::panels::glide::Glide>,
    pub title: Option<String>,
    /// Output timing, for `is_working`: when output last arrived and when the current burst began.
    last_output: Option<Instant>,
    burst_start: Option<Instant>,
    /// The start-up trace has begun for this terminal (`trace.rs`).
    traced: bool,
    /// What the user types while Claude Code starts; moved into the real prompt when it appears (`skeleton.rs`).
    draft: Draft,
    /// The draft is being typed into Claude Code; the skeleton stays until the real prompt shows it.
    handing: bool,
    pub exit_code: Option<Option<i32>>,
    /// IME composition / dead-key text not yet committed; drawn at the cursor, never sent to the program.
    marked: Option<String>,
    font_family: &'static str,
    font_size: Pixels,
    /// Show a startup animation until the program first draws something (set for Claude sessions).
    pub loading: bool,
    /// The account the session is handing over to: the skeleton says so until the new process is ready.
    pub handover: Option<String>,
    /// Whether a pane currently shows this terminal (kept up to date by the workspace's render).
    pub visible: bool,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A row's height at the terminal's 14 px font (`font_size * 1.35`, rounded); the skeleton prompt lines up with it.
pub const LINE_H: f32 = 19.0;

#[cfg(target_os = "windows")]
pub const FONT: &str = "Cascadia Mono";
#[cfg(not(target_os = "windows"))]
pub const FONT: &str = "Menlo";

/// What to look for on Claude Code's screen to know a piece of the draft arrived.
enum Marker {
    Text(String),
    Image,
}

/// Images Claude Code takes.
fn is_image_file(p: &std::path::Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["png", "jpg", "jpeg", "gif", "webp", "bmp"].contains(&e.to_ascii_lowercase().as_str()))
}

/// Keep a pasted clipboard image as a file under the data folder (`pasted/`, pruned after a week).
fn save_pasted(image: &gpui::Image) -> Option<std::path::PathBuf> {
    use gpui::ImageFormat::*;
    let ext = match image.format {
        Png => "png",
        Jpeg => "jpg",
        Webp => "webp",
        Gif => "gif",
        Bmp => "bmp",
        _ => return None,
    };
    let dir = crate::datadir::data_dir().join("pasted");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{:x}.{ext}", image.id));
    std::fs::write(&path, &image.bytes).ok()?;
    Some(path)
}

impl TerminalView {
    /// Start the process in the background, so a slow start never freezes the window. A failure to start
    /// shows up as an exit with `start_error` set.
    pub fn spawn(spec: &SpawnSpec) -> (Terminal, UnboundedReceiver<TermEvent>) {
        Terminal::spawn_deferred(spec, 120, 32)
    }

    pub fn start_error(&self) -> Option<String> {
        self.terminal.start_error()
    }

    pub fn new(
        terminal: Terminal,
        rx: UnboundedReceiver<TermEvent>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.spawn(async move |this, cx| pump(this, rx, cx).await)
            .detach();
        Self {
            terminal,
            focus: cx.focus_handle(),
            metrics: None,
            selecting: false,
            scroll_px: 0.0,
            frac: 0.0,
            glide: None,
            title: None,
            last_output: None,
            burst_start: None,
            traced: false,
            draft: Draft::default(),
            handing: false,
            exit_code: None,
            marked: None,
            font_family: FONT,
            font_size: px(14.0),
            loading: false,
            handover: None,
            visible: true,
        }
    }

    /// Trace when the start-up burst of output ends (a second without any): about when Claude Code is ready.
    fn trace_quiet(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let Ok(quiet) = this.update(cx, |v, _| {
                    let last = v.last_output?;
                    (last.elapsed() > Duration::from_secs(1)).then(|| (v.terminal.tag.clone(), last.duration_since(v.terminal.born)))
                }) else {
                    break;
                };
                if let Some((tag, at)) = quiet {
                    crate::terminal::trace::note(&tag, at, "last output of the start-up burst");
                    break;
                }
            }
        })
        .detach();
    }

    pub fn is_running(&self) -> bool {
        self.exit_code.is_none()
    }

    /// Claude Code redraws its spinner continuously while it works and is silent while it waits for you.
    /// So: output that has kept arriving for a second or more and is still fresh. A single echoed keystroke
    /// or redraw is a short burst and doesn't count. (Heuristic: no Claude Code hook or config involved.)
    pub fn is_working(&self) -> bool {
        const GAP: Duration = Duration::from_millis(1500);
        match (self.last_output, self.burst_start) {
            (Some(last), Some(start)) => self.is_running() && last.elapsed() < GAP && last - start >= Duration::from_secs(1),
            _ => false,
        }
    }

    fn handle_event(&mut self, event: TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::Wakeup => {
                let t = Instant::now();
                if self.last_output.is_none_or(|l| t - l > Duration::from_millis(1500)) {
                    self.burst_start = Some(t);
                }
                self.last_output = Some(t);
                if crate::terminal::trace::on() && !self.traced {
                    self.traced = true;
                    crate::terminal::trace::note(&self.terminal.tag, self.terminal.born.elapsed(), "first output");
                    self.trace_quiet(cx);
                }
                // Off-screen terminals only record the timing; a notify would redraw the whole window.
                if self.visible {
                    cx.notify()
                }
            }
            TermEvent::PtyWrite(text) => self.terminal.write(text.into_bytes()),
            TermEvent::Title(title) => self.title = Some(title),
            TermEvent::ResetTitle => self.title = None,
            TermEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            TermEvent::ColorRequest(index, format) => {
                let rgb =
                    theme::palette(index, self.terminal.term.lock().renderable_content().colors);
                self.terminal.write(format(rgb).into_bytes());
            }
            TermEvent::TextAreaSizeRequest(format) => {
                let (w, h) = self
                    .metrics
                    .map_or((8.0, 16.0), |m| (f32::from(m.cell_w), f32::from(m.line_h)));
                self.terminal
                    .write(format(self.terminal.window_size(w, h)).into_bytes());
            }
            TermEvent::Bell => cx.emit(TerminalEvent::Bell),
            TermEvent::ChildExit(status) => {
                let code = status.code();
                self.exit_code = Some(code);
                cx.emit(TerminalEvent::Exited(code));
                cx.notify();
            }
            TermEvent::Exit => {
                if self.exit_code.is_none() {
                    self.exit_code = Some(None);
                    cx.emit(TerminalEvent::Exited(None));
                    cx.notify();
                }
            }
            TermEvent::MouseCursorDirty
            | TermEvent::CursorBlinkingChange
            | TermEvent::ClipboardLoad(..) => {}
        }
    }

    pub fn mode(&self) -> TermMode {
        *self.terminal.term.lock().mode()
    }

    /// How often the markers show on the screen: the text just typed (or "[Pasted text" when it is long), or "[Image #".
    fn marks(&self, marker: &Marker) -> usize {
        let screen = self.tail_text(60);
        match marker {
            Marker::Text(t) => screen.matches(t.as_str()).count() + screen.matches("[Pasted text").count(),
            Marker::Image => screen.matches("[Image #").count(),
        }
    }

    /// Wait until `marker` shows more often than `before`, at most `patience`. A count, not a lookup, because a resumed
    /// session may already show such text, and Claude Code's image numbers may not start at 1.
    async fn wait_for(this: &gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp, marker: &Marker, before: usize, patience: Duration) {
        let start = Instant::now();
        while start.elapsed() < patience {
            match this.update(cx, |v, _| v.marks(marker) > before) {
                Ok(false) => cx.background_executor().timer(Duration::from_millis(50)).await,
                _ => return,
            }
        }
    }

    /// Claude Code drew something: type the draft into it piece by piece, waiting after each for it to show on the
    /// real screen (its first screen can arrive well before it reads the keyboard, and each image is read from the
    /// clipboard when its key is handled). The skeleton stays up meanwhile, and keeps taking what is typed; then
    /// `finish_loading`.
    fn hand_over(&mut self, cx: &mut Context<Self>) {
        self.handing = true;
        cx.spawn(async move |this, cx| {
            // Images borrow the clipboard; the user's own comes back at the end.
            let mut clipboard = None;
            // The first piece also waits for Claude Code to start reading the keyboard.
            let mut patience = Duration::from_secs(10);
            loop {
                let Ok(steps) = this.update(cx, |v, _| v.draft.next_steps()) else { return };
                if steps.is_empty() {
                    break;
                }
                for step in steps {
                    let wait = this
                        .update(cx, |v, cx| match &step {
                            Step::Text(t) => {
                                let last = t.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
                                let marker = Marker::Text(last.chars().take(20).collect());
                                let before = v.marks(&marker);
                                v.type_in(t, cx);
                                (!last.is_empty()).then_some((marker, before))
                            }
                            Step::Erase(n) => {
                                v.send(vec![0x7f; *n], cx);
                                None
                            }
                            Step::Image(path, _) => {
                                clipboard.get_or_insert_with(|| cx.read_from_clipboard());
                                let before = v.marks(&Marker::Image);
                                v.paste_image_file(path, cx).then_some((Marker::Image, before))
                            }
                        })
                        .ok()
                        .flatten();
                    if let Some((marker, before)) = wait {
                        Self::wait_for(&this, cx, &marker, before, patience).await;
                    }
                    patience = Duration::from_secs(3);
                }
            }
            this.update(cx, |v, cx| {
                if let Some(Some(item)) = clipboard {
                    cx.write_to_clipboard(item);
                }
                v.finish_loading(cx)
            })
            .ok();
        })
        .detach();
    }

    /// The real prompt is up: drop the skeleton, type what was added meanwhile, and press Enter if the user did.
    fn finish_loading(&mut self, cx: &mut Context<Self>) {
        self.loading = false;
        self.handover = None;
        self.handing = false;
        crate::terminal::trace::note(&self.terminal.tag, self.terminal.born.elapsed(), "prompt ready");
        let erase = self.draft.take_erase();
        if erase > 0 {
            self.send(vec![0x7f; erase], cx);
        }
        let (rest, sent) = self.draft.take();
        self.type_in(&rest, cx);
        if sent {
            // Enter right behind a paste can be read as part of it.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                this.update(cx, |v, cx| v.send(b"\r".to_vec(), cx)).ok();
            })
            .detach();
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.glide_step(window);
        if self.loading && self.exit_code.is_some() {
            self.loading = false;
            self.handover = None;
        } else if self.loading && !self.handing && self.has_content() {
            crate::terminal::trace::note(&self.terminal.tag, self.terminal.born.elapsed(), "first content painted");
            if self.draft.is_empty() {
                self.finish_loading(cx);
            } else {
                self.hand_over(cx);
            }
        }
        let phase = match (&self.handover, self.handing, self.last_output) {
            (Some(to), _, _) => format!("Handing over to {to}…"),
            (None, true, _) => "Handing your message to Claude Code…".into(),
            (None, false, Some(_)) => "Loading skills, plugins and MCP servers…".into(),
            (None, false, None) => "Starting Claude Code…".into(),
        };
        div()
            .id("terminal")
            .relative()
            .size_full()
            .bg(theme::hsla(theme::TERMINAL_BG))
            .key_context("Terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            // The Edit menu dispatches the text input's actions; a focused terminal answers them too.
            .on_action(cx.listener(|this, _: &input::Copy, _, cx| {
                this.copy(cx);
            }))
            .on_action(cx.listener(|this, _: &input::Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &input::SelectAll, _, cx| this.select_all(cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(TerminalElement { view: cx.entity() })
            .when(self.loading || self.handover.is_some(), |d| {
                let (cols, rows) = self.terminal.size();
                let (cell_w, line_h) = self.metrics.map_or((8.0, LINE_H), |m| (f32::from(m.cell_w), f32::from(m.line_h)));
                let grid = skeleton::Grid { cell_w, line_h, cols, rows };
                d.child(skeleton::overlay(&self.draft, self.terminal.born.elapsed(), &phase, grid))
            })
    }
}

impl TerminalView {
    /// Input goes to the skeleton's draft, not the PTY: Claude Code is starting, or the session is handing over.
    fn held(&self) -> bool {
        self.loading || self.handover.is_some()
    }

    /// Anything visible on screen yet. Startup mode/escape sequences wake us up but draw nothing.
    fn has_content(&self) -> bool {
        self.terminal.term.lock().renderable_content().display_iter.any(|c| !matches!(c.c, ' ' | '\0'))
    }
}

/// Drains terminal events, coalescing bursts so we redraw at most ~60 times a second.
async fn pump(
    this: gpui::WeakEntity<TerminalView>,
    mut rx: UnboundedReceiver<TermEvent>,
    cx: &mut gpui::AsyncApp,
) {
    while let Some(first) = rx.next().await {
        let mut batch = vec![first];
        while let Ok(ev) = rx.try_recv() {
            batch.push(ev);
        }
        let alive = this
            .update(cx, |view, cx| {
                for ev in batch {
                    view.handle_event(ev, cx);
                }
            })
            .is_ok();
        if !alive {
            break;
        }
        cx.background_executor()
            .timer(Duration::from_millis(16))
            .await;
    }
}
