//! A terminal's state and its Rune view: paints the grid, turns Rune input into VT input.
//! Children: `typing` (keys, paste, IME), `mouse` (selection, reporting, scrolling), `element` (the grid as Rune
//! elements), `cards` (the model's cards over the transcript), `paths` (file paths in the text).
//!
//! A view is owned by the workspace and found again by its `key` (the session id, or `shell:<id>` for a shell). What
//! the workspace has to act on (the process ended, a card's button, Ctrl+click on a path) waits in `events`; what needs
//! the frame's `Tx` (the clipboard) waits in `fx`. The workspace drains both after every handler and frame.

mod cards;
mod element;
mod mouse;
mod paths;
mod typing;

use cards::INLINE_LINK;

use std::time::{Duration, Instant};

use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use rune::Tx;

use crate::terminal::EventReceiver;
use crate::terminal::glyphs;
use crate::terminal::keys;
use crate::terminal::skeleton::{self, Draft, Step};
use crate::terminal::{SpawnSpec, Terminal};
use crate::panels::theme;

pub enum TerminalEvent {
    Exited(Option<i32>),
    Bell,
    /// Ctrl+click on a file path in the text, or on a `file:` link: the path as written (maybe relative to the
    /// session's folder), and its line (1-based) when one was given.
    OpenPath(String, Option<usize>),
    /// A card's button: text to send as the user's next message.
    SendText(String),
}

/// The size of one cell, and where the view stands between two lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub cell_w: f32,
    pub line_h: f32,
}

/// The cell size of the terminal's font at `font_size` points: the advance of `m`, measured over many so the
/// rounding of a single width doesn't drift across a long row.
pub fn metrics_for(font_size: f32) -> Metrics {
    thread_local! {
        static CACHE: std::cell::RefCell<Vec<(u32, Metrics)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    CACHE.with(|cache| {
        let bits = font_size.to_bits();
        if let Some((_, m)) = cache.borrow().iter().find(|(b, _)| *b == bits) {
            return *m;
        }
        let style = element::font(font_size, false, false);
        let (w, _) = rune::text::measure(&"m".repeat(100), &[], style, None);
        let cell_w = if w > 0.0 { w / 100.0 } else { font_size * 0.6 };
        let m = Metrics { cell_w, line_h: (font_size * 1.35).round() };
        cache.borrow_mut().push((bits, m));
        m
    })
}

/// Where a view was last drawn: its grid and cell size.
#[derive(Clone, Copy)]
pub struct Layout {
    cols: u16,
    rows: u16,
    metrics: Metrics,
}

/// A session being moved while its process restarts: what the skeleton says, and when the move began (an account
/// hand-off compacts the old process first, so it began earlier than the restart).
pub struct Handover {
    pub message: String,
    pub since: Instant,
}

/// What Ctrl+click opens.
enum Link {
    /// A file path and the line it names.
    File(String, Option<usize>),
    Url(String),
}

/// What to look for on Claude Code's screen to know a piece of the draft arrived.
enum Marker {
    Text(String),
    Image,
}

/// The draft typed while Claude Code started, going into it piece by piece (`hand_over`).
struct Handing {
    steps: std::collections::VecDeque<Step>,
    /// The piece last typed shows this marker more often than `before` once it arrived; given up after `until`.
    waiting: Option<(Marker, usize, Instant)>,
    /// How long the next piece may take to show: the first also waits for Claude Code to read the keyboard.
    patience: Duration,
    /// The user's clipboard, which images borrow, to put back at the end (asked for once, before the first image).
    clipboard: Option<Option<rune::ClipboardContent>>,
}

/// The `@path#L..` line `insert_reference` put into Claude's input.
struct Inserted {
    chars: usize,
    /// The user typed or pasted after it, so it can no longer be erased without erasing their text too.
    typed: bool,
}

/// Images Claude Code takes.
fn is_image_file(p: &std::path::Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["png", "jpg", "jpeg", "gif", "webp", "bmp"].contains(&e.to_ascii_lowercase().as_str()))
}

/// Keep a pasted clipboard image (PNG) as a file under the data folder (`pasted/`, pruned after a week).
fn save_pasted(png: &[u8]) -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    png.hash(&mut h);
    let dir = crate::datadir::data_dir().join("pasted");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{:x}.png", h.finish()));
    std::fs::write(&path, png).ok()?;
    Some(path)
}

