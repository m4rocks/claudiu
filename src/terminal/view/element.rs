//! The terminal grid as Rune elements. Each screen row is one element of backgrounds, box-drawing rectangles and
//! text runs at their cells, memoized on what the row shows, so a frame rebuilds only the rows that changed. The
//! cursor, the input method's composition and the cards are drawn over the rows.

use std::hash::{Hash, Hasher};

use alacritty_terminal::term::RenderableCursor;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::CursorShape;
use rune::{Color, Cursor, El, Family, PointerButton, TextStyle, el, text};

use super::mouse::Button;
use super::*;
use crate::app::Workspace;

/// The terminal's font at `size` points.
pub(super) fn font(size: f32, bold: bool, italic: bool) -> TextStyle {
    let style = TextStyle::new(size, if bold { 700.0 } else { 400.0 }).family(Family::Named(crate::platform::monospace_font()));
    if italic { style.italic() } else { style }
}

#[derive(Clone, Copy, PartialEq)]
struct CellStyle {
    fg: Color,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

/// A run of cells in one style, from column `col`.
struct Batch {
    col: usize,
    next_col: usize,
    text: String,
    style: CellStyle,
}

/// What one screen row paints.
#[derive(Default)]
struct RowPaint {
    /// Backgrounds: start and end column, colour.
    bgs: Vec<(usize, usize, Color)>,
    /// Box drawing and block elements as rectangles: x, y, w, h in the row, colour.
    rects: Vec<([f32; 4], Color)>,
    batches: Vec<Batch>,
}

impl RowPaint {
    fn version(&self, m: Metrics) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        m.cell_w.to_bits().hash(&mut h);
        m.line_h.to_bits().hash(&mut h);
        for (a, b, c) in &self.bgs {
            (a, b, c).hash(&mut h);
        }
        for (r, c) in &self.rects {
            r.map(f32::to_bits).hash(&mut h);
            c.hash(&mut h);
        }
        for b in &self.batches {
            (b.col, &b.text, b.style.fg, b.style.bold, b.style.italic, b.style.underline, b.style.strike).hash(&mut h);
        }
        h.finish()
    }

    fn build(self, m: Metrics, font_size: f32) -> El {
        let mut row = el().w_full().h(m.line_h);
        for (c0, c1, color) in self.bgs {
            row = row.child(el().absolute((m.cell_w * c0 as f32).round(), 0.0).size((m.cell_w * c1 as f32).round() - (m.cell_w * c0 as f32).round(), m.line_h).fill(color));
        }
        for ([x, y, w, h], color) in self.rects {
            row = row.child(el().absolute(x, y).size(w, h).fill(color));
        }
        for b in self.batches {
            let x = m.cell_w * b.col as f32;
            let width = m.cell_w * b.text.chars().count() as f32;
            let s = b.style;
            row = row.child(text(b.text).font(font(font_size, s.bold, s.italic).lh(m.line_h)).color(s.fg).absolute(x, 0.0));
            if s.underline {
                row = row.child(el().absolute(x, m.line_h - 2.0).size(width, 1.0).fill(s.fg));
            }
            if s.strike {
                row = row.child(el().absolute(x, (m.line_h * 0.55).round()).size(width, 1.0).fill(s.fg));
            }
        }
        row
    }
}

impl TerminalView {
    /// The terminal as Rune elements, filling its parent. `focused`: it has the keyboard (a solid block cursor).
    pub fn render(&self, focused: bool, scale: f32) -> El {
        handlers(self.drawing(focused, scale), self.key.clone())
    }

