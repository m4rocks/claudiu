//! GPUI view over a `Terminal`: paints the grid, turns GPUI input into VT input.

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

use crate::glyphs;
use crate::keys;
use crate::skeleton::{self, Draft, Step};
use crate::terminal::{SpawnSpec, Terminal};
use crate::theme;

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
    /// notch eases there (`glide.rs`). Sub-line positions are drawn by shifting the rows.
    frac: f32,
    glide: Option<crate::glide::Glide>,
    pub title: Option<String>,
    /// Output timing, for `is_working`: when output last arrived and when the current burst began.
    last_output: Option<Instant>,
    burst_start: Option<Instant>,
    /// The start-up trace has begun for this terminal (`trace.rs`).
    traced: bool,
    /// The previous screen of a session being resumed, shown dimmed behind the start-up spinner.
    pub preview: Option<String>,
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
            preview: None,
            draft: Draft::default(),
            handing: false,
            exit_code: None,
            marked: None,
            font_family: FONT,
            font_size: px(14.0),
            loading: false,
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
                    crate::trace::note(&tag, at, "last output of the start-up burst");
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
                if crate::trace::on() && !self.traced {
                    self.traced = true;
                    crate::trace::note(&self.terminal.tag, self.terminal.born.elapsed(), "first output");
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

    fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        // Claude Code is not listening yet: keys edit the draft instead (`draft_key`).
        if !self.loading {
            self.write_input(bytes, cx);
        }
    }

    fn write_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let mut term = self.terminal.term.lock();
        term.selection = None;
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
        }
        drop(term);
        (self.frac, self.glide) = (0.0, None);
        self.terminal.write(bytes);
        cx.notify();
    }

    pub fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.loading {
            self.draft.insert(text);
            cx.notify();
            return;
        }
        let bytes = self.paste_bytes(text);
        self.send(bytes, cx);
    }

    fn paste_bytes(&self, text: &str) -> Vec<u8> {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            // Strip embedded end-marker so pasted text can't terminate the paste early.
            let clean = text.replace("\x1b[201~", "");
            format!("\x1b[200~{clean}\x1b[201~").into_bytes()
        } else {
            text.into_bytes()
        }
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text().filter(|t| !t.is_empty()) {
            self.paste_text(&text, cx);
            return;
        }
        // While Claude Code starts, images are kept as files and typed in as paths when it is ready (`skeleton.rs`).
        if self.loading {
            for entry in item.entries() {
                match entry {
                    gpui::ClipboardEntry::Image(image) => {
                        if let Some(path) = save_pasted(image) {
                            self.draft.add_image(path);
                        }
                    }
                    gpui::ClipboardEntry::ExternalPaths(p) => self.paste_paths(p.paths(), cx),
                    _ => {}
                }
            }
            cx.notify();
            return;
        }
        // An image on the clipboard: Claude Code reads it itself on its image-paste key (Alt+V on Windows,
        // Ctrl+V elsewhere).
        if item.entries().iter().any(|e| matches!(e, gpui::ClipboardEntry::Image(_))) {
            let key: &[u8] = if cfg!(target_os = "windows") { b"\x1bv" } else { b"\x16" };
            self.send(key.to_vec(), cx);
        } else if let Some(paths) = item.entries().iter().find_map(|e| match e {
            gpui::ClipboardEntry::ExternalPaths(p) => Some(p.paths().to_vec()),
            _ => None,
        }) {
            self.paste_paths(&paths, cx);
        }
    }

    /// Dropped or copied files: paste their paths (quoted when needed); Claude Code attaches them.
    pub fn paste_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        if self.loading {
            for p in paths {
                if is_image_file(p) {
                    self.draft.add_image(p.clone());
                } else {
                    self.draft.insert(&format!("{} ", skeleton::quote_path(p)));
                }
            }
            cx.notify();
            return;
        }
        let text = paths.iter().map(|p| skeleton::quote_path(p)).collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            self.paste_text(&format!("{text} "), cx);
        }
    }

    /// The last `max_lines` lines of the screen and scrollback as plain text, trailing blank lines dropped.
    pub fn tail_text(&self, max_lines: usize) -> String {
        let term = self.terminal.term.lock();
        let (top, bottom, last) = (term.topmost_line(), term.bottommost_line(), term.last_column());
        let start = Line((bottom.0 - max_lines.max(1) as i32 + 1).max(top.0));
        term.bounds_to_string(GridPoint::new(start, Column(0)), GridPoint::new(bottom, last)).trim_end().to_string()
    }

    /// Copies the selection; returns whether there was one.
    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        let mut term = self.terminal.term.lock();
        let text = term.selection_to_string().filter(|t| !t.is_empty());
        if text.is_some() {
            term.selection = None;
        }
        drop(term);
        match text {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.notify();
                true
            }
            None => false,
        }
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let mut term = self.terminal.term.lock();
        let (top, bottom, last) = (term.topmost_line(), term.bottommost_line(), term.last_column());
        let mut sel = Selection::new(SelectionType::Simple, GridPoint::new(top, Column(0)), Side::Left);
        sel.update(GridPoint::new(bottom, last), Side::Right);
        term.selection = Some(sel);
        drop(term);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        let key = ks.key.as_str();

        // Claudiu terminal shortcuts. Everything else goes to the program untouched.
        let copy_chord = (m.control && m.shift && key == "c")
            || (m.control && !m.shift && !m.alt && key == "insert");
        let paste_chord = (m.control && m.shift && key == "v")
            || (m.shift && !m.control && !m.alt && key == "insert");
        // Ctrl+V pastes on Windows/Linux (as in Windows Terminal); on macOS Cmd+C / Cmd+V are the copy/paste keys.
        let ctrl_v = !cfg!(target_os = "macos") && m.control && !m.shift && !m.alt && key == "v";
        let mac_copy = m.platform && !m.shift && !m.control && !m.alt && key == "c";
        let mac_paste = m.platform && !m.shift && !m.control && !m.alt && key == "v";
        // Every key handled here stops propagation, so the platform doesn't also deliver it as text.
        if mac_copy || copy_chord {
            self.copy(cx);
            cx.stop_propagation();
            return;
        }
        if mac_paste || paste_chord || ctrl_v {
            self.paste(cx);
            cx.stop_propagation();
            return;
        }
        // Ctrl+C with a selection copies (like Windows Terminal); otherwise it is SIGINT as usual.
        if m.control && !m.shift && !m.alt && key == "c" && self.copy(cx) {
            cx.stop_propagation();
            return;
        }
        if m.shift && !m.control && !m.alt {
            let scroll = match key {
                "pageup" => Some(Scroll::PageUp),
                "pagedown" => Some(Scroll::PageDown),
                "home" => Some(Scroll::Top),
                "end" => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                (self.frac, self.glide) = (0.0, None);
                self.terminal.term.lock().scroll_display(scroll);
                cx.notify();
                cx.stop_propagation();
                return;
            }
        }
        // Plain text arrives through `EntityInputHandler::replace_text_in_range` (IME, dead keys).
        if self.exit_code.is_some() || keys::is_text(ks) {
            return;
        }
        if self.loading {
            self.draft.key(ks);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if let Some(bytes) = keys::encode(ks, self.mode()) {
            self.send(bytes, cx);
            cx.stop_propagation();
        }
    }

    /// Type a message into Claude Code: a line as keystrokes, several as one paste so their breaks stay breaks.
    fn type_in(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        // A long line is better pasted than typed.
        let bytes = if text.contains('\n') || text.chars().count() > 200 { self.paste_bytes(text) } else { text.as_bytes().to_vec() };
        self.write_input(bytes, cx);
    }

    /// Put an image file on the clipboard and press Claude Code's image-paste key, which reads it from there.
    fn paste_image_file(&mut self, path: &std::path::Path, cx: &mut Context<Self>) -> bool {
        use gpui::ImageFormat::*;
        let format = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("png") => Png,
            Some("jpg" | "jpeg") => Jpeg,
            Some("gif") => Gif,
            Some("webp") => Webp,
            Some("bmp") => Bmp,
            _ => return false,
        };
        let Ok(bytes) = std::fs::read(path) else { return false };
        #[cfg(target_os = "windows")]
        if !crate::platform::set_clipboard_bitmap(&bytes) {
            return false;
        }
        #[cfg(not(target_os = "windows"))]
        cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::from_bytes(format, bytes)));
        #[cfg(target_os = "windows")]
        let _ = format;
        let key: &[u8] = if cfg!(target_os = "windows") { b"\x1bv" } else { b"\x16" };
        self.write_input(key.to_vec(), cx);
        true
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
        self.handing = false;
        crate::trace::note(&self.terminal.tag, self.terminal.born.elapsed(), "prompt ready");
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

    fn grid_point(&self, position: Point<Pixels>) -> Option<(GridPoint, Side, usize, usize)> {
        let m = self.metrics?;
        let (cols, rows) = self.terminal.size();
        let x = f32::from(position.x - m.origin.x).max(0.0);
        let y = f32::from(position.y - m.origin.y - m.shift).max(0.0);
        let col = ((x / f32::from(m.cell_w)) as usize).min(cols as usize - 1);
        let row = ((y / f32::from(m.line_h)) as usize).min(rows as usize - 1);
        let side = if x - col as f32 * f32::from(m.cell_w) < f32::from(m.cell_w) / 2.0 {
            Side::Left
        } else {
            Side::Right
        };
        let offset = self.terminal.term.lock().grid().display_offset() as i32;
        Some((
            GridPoint::new(Line(row as i32 - offset), Column(col)),
            side,
            col,
            row,
        ))
    }

    fn report_mouse(
        &mut self,
        button: u8,
        col: usize,
        row: usize,
        pressed: bool,
        mods: &Modifiers,
        motion: bool,
    ) {
        let mode = self.mode();
        let mut code = button;
        if mods.shift {
            code |= 4;
        }
        if mods.alt {
            code |= 8;
        }
        if mods.control {
            code |= 16;
        }
        if motion {
            code |= 32;
        }
        let bytes = if mode.contains(TermMode::SGR_MOUSE) {
            format!(
                "\x1b[<{};{};{}{}",
                code,
                col + 1,
                row + 1,
                if pressed { 'M' } else { 'm' }
            )
            .into_bytes()
        } else {
            if col >= 223 || row >= 223 {
                return;
            }
            let code = if pressed { code } else { 3 | (code & !3) };
            vec![
                0x1b,
                b'[',
                b'M',
                32 + code,
                32 + col as u8 + 1,
                32 + row as u8 + 1,
            ]
        };
        self.terminal.write(bytes);
    }

    fn mouse_reporting(&self, mods: &Modifiers) -> bool {
        self.mode().intersects(TermMode::MOUSE_MODE) && !mods.shift
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        let Some((point, side, col, row)) = self.grid_point(event.position) else {
            return;
        };

        if self.mouse_reporting(&event.modifiers) {
            let button = match event.button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                _ => return,
            };
            self.report_mouse(button, col, row, true, &event.modifiers, false);
            return;
        }

        match event.button {
            MouseButton::Left => {
                if event.modifiers.control {
                    let link = self.terminal.term.lock().grid()[point].hyperlink();
                    if let Some(link) = link {
                        cx.open_url(link.uri());
                        return;
                    }
                }
                let ty = match event.click_count {
                    1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                self.terminal.term.lock().selection = Some(Selection::new(ty, point, side));
                self.selecting = true;
                cx.notify();
            }
            // Windows Terminal convention: right click copies when there is a selection, else pastes.
            MouseButton::Right => {
                if !self.copy(cx) {
                    self.paste(cx);
                }
            }
            MouseButton::Middle => self.paste(cx),
            _ => {}
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_reporting(&event.modifiers) {
            if let Some((_, _, col, row)) = self.grid_point(event.position) {
                let button = match event.button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                self.report_mouse(button, col, row, false, &event.modifiers, false);
            }
            return;
        }
        if self.selecting {
            self.selecting = false;
            let mut term = self.terminal.term.lock();
            if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
                term.selection = None;
            }
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_reporting(&event.modifiers) {
            let mode = self.mode();
            let any_motion = mode.contains(TermMode::MOUSE_MOTION);
            let drag = mode.contains(TermMode::MOUSE_DRAG) && event.pressed_button.is_some();
            if (any_motion || drag)
                && let Some((_, _, col, row)) = self.grid_point(event.position) {
                    let button = match event.pressed_button {
                        Some(MouseButton::Left) => 0,
                        Some(MouseButton::Middle) => 1,
                        Some(MouseButton::Right) => 2,
                        _ => 3,
                    };
                    self.report_mouse(button, col, row, true, &event.modifiers, true);
                }
            return;
        }
        if self.selecting && event.dragging()
            && let Some((point, side, ..)) = self.grid_point(event.position) {
                if let Some(sel) = self.terminal.term.lock().selection.as_mut() {
                    sel.update(point, side);
                }
                cx.notify();
            }
    }

    /// Where the view is in the scrollback, in lines up from the bottom.
    fn position(&self) -> f32 {
        self.terminal.term.lock().grid().display_offset() as f32 + self.frac
    }

    /// Show the scrollback at `p` lines up from the bottom: whole lines are alacritty's offset, the rest is `frac`.
    fn set_position(&mut self, p: f32) {
        let mut term = self.terminal.term.lock();
        let (now, to) = (term.grid().display_offset() as i32, p.floor() as i32);
        if to != now {
            term.scroll_display(Scroll::Delta(to - now));
        }
        self.frac = if term.grid().display_offset() as i32 == to { p - to as f32 } else { 0.0 };
    }

    /// A wheel notch moves the target and the view eases there; a trackpad's pixel steps are applied as they come.
    fn scroll_back(&mut self, lines: f32, notch: bool) {
        let top = self.terminal.term.lock().history_size() as f32;
        let here = self.position();
        if notch {
            crate::glide::Glide::nudge(&mut self.glide, here, lines * crate::glide::NOTCH, 0.0, top);
        } else {
            self.glide = None;
            self.set_position((here + lines).clamp(0.0, top));
        }
    }

    /// One frame of an easing scroll.
    fn glide_step(&mut self, window: &mut Window) {
        let Some(g) = &mut self.glide else { return };
        let moving = g.step(0.01);
        let p = g.shown;
        if moving {
            window.request_animation_frame();
        } else {
            self.glide = None;
        }
        self.set_position(p);
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.metrics else { return };
        let notch = matches!(event.delta, ScrollDelta::Lines(_));
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / f32::from(m.line_h),
        };
        let mode = self.mode();
        let alt_scroll = mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL);
        // Plain scrollback is ours to scroll smoothly; a program that takes the wheel gets whole lines.
        if !self.mouse_reporting(&event.modifiers) && !alt_scroll {
            self.scroll_back(lines, notch);
            cx.notify();
            return;
        }
        self.scroll_px += lines;
        let whole = self.scroll_px.trunc() as i32;
        if whole == 0 {
            return;
        }
        self.scroll_px -= whole as f32;

        if self.mouse_reporting(&event.modifiers) {
            if let Some((_, _, col, row)) = self.grid_point(event.position) {
                let button = if whole > 0 { 64 } else { 65 };
                for _ in 0..whole.unsigned_abs() {
                    self.report_mouse(button, col, row, true, &event.modifiers, false);
                }
            }
        } else {
            let app = mode.contains(TermMode::APP_CURSOR);
            let key = match (whole > 0, app) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.terminal
                .write(key.repeat(whole.unsigned_abs() as usize).into_bytes());
        }
    }
}