pub struct TerminalView {
    pub terminal: Terminal,
    rx: EventReceiver,
    /// Finds this view again from its handlers: the session id, or `shell:<id>`.
    pub key: String,
    /// The size of the area it was last laid out in, in points.
    area: Option<[f32; 2]>,
    selecting: bool,
    /// The button held down since a press, which the pointer-move handler sees as plain hovering too.
    pub(super) pressed: Option<mouse::Button>,
    scroll_px: f32,
    /// Smooth scrollback: the view stands `frac` of a line above the line alacritty shows (0 at rest), and a wheel
    /// notch eases there (`panels/glide.rs`). Sub-line positions are drawn by shifting the rows.
    frac: f32,
    glide: Option<crate::panels::glide::Glide>,
    /// A notch's steps for a program that scrolls itself (Claude Code's fullscreen view), see `wheel_step`.
    wheel: mouse::WheelSteps,
    pub title: Option<String>,
    /// Output timing, for `is_working`: when output last arrived and when the current burst began.
    last_output: Option<Instant>,
    burst_start: Option<Instant>,
    /// The start-up trace has begun for this terminal (`trace.rs`), and its quiet end has been noted.
    traced: bool,
    trace_quiet: bool,
    /// What the user types while Claude Code starts; moved into the real prompt when it appears (`skeleton.rs`).
    draft: Draft,
    /// The draft is being typed into Claude Code; the skeleton stays until the real prompt shows it.
    handing: Option<Handing>,
    /// Enter, pressed while starting, goes out this long after the draft (right behind a paste it can read as part of it).
    enter_at: Option<Instant>,
    pub exit_code: Option<Option<i32>>,
    /// IME composition / dead-key text not yet committed; drawn at the cursor, never sent to the program.
    marked: Option<String>,
    font_size: f32,
    /// Show a startup animation until the program first draws something (set for Claude sessions).
    pub loading: bool,
    /// The skeleton says what the session is being moved to until the new process is ready.
    pub handover: Option<Handover>,
    /// Whether a pane currently shows this terminal (kept up to date by the workspace's view).
    pub visible: bool,
    /// Inactive windows still show output, but do not need the foreground frame rate.
    pub foreground: bool,
    last_redraw: Option<Instant>,
    /// Output the screen doesn't show yet, held back in an inactive window, is drawn at this time.
    redraw_at: Option<Instant>,
    /// While Ctrl is held over a link: its viewport row and the columns to underline (none when it is drawn as a link already).
    hovered_link: Option<(usize, Option<std::ops::Range<usize>>)>,
    /// A line Claudiu put into Claude's input for the user (`insert_reference`), until it is kept or taken back.
    reference: Option<Inserted>,
    cards: cards::Cards,
    /// For the workspace to act on (see the module docs).
    pub events: Vec<TerminalEvent>,
    pub fx: Vec<crate::app::rt::Fx>,
    /// The screen changed and the view must be built again.
    pub dirty: bool,
    /// The skeleton's last drawn 120 ms step (its spinner glyph and seconds), while it is up.
    skeleton_step: Option<u128>,
}

impl TerminalView {
    /// Start the process in the background, so a slow start never freezes the window. A failure to start
    /// shows up as an exit with `start_error` set.
    pub fn spawn(spec: &SpawnSpec) -> (Terminal, EventReceiver) {
        Terminal::spawn_deferred(spec, 120, 32)
    }

    pub fn start_error(&self) -> Option<String> {
        self.terminal.start_error()
    }

    fn metrics(&self) -> Metrics {
        metrics_for(crate::panels::scale::px(self.font_size))
    }

    /// `None` until the view has been laid out.
    pub fn layout(&self) -> Option<Layout> {
        let (cols, rows) = self.terminal.size();
        self.area.map(|_| Layout { cols, rows, metrics: self.metrics() })
    }

