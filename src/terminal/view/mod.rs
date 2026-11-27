//! GPUI view over a `Terminal`: paints the grid, turns GPUI input into VT input.
//! Children: `typing` (keys, paste, IME), `mouse` (selection, reporting, scrolling), `element` (the paint pass).

mod element;
mod mouse;
mod paths;
mod typing;

pub use typing::{SHELL_RECENT_LINES, format_shell_quote_block};

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
use crate::terminal::EventReceiver;
use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    KeyDownEvent, LayoutId, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ContentMask, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, ShapedLine, StrikethroughStyle, Style,
    TextRun, UTF16Selection, UnderlineStyle, Window, div, fill, outline, point, prelude::*, px, relative, size,
};

use gpui_kit::base::TextSelection;
use gpui_kit::component::input;

use crate::terminal::glyphs;
use crate::terminal::keys;
use crate::terminal::{SpawnSpec, Terminal};
use crate::panels::theme::{self};

pub enum TerminalEvent {
    Exited(Option<i32>),
    Bell,
    /// Ctrl+click on a file path in the text, or on a `file:` link: the path as written (maybe relative to the
    /// session's folder), and its line (1-based) when one was given.
    OpenPath(String, Option<usize>),
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
    /// A notch's steps for a program that scrolls itself (Claude Code's fullscreen view), see `wheel_step`.
    wheel: mouse::WheelSteps,
    pub title: Option<String>,
    /// Output timing, for `is_working`: when output last arrived and when the current burst began.
    last_output: Option<Instant>,
    burst_start: Option<Instant>,
    pub exit_code: Option<Option<i32>>,
    /// IME composition / dead-key text not yet committed; drawn at the cursor, never sent to the program.
    marked: Option<String>,
    font_family: &'static str,
    font_size: Pixels,
    /// Whether a pane currently shows this terminal (kept up to date by the workspace's render).
    pub visible: bool,
    /// Inactive windows still show output, but do not need the foreground frame rate.
    pub foreground: bool,
    last_redraw: Option<Instant>,
    redraw_task: Option<gpui::Task<()>>,
    /// While Ctrl is held over a link: its viewport row and the columns to underline (none when it is drawn as a link already).
    hovered_link: Option<(usize, Option<std::ops::Range<usize>>)>,
}

/// What Ctrl+click opens.
enum Link {
    /// A file path and the line it names.
    File(String, Option<usize>),
    Url(String),
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
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

    pub fn new(
        terminal: Terminal,
        rx: EventReceiver,
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
            wheel: Default::default(),
            title: None,
            last_output: None,
            burst_start: None,
            exit_code: None,
            marked: None,
            font_family: crate::platform::monospace_font(),
            font_size: px(14.0),
            visible: true,
            foreground: true,
            last_redraw: None,
            redraw_task: None,
            hovered_link: None,
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

    fn handle_event(&mut self, event: TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::Wakeup => {
                let t = Instant::now();
                if self.last_output.is_none_or(|l| t - l > Duration::from_millis(1500)) {
                    self.burst_start = Some(t);
                }
                self.last_output = Some(t);
                // Off-screen terminals only record the timing; a notify would redraw the whole window.
                if redraw_due(self.visible, self.foreground, self.last_redraw, t) {
                    self.redraw_task = None;
                    self.last_redraw = Some(t);
                    cx.notify()
                } else if self.visible && self.redraw_task.is_none() {
                    // Flush the final output even if the process falls silent before the next frame.
                    let delay = Duration::from_millis(250).saturating_sub(t - self.last_redraw.unwrap());
                    self.redraw_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(delay).await;
                        let _ = this.update(cx, |v, cx| {
                            v.redraw_task = None;
                            if v.visible {
                                v.last_redraw = Some(Instant::now());
                                cx.notify();
                            }
                        });
                    }));
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

}

fn redraw_due(visible: bool, foreground: bool, last: Option<Instant>, now: Instant) -> bool {
    visible && (foreground || last.is_none_or(|last| now.duration_since(last) >= Duration::from_millis(250)))
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.glide_step(window);
        self.wheel_step(window);
        div()
            .id("terminal")
            .relative()
            .size_full()
            .bg(theme::theme().background)
            .key_context("Terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            // The Edit menu dispatches the text input's actions; a focused terminal answers them too.
            .on_action(cx.listener(|this, _: &input::Copy, window, cx| {
                this.copy(window, cx);
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
            .when(self.hovered_link.is_some(), |d| d.cursor_pointer())
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(TerminalElement { view: cx.entity() })
    }
}

/// Drains terminal events, coalescing bursts so we redraw at most ~60 times a second.
async fn pump(
    this: gpui::WeakEntity<TerminalView>,
    mut rx: EventReceiver,
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

#[cfg(test)]
mod redraw_tests {
    use super::*;
    use futures::FutureExt;

    #[test]
    #[ignore = "manual performance measurement with wall-clock output"]
    fn benchmark_terminal_notifications() {
        for (visible, foreground) in [(true, true), (true, false), (false, false)] {
            let mut cx = gpui::TestAppContext::single();
            let (terminal, rx, listener, _) = Terminal::build(&SpawnSpec::default(), 80, 24);
            let view = cx.update(|app| app.new(|cx| TerminalView::new(terminal, rx, cx)));
            cx.update(|app| view.update(app, |v, _| {
                v.visible = visible;
                v.foreground = foreground;
            }));
            let mut notifications = cx.notifications(&view);
            let start = Instant::now();
            for _ in 0..60 {
                alacritty_terminal::event::EventListener::send_event(&listener, TermEvent::Wakeup);
                cx.run_until_parked();
                std::thread::sleep(Duration::from_millis(17));
                cx.executor().advance_clock(Duration::from_millis(17));
                cx.run_until_parked();
            }
            let mut count = 0;
            while notifications.next().now_or_never().flatten().is_some() { count += 1; }
            println!("visible={visible} foreground={foreground} output_events=60 notifications={count} elapsed_ms={}", start.elapsed().as_millis());
            if !visible { assert_eq!(count, 0); }
            else if foreground { assert_eq!(count, 60); }
            else { assert!(count <= 5, "background redraws: {count}"); }
        }
    }

    #[test]
    fn a_quiet_background_terminal_flushes_its_last_output() {
        let mut cx = gpui::TestAppContext::single();
        let (terminal, rx, _, _) = Terminal::build(&SpawnSpec::default(), 80, 24);
        let view = cx.update(|app| app.new(|cx| TerminalView::new(terminal, rx, cx)));
        let mut notifications = cx.notifications(&view);
        cx.update(|app| view.update(app, |v, cx| {
            v.foreground = false;
            v.handle_event(TermEvent::Wakeup, cx);
        }));
        assert!(notifications.next().now_or_never().flatten().is_some());
        cx.update(|app| view.update(app, |v, cx| v.handle_event(TermEvent::Wakeup, cx)));
        assert!(notifications.next().now_or_never().is_none());
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(251));
        cx.run_until_parked();
        assert!(notifications.next().now_or_never().flatten().is_some(), "the last output must be drawn without another PTY event");
        cx.update(|app| view.update(app, |v, cx| {
            v.visible = false;
            v.handle_event(TermEvent::Wakeup, cx);
        }));
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(notifications.next().now_or_never().is_none(), "hidden terminals do not redraw");
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
