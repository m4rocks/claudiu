//! Mouse selection, mouse reporting and scroll-wheel handling. Points are in the terminal's own points, from its
//! top-left (Rune's local coordinates).

use super::*;
use keys::Modifiers;

/// A wheel notch scrolls the terminal's scrollback this many times what the system reports for it, in lines.
const TERMINAL_NOTCH: f32 = 0.8;
/// The lines Windows reports for a wheel notch at its default setting.
const NOTCH_LINES: f32 = 3.0;

/// A mouse button as VT mouse reports number it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    fn code(self) -> u8 {
        match self {
            Button::Left => 0,
            Button::Middle => 1,
            Button::Right => 2,
        }
    }
}

impl TerminalView {
    /// The cell under a point, which half of it, and its viewport column and row.
    pub(super) fn grid_point(&self, x: f32, y: f32) -> Option<(GridPoint, Side, usize, usize)> {
        self.area?;
        let m = self.metrics();
        let (cols, rows) = self.terminal.size();
        let x = x.max(0.0);
        let y = (y - self.shift()).max(0.0);
        let col = ((x / m.cell_w) as usize).min(cols as usize - 1);
        let row = ((y / m.line_h) as usize).min(rows as usize - 1);
        let side = if x - col as f32 * m.cell_w < m.cell_w / 2.0 { Side::Left } else { Side::Right };
        let offset = self.terminal.term.lock().grid().display_offset() as i32;
        Some((GridPoint::new(Line(row as i32 - offset), Column(col)), side, col, row))
    }

    /// How far the rows are drawn below their cells while scrolling between two lines.
    pub(super) fn shift(&self) -> f32 {
        self.metrics().line_h * self.frac
    }