/// Text input from the platform: typed characters, dead-key results and IME commits. A terminal has no editable
/// document, so ranges are ignored; only in-progress composition (`marked`) is tracked, to draw it at the cursor.
impl EntityInputHandler for TerminalView {
    fn text_for_range(&mut self, _: Range<usize>, _: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|t| 0..t.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        if self.loading {
            self.draft.insert(text);
            cx.notify();
        } else if !text.is_empty() && self.is_running() {
            self.send(text.as_bytes().to_vec(), cx);
        } else {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!text.is_empty()).then(|| text.to_string());
        cx.notify();
    }

    /// The cursor cell, so the IME candidate window opens next to it.
    fn bounds_for_range(&mut self, _: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let m = self.metrics?;
        let term = self.terminal.term.lock();
        let cursor = term.grid().cursor.point;
        let row = (cursor.line.0 + term.grid().display_offset() as i32).max(0);
        Some(Bounds::new(
            point(m.origin.x + m.cell_w * cursor.column.0 as f32, m.origin.y + m.shift + m.line_h * row as f32),
            size(m.cell_w, m.line_h),
        ))
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
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

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.glide_step(window);
        if self.loading && self.exit_code.is_some() {
            self.loading = false;
        } else if self.loading && !self.handing && self.has_content() {
            crate::trace::note(&self.terminal.tag, self.terminal.born.elapsed(), "first content painted");
            if self.draft.is_empty() {
                self.finish_loading(cx);
            } else {
                self.hand_over(cx);
            }
        }
        let phase = match (self.handing, self.last_output) {
            (true, _) => "Handing your message to Claude Code…",
            (false, Some(_)) => "Loading skills, plugins and MCP servers…",
            (false, None) => "Starting Claude Code…",
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
            .when(self.loading, |d| {
                let (cols, rows) = self.terminal.size();
                let (cell_w, line_h) = self.metrics.map_or((8.0, LINE_H), |m| (f32::from(m.cell_w), f32::from(m.line_h)));
                let grid = skeleton::Grid { cell_w, line_h, cols, rows };
                d.child(skeleton::overlay(&self.draft, self.preview.as_deref(), self.terminal.born.elapsed(), phase, grid))
            })
    }
}

impl TerminalView {
    /// Anything visible on screen yet. Startup mode/escape sequences wake us up but draw nothing.
    fn has_content(&self) -> bool {
        self.terminal.term.lock().renderable_content().display_iter.any(|c| !matches!(c.c, ' ' | '\0'))
    }
}

// ---------------------------------------------------------------------------------------------
// Element: layout, resize, paint
// ---------------------------------------------------------------------------------------------

struct TerminalElement {
    view: Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[derive(Clone, PartialEq)]
struct CellStyle {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

struct Batch {
    row: usize,
    col: usize,
    next_col: usize,
    text: String,
    style: CellStyle,
}

struct Prepaint {
    bounds: Bounds<Pixels>,
    cell_w: Pixels,
    line_h: Pixels,
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    glyph_quads: Vec<(Bounds<Pixels>, Hsla)>,
    cursor: Option<(Bounds<Pixels>, CursorShape, bool)>,
    /// IME composition text, painted over the grid at the cursor.
    preedit: Option<(Point<Pixels>, ShapedLine)>,
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let (family, font_size, focused, marked) = {
            let v = self.view.read(cx);
            (v.font_family, crate::scale::px(f32::from(v.font_size)), v.focus.is_focused(window), v.marked.clone())
        };
        let base_font: Font = gpui::font(family);
        let font_id = window.text_system().resolve_font(&base_font);
        let cell_w = window
            .text_system()
            .advance(font_id, font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(px(8.0));
        let line_h = (font_size * 1.35).round();

        let cols = ((bounds.size.width / cell_w).floor() as u16).max(2);
        let rows = ((bounds.size.height / line_h).floor() as u16).max(1);

        let terminal = &self.view.read(cx).terminal;
        terminal.resize(cols, rows, f32::from(cell_w), f32::from(line_h));
        let frac = self.view.read(cx).frac;
        self.view.update(cx, |v, _| {
            v.metrics = Some(Metrics {
                cell_w,
                line_h,
                origin: bounds.origin,
                shift: line_h * frac,
            });
        });
        // Where row 0 of the cells below is drawn (one row higher while the line above is peeking in).
        let top_y;

        let mut backgrounds: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();
        let mut batches: Vec<Batch> = Vec::new();
        let mut glyph_quads: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();
        let mut cursor_out = None;
        let mut preedit_at = None;

        {
            let terminal = &self.view.read(cx).terminal;
            let term = terminal.term.lock();
            let content = term.renderable_content();
            let offset = content.display_offset as i32;
            let colors = content.colors;
            let cursor: RenderableCursor = content.cursor;
            let selection = content.selection;
            let block_cursor = focused && cursor.shape == CursorShape::Block;
            let default_bg = theme::default_bg();

            // Between two lines the view is shifted down by `frac` of a line and the line above shows in the gap.
            let extra = (frac > 0.0 && term.grid().display_offset() < term.history_size()).then(|| Line(-offset - 1));
            let top = extra.is_some() as usize;
            top_y = bounds.origin.y + line_h * frac - line_h * top as f32;
            let cell_origin = |row: usize, col: usize| point(bounds.origin.x + cell_w * col as f32, top_y + line_h * row as f32);
            let above = extra.into_iter().flat_map(|l| (0..cols as usize).map(move |c| (l, c))).map(|(l, c)| (&term.grid()[l][Column(c)], GridPoint::new(l, Column(c))));

            // Open background span: (row, start_col, end_col_exclusive, color)
            let mut open_bg: Option<(usize, usize, usize, Hsla)> = None;
            let flush_bg = |span: &mut Option<(usize, usize, usize, Hsla)>,
                            out: &mut Vec<(Bounds<Pixels>, Hsla)>| {
                if let Some((row, c0, c1, color)) = span.take() {
                    out.push((
                        Bounds::new(
                            cell_origin(row, c0),
                            size(cell_w * (c1 - c0) as f32, line_h),
                        ),
                        color,
                    ));
                }
            };

            for (cell, p) in content.display_iter.map(|i| (i.cell, i.point)).chain(above) {
                let row = p.line.0 + offset;
                if row < -(top as i32) || row >= rows as i32 || p.column.0 >= cols as usize {
                    continue;
                }
                let (row, col) = ((row + top as i32) as usize, p.column.0);
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };

                let mut fg = theme::resolve(cell.fg, colors);
                let mut bg = theme::resolve(cell.bg, colors);
                let mut bg_is_default = theme::is_default_bg(&cell.bg);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                    bg_is_default = false;
                }
                if cell.flags.contains(Flags::DIM) {
                    fg = theme::dimmed(fg);
                }
                if block_cursor && p == cursor.point {
                    std::mem::swap(&mut fg, &mut bg);
                    bg_is_default = false;
                }
                let selected = selection.is_some_and(|s| s.contains(p));
                let bg_color = if selected {
                    bg_is_default = false;
                    theme::hsla(theme::SELECTION)
                } else {
                    theme::rgb_to_hsla(bg)
                };
                let _ = default_bg;

                // Background spans (merged per row)
                if bg_is_default {
                    flush_bg(&mut open_bg, &mut backgrounds);
                } else {
                    match &mut open_bg {
                        Some((r, _, end, color))
                            if *r == row && *end == col && *color == bg_color =>
                        {
                            *end = col + width
                        }
                        _ => {
                            flush_bg(&mut open_bg, &mut backgrounds);
                            open_bg = Some((row, col, col + width, bg_color));
                        }
                    }
                }

                // Text
                let underline =
                    cell.flags.intersects(Flags::ALL_UNDERLINES) || cell.hyperlink().is_some();
                let ch = cell.c;
                if cell.flags.contains(Flags::HIDDEN) || (ch == ' ' && !underline) || ch == '\0' {
                    continue;
                }
                // Block elements / box drawing: exact rectangles instead of font glyphs, so they tile seamlessly.
                let (ox, oy) = (f32::from(bounds.origin.x), f32::from(top_y));
                let (cw, lh) = (f32::from(cell_w), f32::from(line_h));
                let x0 = (ox + cw * col as f32).round();
                let x1 = (ox + cw * (col + width) as f32).round();
                let y0 = (oy + lh * row as f32).round();
                if let Some(rects) = glyphs::rects(ch, x1 - x0, lh) {
                    let color = theme::rgb_to_hsla(fg);
                    for rc in rects {
                        glyph_quads.push((
                            Bounds::new(
                                point(px(x0 + rc.x), px(y0 + rc.y)),
                                size(px(rc.w), px(rc.h)),
                            ),
                            color.opacity(rc.alpha),
                        ));
                    }
                    continue;
                }
                let style = CellStyle {
                    fg: theme::rgb_to_hsla(fg),
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                    underline,
                    strike: cell.flags.contains(Flags::STRIKEOUT),
                };
                // Only plain single-width chars batch; wide chars are placed individually.
                let batchable = width == 1;
                match batches.last_mut() {
                    Some(b)
                        if batchable && b.row == row && b.next_col == col && b.style == style =>
                    {
                        b.text.push(ch);
                        b.next_col += 1;
                    }
                    _ => batches.push(Batch {
                        row,
                        col,
                        next_col: col + width,
                        text: ch.to_string(),
                        style,
                    }),
                }
                if !batchable {
                    // Terminate so the next cell starts a fresh batch.
                    if let Some(b) = batches.last_mut() {
                        b.next_col = usize::MAX;
                    }
                }
            }
            flush_bg(&mut open_bg, &mut backgrounds);