    /// What `render` draws, before the pointer and keys are wired to the workspace.
    fn drawing(&self, focused: bool, scale: f32) -> El {
        let key = self.key.clone();
        let m = self.metrics();
        let font_size = crate::panels::scale::px(self.font_size);
        let (cols, rows) = self.terminal.size();
        // The shift is a whole number of device pixels, so the rows' text and their backgrounds move together.
        let shift = crate::panels::glide::snap(m.line_h * self.frac, scale);

        let mut paints: Vec<RowPaint> = Vec::new();
        let mut cursor_el = None;
        let mut preedit_at = None;
        let top;
        {
            let term = self.terminal.term.lock();
            let content = term.renderable_content();
            let offset = content.display_offset as i32;
            let colors = content.colors;
            let cursor: RenderableCursor = content.cursor;
            let selection = content.selection;
            let block_cursor = focused && cursor.shape == CursorShape::Block;

            // Between two lines the view is shifted down by `frac` of a line and the line above shows in the gap.
            let extra = (self.frac > 0.0 && term.grid().display_offset() < term.history_size()).then(|| Line(-offset - 1));
            top = extra.is_some() as usize;
            paints.resize_with(rows as usize + top, RowPaint::default);
            let above = extra
                .into_iter()
                .flat_map(|l| (0..cols as usize).map(move |c| (l, c)))
                .map(|(l, c)| (&term.grid()[l][Column(c)], GridPoint::new(l, Column(c))));

            for (cell, p) in content.display_iter.map(|i| (i.cell, i.point)).chain(above) {
                let row = p.line.0 + offset;
                if row < -(top as i32) || row >= rows as i32 || p.column.0 >= cols as usize {
                    continue;
                }
                let (row, col) = ((row + top as i32) as usize, p.column.0);
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
                let paint = &mut paints[row];

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
                    theme::rgb(theme::SELECTION)
                } else {
                    theme::term_color(bg)
                };
                // Background spans, merged along the row.
                if !bg_is_default {
                    match paint.bgs.last_mut() {
                        Some((_, end, color)) if *end == col && *color == bg_color => *end = col + width,
                        _ => paint.bgs.push((col, col + width, bg_color)),
                    }
                }

                let hovered_link = self.hovered_link.as_ref().is_some_and(|(r, columns)| *r == row && columns.as_ref().is_some_and(|c| c.contains(&col)));
                let link = cell.hyperlink();
                // Claude Code underlines a link it sees hovered; a row reserved for a card shows nothing of its own.
                let reserved = link.as_ref().is_some_and(|l| l.uri().starts_with(INLINE_LINK));
                let underline = !reserved && (cell.flags.intersects(Flags::ALL_UNDERLINES) || link.is_some() || hovered_link);
                let ch = cell.c;
                if cell.flags.contains(Flags::HIDDEN) || (ch == ' ' && !underline) || ch == '\0' {
                    continue;
                }
                // Block elements / box drawing: exact rectangles instead of font glyphs, so they tile seamlessly.
                let x0 = (m.cell_w * col as f32).round();
                let x1 = (m.cell_w * (col + width) as f32).round();
                if let Some(rects) = glyphs::rects(ch, x1 - x0, m.line_h) {
                    let color = theme::term_color(fg);
                    for rc in rects {
                        paint.rects.push(([x0 + rc.x, rc.y, rc.w, rc.h], color.alpha(rc.alpha)));
                    }
                    continue;
                }
                let style = CellStyle {
                    fg: theme::term_color(fg),
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                    underline,
                    strike: cell.flags.contains(Flags::STRIKEOUT),
                };
                // Only plain single-width chars batch; wide chars are placed individually, on their own cells.
                match paint.batches.last_mut() {
                    Some(b) if width == 1 && b.next_col == col && b.style == style => {
                        b.text.push(ch);
                        b.next_col += 1;
                    }
                    _ => paint.batches.push(Batch {
                        col,
                        next_col: if width == 1 { col + 1 } else { usize::MAX },
                        text: ch.to_string(),
                        style,
                    }),
                }
            }

            // Cursor (non-block shapes, or hollow when unfocused).
            let cur_row = cursor.point.line.0 + offset;
            if cur_row >= 0 && cur_row < rows as i32 {
                let x = m.cell_w * cursor.point.column.0 as f32;
                let y = shift + m.line_h * (cur_row as usize) as f32;
                preedit_at = Some((x, y));
                if cursor.point.column.0 < cols as usize && cursor.shape != CursorShape::Hidden {
                    let wide = term.grid()[cursor.point].flags.contains(Flags::WIDE_CHAR);
                    let w = m.cell_w * if wide { 2.0 } else { 1.0 };
                    let accent = theme::rgb(theme::ACCENT);
                    let shape = if focused { cursor.shape } else { CursorShape::HollowBlock };
                    let caret = match shape {
                        // Drawn in the cells already; this box only carries the input method's position.
                        CursorShape::Block => el().absolute(x, y).size(w, m.line_h),
                        CursorShape::Beam => el().absolute(x, y).size(2.0, m.line_h).fill(accent),
                        CursorShape::Underline => el().absolute(x, y + m.line_h - 2.0).size(w, 2.0).fill(accent),
                        _ => el().absolute(x, y).size(w, m.line_h).border(1.0, accent),
                    };
                    cursor_el = Some(if focused { caret.ime_cursor() } else { caret });
                }
            }
        }