    pub(super) fn report_mouse(&mut self, button: u8, col: usize, row: usize, pressed: bool, mods: &Modifiers, motion: bool) {
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
            format!("\x1b[<{};{};{}{}", code, col + 1, row + 1, if pressed { 'M' } else { 'm' }).into_bytes()
        } else {
            if col >= 223 || row >= 223 {
                return;
            }
            let code = if pressed { code } else { 3 | (code & !3) };
            vec![0x1b, b'[', b'M', 32 + code, 32 + col as u8 + 1, 32 + row as u8 + 1]
        };
        self.terminal.write(bytes);
    }

    pub(super) fn mouse_reporting(&self, mods: &Modifiers) -> bool {
        self.mode().intersects(TermMode::MOUSE_MODE) && !mods.shift
    }

    /// A button went down at `x, y`; `clicks` counts a run of quick presses (2 selects words, 3 lines).
    pub fn mouse_down(&mut self, button: Button, x: f32, y: f32, clicks: u32, mods: Modifiers, tx: &mut Tx) {
        let Some((point, side, col, row)) = self.grid_point(x, y) else { return };

        // Ctrl+click on a path or link opens it even when the program (Claude Code's full-screen view) has asked for the
        // mouse; any other click goes to the program.
        if button == Button::Left && mods.control
            && let Some((link, _)) = self.link_at(point, col)
        {
            match link {
                Link::File(path, line) => self.events.push(TerminalEvent::OpenPath(path, line)),
                Link::Url(url) => {
                    let _ = tx.open_url(&url);
                }
            }
            return;
        }

        if self.mouse_reporting(&mods) {
            self.report_mouse(button.code(), col, row, true, &mods, false);
            return;
        }

        match button {
            Button::Left => {
                let ty = match clicks {
                    0 | 1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                self.terminal.term.lock().selection = Some(Selection::new(ty, point, side));
                self.selecting = true;
                self.dirty = true;
            }
            // Windows Terminal convention: right click copies when there is a selection, else pastes.
            Button::Right => {
                if !self.copy(tx) {
                    self.paste(tx);
                }
            }
            Button::Middle => self.paste(tx),
        }
    }

    pub fn mouse_up(&mut self, button: Button, x: f32, y: f32, mods: Modifiers) {
        if self.mouse_reporting(&mods) {
            if let Some((_, _, col, row)) = self.grid_point(x, y) {
                self.report_mouse(button.code(), col, row, false, &mods, false);
            }
            return;
        }
        if self.selecting {
            self.selecting = false;
            let mut term = self.terminal.term.lock();
            if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
                term.selection = None;
            }
            self.dirty = true;
        }
    }

    /// The pointer moved; `held` is the button being dragged, if any.
    pub fn mouse_move(&mut self, x: f32, y: f32, held: Option<Button>, mods: Modifiers) {
        let hovered = self
            .grid_point(x, y)
            .filter(|_| mods.control)
            .and_then(|(point, _, col, row)| self.link_at(point, col).map(|(_, columns)| (row, columns)));
        if hovered != self.hovered_link {
            self.hovered_link = hovered;
            self.dirty = true;
        }
        if self.mouse_reporting(&mods) {
            let mode = self.mode();
            let any_motion = mode.contains(TermMode::MOUSE_MOTION);
            let drag = mode.contains(TermMode::MOUSE_DRAG) && held.is_some();
            if (any_motion || drag)
                && let Some((_, _, col, row)) = self.grid_point(x, y)
            {
                let button = held.map_or(3, Button::code);
                self.report_mouse(button, col, row, true, &mods, true);
            }
            return;
        }
        if self.selecting
            && held.is_some()
            && let Some((point, side, ..)) = self.grid_point(x, y)
        {
            if let Some(sel) = self.terminal.term.lock().selection.as_mut() {
                sel.update(point, side);
            }
            self.dirty = true;
        }
    }

    /// Whether Ctrl is over a link, so the pointer shows a hand.
    pub fn over_link(&self) -> bool {
        self.hovered_link.is_some()
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
            if link.uri().starts_with(INLINE_LINK) {
                return None;
            }
            let file = url::Url::parse(link.uri()).ok().filter(|u| u.scheme() == "file").and_then(|u| u.to_file_path().ok());
            // Cells of a link are underlined already.
            return Some((
                match file {
                    Some(path) => Link::File(path.to_string_lossy().into_owned(), None),
                    None => Link::Url(link.uri().to_string()),
                },
                None,
            ));
        }
        let (path, line, columns) = paths::path_at(&row, column)?;
        Some((Link::File(path, line), Some(columns)))
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
        self.dirty = true;
    }

    /// A wheel notch moves the target and the view eases there; a trackpad's steps are applied as they come.
    pub(super) fn scroll_back(&mut self, lines: f32, notch: bool) {
        let top = self.terminal.term.lock().history_size() as f32;
        let here = self.position();
        if notch {
            crate::panels::glide::Glide::nudge(&mut self.glide, here, lines * TERMINAL_NOTCH, 0.0, top);
        } else {
            self.glide = None;
            self.set_position((here + lines).clamp(0.0, top));
        }
    }

    /// One frame of an easing scroll; whether another is needed.
    pub(super) fn glide_step(&mut self) -> bool {
        let Some(g) = &mut self.glide else { return false };
        let moving = g.step(0.01);
        let p = g.shown;
        if !moving {
            self.glide = None;
        }
        self.set_position(p);
        moving
    }

    /// The wheel over the terminal: `dy` points, positive moving the content down (scrolling back). `precise` is a
    /// trackpad's continuous motion, not a mouse wheel's notches.
    pub fn wheel(&mut self, dy: f32, precise: bool, x: f32, y: f32, mods: Modifiers) {
        let m = self.metrics();
        let notch = !precise;
        // A notch arrives as points too: Rune counts a line of a notch as about this many.
        let lines = if notch { (dy / 40.0) * NOTCH_LINES } else { dy / m.line_h };
        let mode = self.mode();
        let alt_scroll = mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL);
        // Plain scrollback is ours to scroll smoothly; a program that takes the wheel gets whole lines.
        if !self.mouse_reporting(&mods) && !alt_scroll {
            self.scroll_back(lines, notch);
            self.dirty = true;
            return;
        }
        // A mouse report means one notch, as in other terminals. Claude Code reads several in a row as fast spinning and
        // speeds up: three per notch moved its view 25 lines, one moves it 3.
        let per_notch = if notch && self.mouse_reporting(&mods) { NOTCH_LINES } else { 1.0 };
        self.scroll_px += lines / per_notch;
        let whole = self.scroll_px.trunc() as i32;
        if whole == 0 {
            return;
        }
        self.scroll_px -= whole as f32;
        // The program scrolls itself, a whole line per step. Sent all at once, a notch's steps land as one jump;
        // one per frame (see `wheel_step`), they roll the view a line at a time.
        if notch {
            self.wheel.add(whole, (x, y), mods);
        } else {
            self.send_wheel(whole, (x, y), &mods);
        }
    }

    /// One frame of a notch's steps to the program; whether more wait.
    pub(super) fn wheel_step(&mut self) -> bool {
        let Some((step, position, mods)) = self.wheel.take() else { return false };
        self.send_wheel(step, position, &mods);
        !self.wheel.is_empty()
    }

    /// `whole` lines of wheel, up when positive, to a program that takes the wheel: as mouse reports, or as arrow
    /// keys on its alternate screen.
    fn send_wheel(&mut self, whole: i32, (x, y): (f32, f32), mods: &Modifiers) {
        if self.mouse_reporting(mods) {
            if let Some((_, _, col, row)) = self.grid_point(x, y) {
                let button = if whole > 0 { 64 } else { 65 };
                for _ in 0..whole.unsigned_abs() {
                    self.report_mouse(button, col, row, true, mods, false);
                }
            }
        } else {
            let app = self.mode().contains(TermMode::APP_CURSOR);
            let key = match (whole > 0, app) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.terminal.write(key.repeat(whole.unsigned_abs() as usize).into_bytes());
        }
    }
}

/// Wheel steps waiting to go to the program, one per frame; a turn the other way cancels steps still waiting.
#[derive(Default)]
pub(super) struct WheelSteps {
    /// Lines left to send, up when positive.
    left: i32,
    /// Where the pointer was and which keys were held at the last notch.
    at: Option<((f32, f32), Modifiers)>,
}

impl WheelSteps {
    fn add(&mut self, lines: i32, position: (f32, f32), mods: Modifiers) {
        self.left += lines;
        self.at = Some((position, mods));
    }

    /// The next single step, with where it happened.
    fn take(&mut self) -> Option<(i32, (f32, f32), Modifiers)> {
        let (position, mods) = self.at?;
        if self.left == 0 {
            return None;
        }
        let step = self.left.signum();
        self.left -= step;
        Some((step, position, mods))
    }

    fn is_empty(&self) -> bool {
        self.left == 0
    }
}

#[cfg(test)]
mod tests {
    use super::WheelSteps;

    #[test]
    fn a_notch_goes_out_a_line_at_a_time_and_a_turn_back_cancels_what_waits() {
        let mut w = WheelSteps::default();
        let at = (1.0, 2.0);
        w.add(3, at, Default::default());
        assert_eq!(w.take().map(|s| s.0), Some(1));
        assert!(!w.is_empty());
        w.add(-3, at, Default::default());
        assert_eq!(w.take().map(|s| s.0), Some(-1), "two up were waiting, three down came: one down is left");
        assert!(w.is_empty());
        assert_eq!(w.take().map(|s| s.0), None);
    }
}