    /// Start on the grid of a view in the same pane. A view's first frame is drawn before it is laid out, so a new one
    /// would otherwise draw that frame, and the skeleton's prompt with it, on the default 120×32 grid.
    pub fn take_layout(&mut self, layout: Layout) {
        let Layout { cols, rows, metrics } = layout;
        self.terminal.resize(cols, rows, metrics.cell_w, metrics.line_h);
        self.area = Some([metrics.cell_w * f32::from(cols), metrics.line_h * f32::from(rows)]);
    }

    /// The area the view was laid out in: the grid fills as many whole cells as fit.
    pub fn set_area(&mut self, w: f32, h: f32) {
        let m = self.metrics();
        let cols = ((w / m.cell_w).floor() as u16).max(2);
        let rows = ((h / m.line_h).floor() as u16).max(1);
        self.area = Some([w, h]);
        if self.terminal.size() != (cols, rows) {
            self.terminal.resize(cols, rows, m.cell_w, m.line_h);
            self.dirty = true;
        }
    }

    pub fn new(terminal: Terminal, rx: EventReceiver, key: impl Into<String>) -> Self {
        Self {
            terminal,
            rx,
            key: key.into(),
            area: None,
            selecting: false,
            pressed: None,
            scroll_px: 0.0,
            frac: 0.0,
            glide: None,
            wheel: Default::default(),
            title: None,
            last_output: None,
            burst_start: None,
            traced: false,
            trace_quiet: false,
            draft: Draft::default(),
            handing: None,
            enter_at: None,
            exit_code: None,
            marked: None,
            font_size: 14.0,
            loading: false,
            handover: None,
            visible: true,
            foreground: true,
            last_redraw: None,
            redraw_at: None,
            hovered_link: None,
            reference: None,
            cards: cards::Cards::default(),
            events: vec![],
            fx: vec![],
            dirty: true,
            skeleton_step: None,
        }
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

    /// Read what the process sent since the last frame, and run what is due (held-back output, the hand-over of the
    /// draft, a smooth scroll). Returns when it next needs a frame, if it does.
    pub fn pump(&mut self) -> Option<Instant> {
        while let Ok(ev) = self.rx.try_recv() {
            self.handle_event(ev);
        }
        let now = Instant::now();
        if self.redraw_at.is_some_and(|at| at <= now) {
            self.redraw_at = None;
            if self.visible {
                self.last_redraw = Some(now);
                self.dirty = true;
            }
        }
        if self.traced && !self.trace_quiet && self.last_output.is_some_and(|l| l.elapsed() > Duration::from_secs(1)) {
            self.trace_quiet = true;
            let at = self.last_output.map_or(Duration::ZERO, |l| l.duration_since(self.terminal.born));
            crate::terminal::trace::note(&self.terminal.tag, at, "last output of the start-up burst");
        }
        self.check_loading();
        self.hand_over_step();
        if self.enter_at.is_some_and(|at| at <= now) {
            self.enter_at = None;
            self.send(b"\r".to_vec());
        }
        let mut next = [self.redraw_at, self.enter_at].into_iter().flatten().min();
        let frame = now + Duration::from_millis(16);
        if self.glide_step() || self.wheel_step() {
            next = Some(next.map_or(frame, |n| n.min(frame)));
        }
        if self.handing.is_some() {
            next = Some(next.map_or(now + Duration::from_millis(50), |n| n.min(now + Duration::from_millis(50))));
        }
        if self.held() {
            // The skeleton's spinner and its seconds change every 120 ms, so it is drawn again only then.
            next = Some(next.map_or(now + Duration::from_millis(120), |n| n.min(now + Duration::from_millis(120))));
            let since = self.handover.as_ref().map_or(self.terminal.born, |h| h.since);
            let step = Some(since.elapsed().as_millis() / 120);
            if self.skeleton_step != step {
                self.skeleton_step = step;
                self.dirty = true;
            }
        }
        next
    }

    fn handle_event(&mut self, event: TermEvent) {
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
                }
                // Off-screen terminals only record the timing; a redraw would rebuild the whole window.
                if redraw_due(self.visible, self.foreground, self.last_redraw, t) {
                    self.redraw_at = None;
                    self.last_redraw = Some(t);
                    self.dirty = true;
                } else if self.visible && self.redraw_at.is_none() {
                    // Flush the final output even if the process falls silent before the next frame.
                    self.redraw_at = Some(self.last_redraw.unwrap_or(t) + Duration::from_millis(250));
                }
            }
            TermEvent::PtyWrite(text) => self.terminal.write(text.into_bytes()),
            TermEvent::Title(title) => {
                self.title = Some(title);
                self.dirty = true;
            }
            TermEvent::ResetTitle => {
                self.title = None;
                self.dirty = true;
            }
            TermEvent::ClipboardStore(_, text) => self.fx.push(Box::new(move |tx: &mut Tx| tx.copy(text))),
            TermEvent::ColorRequest(index, format) => {
                let rgb = theme::palette(index, self.terminal.term.lock().renderable_content().colors);
                self.terminal.write(format(rgb).into_bytes());
            }
            TermEvent::TextAreaSizeRequest(format) => {
                let m = self.metrics();
                self.terminal.write(format(self.terminal.window_size(m.cell_w, m.line_h)).into_bytes());
            }
            TermEvent::Bell => self.events.push(TerminalEvent::Bell),
            TermEvent::ChildExit(status) => {
                let code = status.code();
                self.exit_code = Some(code);
                self.events.push(TerminalEvent::Exited(code));
                self.dirty = true;
            }
            TermEvent::Exit => {
                if self.exit_code.is_none() {
                    self.exit_code = Some(None);
                    self.events.push(TerminalEvent::Exited(None));
                    self.dirty = true;
                }
            }
            TermEvent::MouseCursorDirty | TermEvent::CursorBlinkingChange | TermEvent::ClipboardLoad(..) => {}
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