            // Cursor (non-block shapes, or hollow when unfocused)
            let cur_row = cursor.point.line.0 + offset;
            if cur_row >= 0 && cur_row < rows as i32 {
                preedit_at = Some(cell_origin(cur_row as usize + top, cursor.point.column.0));
            }
            if cur_row >= 0
                && cur_row < rows as i32
                && cursor.point.column.0 < cols as usize
                && cursor.shape != CursorShape::Hidden
            {
                let shape = if !focused {
                    CursorShape::HollowBlock
                } else {
                    cursor.shape
                };
                if shape != CursorShape::Block {
                    let wide = term.grid()[cursor.point].flags.contains(Flags::WIDE_CHAR);
                    let origin = cell_origin(cur_row as usize + top, cursor.point.column.0);
                    let bounds =
                        Bounds::new(origin, size(cell_w * if wide { 2.0 } else { 1.0 }, line_h));
                    cursor_out = Some((bounds, shape, focused));
                }
            }
        }

        let mut lines = Vec::with_capacity(batches.len());
        for b in batches {
            let mut font = base_font.clone();
            if b.style.bold {
                font.weight = FontWeight::BOLD;
            }
            if b.style.italic {
                font.style = FontStyle::Italic;
            }
            let run = TextRun {
                len: b.text.len(),
                font,
                color: b.style.fg,
                background_color: None,
                underline: b.style.underline.then_some(UnderlineStyle {
                    color: Some(b.style.fg),
                    thickness: px(1.0),
                    wavy: false,
                }),
                strikethrough: b.style.strike.then_some(StrikethroughStyle {
                    color: Some(b.style.fg),
                    thickness: px(1.0),
                }),
            };
            let line =
                window
                    .text_system()
                    .shape_line(b.text.into(), font_size, &[run], Some(cell_w));
            let origin = point(
                bounds.origin.x + cell_w * b.col as f32,
                top_y + line_h * b.row as f32,
            );
            lines.push((origin, line));
        }