        // Scrolling, and a program redrawing its screen, change what each row shows in place; it lands at once, or the
        // view would spring between lines and a row's words slide over from where the last text's words were.
        let mut grid = el().absolute(0.0, shift - m.line_h * top as f32).w_full().instant_changes();
        for (i, paint) in paints.into_iter().enumerate() {
            let version = paint.version(m);
            let row_key = format!("{key}/row{i}");
            grid = grid.child(el().absolute(0.0, m.line_h * i as f32).w_full().child(rune::memo(row_key, version, move || paint.build(m, font_size))));
        }

        let mut root = el()
            .key(key.clone())
            .w_full()
            .h_full()
            .fill(theme::rgb(theme::TERMINAL_BG))
            // A shifted view draws a row past the edges; the clip cuts it off.
            .clip_rounded(0.0)
            .child(grid);
        if let Some(c) = cursor_el {
            // Scrolling takes the cursor off screen and back; it shows again where it is, without fading in.
            root = root.child(c.key(format!("{key}/cursor")).transition(rune::Transition::none()).instant_changes());
        }
        if let (Some(text_), Some((x, y))) = (self.marked.clone(), preedit_at) {
            let fg = theme::rgb(theme::TEXT);
            let width = m.cell_w * text_.chars().count() as f32;
            root = root.child(
                el().absolute(x, y)
                    .instant_changes()
                    .h(m.line_h)
                    .fill(theme::rgb(theme::TERMINAL_BG))
                    .child(text(text_).font(font(font_size, false, false).lh(m.line_h)).color(fg))
                    .child(el().absolute(0.0, m.line_h - 2.0).size(width, 1.0).fill(fg)),
            );
        }
        root = root.children(self.inline_cards(m, shift));
        if self.held() {
            let grid = skeleton::Grid { cell_w: m.cell_w, line_h: m.line_h, cols, rows };
            let since = self.handover.as_ref().map_or(self.terminal.born, |h| h.since);
            root = root.child(skeleton::overlay(&self.draft, since.elapsed(), &self.phase(), grid).transition(rune::Transition::opacity()));
        }
        if self.over_link() {
            root = root.cursor(Cursor::Pointer);
        }
        root
    }
}

fn mods_of(tx: &Tx) -> keys::Modifiers {
    keys::Modifiers::from_rune(tx.mods())
}