    /// The skeleton is up and Claude Code drew something: hand the draft over, or drop the skeleton when there is none.
    fn check_loading(&mut self) {
        if self.loading && self.exit_code.is_some() {
            self.loading = false;
            self.handover = None;
            self.handing = None;
            self.dirty = true;
        } else if self.loading && self.handing.is_none() && self.has_content() {
            crate::terminal::trace::note(&self.terminal.tag, self.terminal.born.elapsed(), "first content painted");
            if self.draft.is_empty() {
                self.finish_loading();
            } else {
                self.handing = Some(Handing {
                    steps: Default::default(),
                    waiting: None,
                    patience: Duration::from_secs(10),
                    clipboard: None,
                });
            }
        }
    }

    /// Claude Code drew something: type the draft into it piece by piece, waiting after each for it to show on the
    /// real screen (its first screen can arrive well before it reads the keyboard, and each image is read from the
    /// clipboard when its key is handled). The skeleton stays up meanwhile, and keeps taking what is typed; then
    /// `finish_loading`. Runs a step at a time from `pump`.
    fn hand_over_step(&mut self) {
        let Some(mut h) = self.handing.take() else { return };
        if let Some((marker, before, until)) = &h.waiting {
            if self.marks(marker) <= *before && Instant::now() < *until {
                self.handing = Some(h);
                return;
            }
            h.waiting = None;
        }
        if h.steps.is_empty() {
            h.steps = self.draft.next_steps().into();
        }
        let Some(step) = h.steps.pop_front() else {
            // Everything arrived: the user's own clipboard comes back.
            if let Some(Some(content)) = h.clipboard.take() {
                self.fx.push(Box::new(move |tx: &mut Tx| tx.copy_rich(content)));
            }
            self.finish_loading();
            return;
        };
        let patience = h.patience;
        h.patience = Duration::from_secs(3);
        match step {
            Step::Text(t) => {
                let last = t.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_string();
                let marker = Marker::Text(last.chars().take(20).collect());
                let before = self.marks(&marker);
                self.type_in(&t);
                if !last.is_empty() {
                    h.waiting = Some((marker, before, Instant::now() + patience));
                }
            }
            Step::Erase(n) => self.send(vec![0x7f; n]),
            Step::Image(path, _) => {
                if h.clipboard.is_none() {
                    // Keep the user's clipboard to put back; the image goes on it first, so it waits for the read.
                    h.clipboard = Some(None);
                    let key = self.key.clone();
                    self.fx.push(Box::new(move |tx: &mut Tx| {
                        tx.read_clipboard(move |ws: &mut crate::app::Workspace, content, _| {
                            if let Some(h) = ws.terminal_mut(&key).and_then(|t| t.handing.as_mut()) {
                                h.clipboard = Some(Some(content.clone()));
                            }
                        })
                    }));
                }
                let before = self.marks(&Marker::Image);
                if self.paste_image_file(&path) {
                    h.waiting = Some((Marker::Image, before, Instant::now() + patience));
                }
            }
        }
        self.handing = Some(h);
    }