        let preedit = marked.zip(preedit_at).map(|(text, origin)| {
            let fg = theme::hsla(theme::TEXT);
            let run = TextRun {
                len: text.len(),
                font: base_font.clone(),
                color: fg,
                background_color: None,
                underline: Some(UnderlineStyle { color: Some(fg), thickness: px(1.0), wavy: false }),
                strikethrough: None,
            };
            (origin, window.text_system().shape_line(text.into(), font_size, &[run], None))
        });

        Prepaint {
            bounds,
            preedit,
            cell_w,
            line_h,
            backgrounds,
            lines,
            glyph_quads,
            cursor: cursor_out,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        pre: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_quad(fill(pre.bounds, theme::hsla(theme::TERMINAL_BG)));
        // A shifted view draws a row past the edges; the mask cuts it off.
        window.with_content_mask(Some(ContentMask { bounds: pre.bounds }), |window| {
        for (bounds, color) in pre.backgrounds.drain(..) {
            window.paint_quad(fill(bounds, color));
        }
        for (bounds, color) in pre.glyph_quads.drain(..) {
            window.paint_quad(fill(bounds, color));
        }
        for (origin, line) in pre.lines.drain(..) {
            let _ = line.paint(origin, pre.line_h, gpui::TextAlign::Left, None, window, cx);
        }
        if let Some((b, shape, _)) = pre.cursor.take() {
            let color = theme::hsla(theme::ACCENT);
            match shape {
                CursorShape::Beam => window.paint_quad(fill(
                    Bounds::new(b.origin, size(px(2.0), b.size.height)),
                    color,
                )),
                CursorShape::Underline => window.paint_quad(fill(
                    Bounds::new(
                        point(b.origin.x, b.origin.y + b.size.height - px(2.0)),
                        size(b.size.width, px(2.0)),
                    ),
                    color,
                )),
                _ => window.paint_quad(outline(b, color, gpui::BorderStyle::Solid)),
            }
        }
        if let Some((origin, line)) = pre.preedit.take() {
            window.paint_quad(fill(Bounds::new(origin, size(line.width, pre.line_h)), theme::hsla(theme::TERMINAL_BG)));
            let _ = line.paint(origin, pre.line_h, gpui::TextAlign::Left, None, window, cx);
        }
        });
        // Receive typed text / IME input while this terminal has focus.
        let focus = self.view.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(pre.bounds, self.view.clone()), cx);
        let _ = pre.cell_w;
    }
}
