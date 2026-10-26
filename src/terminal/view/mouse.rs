//! Mouse selection, mouse reporting and scroll-wheel handling.

use super::*;

impl TerminalView {
    pub(super) fn grid_point(&self, position: Point<Pixels>) -> Option<(GridPoint, Side, usize, usize)> {
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

    pub(super) fn report_mouse(
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

    pub(super) fn mouse_reporting(&self, mods: &Modifiers) -> bool {
        self.mode().intersects(TermMode::MOUSE_MODE) && !mods.shift
    }

    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        let Some((point, side, col, row)) = self.grid_point(event.position) else {
            return;
        };

        // Ctrl+click on a path or link opens it even when the program (Claude Code's full-screen view) has asked for the
        // mouse; any other click goes to the program.
        if event.button == MouseButton::Left && event.modifiers.control {
            if let Some((link, _)) = self.link_at(point, col) {
                match link {
                    Link::File(path, line) => cx.emit(TerminalEvent::OpenPath(path, line)),
                    Link::Url(url) => cx.open_url(&url),
                }
                return;
            }
        }

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

    pub(super) fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
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

    /// The file path or link under `point` (at viewport `column`), and the columns to underline for it.
    fn link_at(&self, point: GridPoint, column: usize) -> Option<(Link, Option<std::ops::Range<usize>>)> {
        let (hyperlink, row) = {
            let term = self.terminal.term.lock();
            let grid = term.grid();
            let row = &grid[point.line];
            (grid[point].hyperlink(), (0..grid.columns()).map(|c| row[Column(c)].c).collect::<String>())
        };
        if let Some(link) = hyperlink {
            let file = url::Url::parse(link.uri()).ok().filter(|u| u.scheme() == "file").and_then(|u| u.to_file_path().ok());
            // Cells of a link are underlined already.
            return Some((match file {
                Some(path) => Link::File(path.to_string_lossy().into_owned(), None),
                None => Link::Url(link.uri().to_string()),
            }, None));
        }
        let (path, line, columns) = paths::path_at(&row, column)?;
        Some((Link::File(path, line), Some(columns)))
    }

    pub(super) fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let hovered = self
            .grid_point(event.position)
            .filter(|_| event.modifiers.control)
            .and_then(|(point, _, col, row)| self.link_at(point, col).map(|(_, columns)| (row, columns)));
        if hovered != self.hovered_link {
            self.hovered_link = hovered;
            cx.notify();
        }
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
    pub(super) fn position(&self) -> f32 {
        self.terminal.term.lock().grid().display_offset() as f32 + self.frac
    }

    /// Show the scrollback at `p` lines up from the bottom: whole lines are alacritty's offset, the rest is `frac`.
    pub(super) fn set_position(&mut self, p: f32) {
        let mut term = self.terminal.term.lock();
        let (now, to) = (term.grid().display_offset() as i32, p.floor() as i32);
        if to != now {
            term.scroll_display(Scroll::Delta(to - now));
        }
        self.frac = if term.grid().display_offset() as i32 == to { p - to as f32 } else { 0.0 };
    }

    /// A wheel notch moves the target and the view eases there; a trackpad's pixel steps are applied as they come.
    pub(super) fn scroll_back(&mut self, lines: f32, notch: bool) {
        let top = self.terminal.term.lock().history_size() as f32;
        let here = self.position();
        if notch {
            crate::panels::glide::Glide::nudge(&mut self.glide, here, lines * crate::panels::glide::NOTCH, 0.0, top);
        } else {
            self.glide = None;
            self.set_position((here + lines).clamp(0.0, top));
        }
    }

    /// One frame of an easing scroll.
    pub(super) fn glide_step(&mut self, window: &mut Window) {
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

    pub(super) fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
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