    /// The real prompt is up: drop the skeleton, type what was added meanwhile, and press Enter if the user did.
    fn finish_loading(&mut self) {
        self.loading = false;
        self.handover = None;
        self.handing = None;
        self.dirty = true;
        crate::terminal::trace::note(&self.terminal.tag, self.terminal.born.elapsed(), "prompt ready");
        let erase = self.draft.take_erase();
        if erase > 0 {
            self.send(vec![0x7f; erase]);
        }
        let (rest, sent) = self.draft.take();
        self.type_in(&rest);
        if sent {
            // Enter right behind a paste can be read as part of it.
            self.enter_at = Some(Instant::now() + Duration::from_millis(150));
        }
    }

    /// The skeleton is up: Claude Code is starting, or the session is being moved.
    pub fn is_loading(&self) -> bool {
        self.held()
    }

    /// Input goes to the skeleton's draft, not the PTY: Claude Code is starting, or the session is handing over.
    fn held(&self) -> bool {
        self.loading || self.handover.is_some()
    }

    /// Anything visible on screen yet. Startup mode/escape sequences wake us up but draw nothing.
    fn has_content(&self) -> bool {
        self.terminal.term.lock().renderable_content().display_iter.any(|c| !matches!(c.c, ' ' | '\0'))
    }

    /// The skeleton's line under the spinner.
    fn phase(&self) -> String {
        match (&self.handover, self.handing.is_some(), self.last_output) {
            (Some(h), _, _) => h.message.clone(),
            (None, true, _) => "Handing your message to Claude Code…".into(),
            (None, false, Some(_)) => "Loading skills, plugins and MCP servers…".into(),
            (None, false, None) => "Starting Claude Code…".into(),
        }
    }
}

fn redraw_due(visible: bool, foreground: bool, last: Option<Instant>, now: Instant) -> bool {
    visible && (foreground || last.is_none_or(|last| now.duration_since(last) >= Duration::from_millis(250)))
}

#[cfg(test)]
mod redraw_tests {
    use super::*;

    fn view() -> (TerminalView, crate::terminal::Listener) {
        let (terminal, rx, listener, _) = Terminal::build(&SpawnSpec::default(), 80, 24);
        (TerminalView::new(terminal, rx, "t"), listener)
    }

    fn wake(listener: &crate::terminal::Listener) {
        alacritty_terminal::event::EventListener::send_event(listener, TermEvent::Wakeup);
    }

    #[test]
    fn a_quiet_background_terminal_flushes_its_last_output() {
        let (mut v, listener) = view();
        v.dirty = false;
        v.foreground = false;
        wake(&listener);
        v.pump();
        assert!(std::mem::take(&mut v.dirty), "the first output in a while is drawn at once");
        wake(&listener);
        let next = v.pump();
        assert!(!v.dirty, "more output within a quarter second waits");
        let next = next.expect("a frame is asked for to draw it");
        std::thread::sleep(next.saturating_duration_since(Instant::now()) + Duration::from_millis(5));
        v.pump();
        assert!(std::mem::take(&mut v.dirty), "the last output must be drawn without another PTY event");
        v.visible = false;
        wake(&listener);
        std::thread::sleep(Duration::from_millis(300));
        v.pump();
        assert!(!v.dirty, "hidden terminals do not redraw");
    }

    #[test]
    fn inactive_output_is_limited_and_foreground_output_stays_responsive() {
        let start = Instant::now();
        for (visible, foreground, expected) in [(true, true, 1000), (true, false, 4), (false, true, 0), (false, false, 0)] {
            let mut last = None;
            let mut redraws = 0;
            for ms in 0..1000 {
                let now = start + Duration::from_millis(ms);
                if redraw_due(visible, foreground, last, now) {
                    last = Some(now);
                    redraws += 1;
                }
            }
            assert_eq!(redraws, expected);
        }
        assert!(redraw_due(true, true, Some(start), start), "activation and typing do not wait for the background limit");
    }
}