/// The mouse and wheel, routed to the view by its key.
fn handlers(root: El, key: String) -> El {
    let (k1, k2, k3, k4, k5) = (key.clone(), key.clone(), key.clone(), key.clone(), key);
    root.pointer_buttons([PointerButton::Primary, PointerButton::Middle])
        .on_layout(move |ws: &mut Workspace, r: [f32; 4]| {
            if let Some(t) = ws.terminal_mut(&k1) {
                t.set_area(r[2], r[3]);
            }
        })
        .on_drag(move |ws: &mut Workspace, d: &mut rune::Drag, tx: &mut Tx| {
            let button = if tx.button() == PointerButton::Middle { Button::Middle } else { Button::Left };
            let mods = mods_of(tx);
            ws.focus_terminal(&k2);
            if let Some(t) = ws.terminal_mut(&k2) {
                match d.phase {
                    rune::Phase::Down => t.mouse_down(button, d.x, d.y, d.clicks, mods, tx),
                    rune::Phase::Move => t.mouse_move(d.x, d.y, Some(button), mods),
                    rune::Phase::Up => t.mouse_up(button, d.x, d.y, mods),
                }
            }
            ws.after_input(tx);
        })
        .on_secondary_press_local(move |ws: &mut Workspace, p: rune::PointerEvent, tx: &mut Tx| {
            let mods = mods_of(tx);
            ws.focus_terminal(&k3);
            if let Some(t) = ws.terminal_mut(&k3) {
                // A right click is one press and release; Rune reports it once released.
                t.mouse_down(Button::Right, p.x, p.y, 1, mods, tx);
                t.mouse_up(Button::Right, p.x, p.y, mods);
            }
            ws.after_input(tx);
        })
        .on_pointer_move_local(move |ws: &mut Workspace, p: rune::PointerEvent, tx: &mut Tx| {
            let mods = mods_of(tx);
            if let Some(t) = ws.terminal_mut(&k4) {
                t.mouse_move(p.x, p.y, None, mods);
                if !t.dirty {
                    tx.live_only();
                }
            }
        })
        .on_wheel(move |ws: &mut Workspace, e: rune::WheelEvent, tx: &mut Tx| {
            if let Some(t) = ws.terminal_mut(&k5) {
                t.wheel(e.dy, e.precise, e.x, e.y, keys::Modifiers::from_rune(e.mods));
            }
            ws.after_input(tx);
            true
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{SpawnSpec, Terminal};
    use alacritty_terminal::vte::ansi::Processor;

    fn write(v: &TerminalView, bytes: &str) {
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut *v.terminal.term.lock(), bytes.as_bytes());
    }

    /// The terminal drawn alone, its first frame settled, then `change` made: what is still on its way somewhere a
    /// frame later (fading, exiting, or moving toward another place).
    fn moving_after(change: impl FnOnce(&mut TerminalView)) -> Vec<String> {
        let (terminal, rx, _listener, _) = Terminal::build(&SpawnSpec::default(), 80, 24);
        let v = TerminalView::new(terminal, rx, "t");
        write(&v, &(0..60).map(|i| format!("line {i} with a few words\r\n")).collect::<String>());
        let mut rt = rune::Runtime::new(v, |v: &TerminalView, _: &mut rune::Cx<TerminalView>| v.drawing(true, 2.0), (800.0, 600.0), rune::Env::new(true, 2.0));
        for _ in 0..60 {
            rt.frame(1.0 / 60.0);
        }
        rt.update(|v, _| change(v));
        rt.frame(1.0 / 60.0);
        rt.elements()
            .into_iter()
            .filter(|el| el.exiting || el.opacity < 1.0 || el.rect.iter().zip(el.target).any(|(a, b)| (a - b).abs() > 0.01))
            .map(|el| format!("{} {:?} {:?} {:?}->{:?}", el.path, el.text, el.opacity, el.rect, el.target))
            .collect()
    }

    #[test]
    fn scrolling_back_moves_the_rows_without_springing() {
        let moving = moving_after(|v| v.set_position(3.4));
        assert!(moving.is_empty(), "{moving:#?}");
    }

    #[test]
    fn a_program_redrawing_its_screen_does_not_slide_words_across_a_row() {
        let moving = moving_after(|v| write(v, "\x1b[H\x1b[2J  indented then words\r\nx  y   z\r\n"));
        assert!(moving.is_empty(), "{moving:#?}");
    }
}
