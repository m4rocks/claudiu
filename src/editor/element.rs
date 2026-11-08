//! The editor as Rune elements: the line numbers and change marks, then the text. Only the lines on screen are built,
//! and each is memoized on what it shows, so typing rebuilds one line and scrolling builds the few that came in.

use std::hash::{Hash, Hasher};

use rune::{Color, Cursor, El, Family, TextStyle, el, text};

use super::buffer::Pos;
use super::{Editor, Mark, Span, columns};
use crate::app::Workspace;
use crate::panels::widgets::icon;
use crate::panels::{icons, theme};
use crate::terminal::view::Metrics;

/// How big a fold chevron is drawn, as gpui-kit's.
const FOLD_ICON: f32 = 14.0;

/// The editor's font at `size` points.
pub(super) fn font(size: f32) -> TextStyle {
    TextStyle::new(size, 400.0).family(Family::Named(crate::platform::monospace_font()))
}

/// One screen row of a line: its bytes, the cell of the line it starts at, and the cells it is indented by.
#[derive(Clone, Hash)]
struct Row {
    bytes: std::ops::Range<usize>,
    cell: usize,
    shift: usize,
}

/// What one line of text paints, in cells from the line's start, over the rows it wraps to.
struct LinePaint {
    text: String,
    rows: Vec<Row>,
    spans: Vec<Span>,
    /// The selected cells, for each selection touching this line (a trailing cell shows its line break).
    selected: Vec<(usize, usize)>,
    /// Underlined problems: from and to cell, and whether it is an error.
    problems: Vec<(usize, usize, bool)>,
    /// Search matches: from and to cell, and whether it is the current one.
    found: Vec<(usize, usize, bool)>,
    /// The cells of the line's indent guides.
    guides: Vec<usize>,
}

impl LinePaint {
    fn version(&self, m: Metrics) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (m.cell_w.to_bits(), m.line_h.to_bits(), &self.text, &self.rows, &self.selected, &self.problems, &self.found, &self.guides).hash(&mut h);
        for s in &self.spans {
            (&s.bytes, s.color).hash(&mut h);
        }
        h.finish()
    }

    fn build(self, m: Metrics, size: f32) -> El {
        let mut line = el().w_full().h(m.line_h * self.rows.len() as f32);
        for (k, r) in self.rows.iter().enumerate() {
            let next = self.rows.get(k + 1).map_or(usize::MAX, |n| n.cell);
            line = line.child(el().absolute(0.0, m.line_h * k as f32).w_full().h(m.line_h).children(self.row(r, next, m, size)));
        }
        line
    }

    /// What row `r` paints, from the line's cells `r.cell` up to `next`, where the next row starts.
    fn row(&self, r: &Row, next: usize, m: Metrics, size: f32) -> Vec<El> {
        let x = |cell: usize| (m.cell_w * (cell - r.cell + r.shift) as f32).round();
        // The part of cells `a..b` on this row.
        let clip = |a: usize, b: usize| {
            let (a, b) = (a.max(r.cell), b.min(next));
            (a < b).then(|| (x(a), x(b) - x(a)))
        };
        let mut out = vec![];
        for &cell in &self.guides {
            out.push(el().absolute((m.cell_w * cell as f32).round(), 0.0).size(1.0, m.line_h).fill(theme::rgb(theme::BORDER).alpha(0.85)));
        }
        for &(a, b, current) in &self.found {
            let Some((at, w)) = clip(a, b) else { continue };
            let fill = if current { theme::rgb(theme::FOUND).alpha(0.45) } else { theme::rgb(theme::FOUND).alpha(0.18) };
            let mut mark = el().absolute(at, 0.0).size(w.max(m.cell_w), m.line_h).radius(2.0).fill(fill);
            if current {
                mark = mark.border(1.0, theme::rgb(theme::FOUND).alpha(0.8));
            }
            out.push(mark);
        }
        for (at, w) in self.selected.iter().filter_map(|&(a, b)| clip(a, b)) {
            out.push(el().absolute(at, 0.0).size(w, m.line_h).fill(theme::rgb(theme::SELECTION)));
        }
        // The text in runs of one colour, each placed at its cell so tabs and wide characters line up.
        let text_color = theme::rgb(theme::TEXT);
        let mut at = r.bytes.start;
        let mut runs: Vec<(usize, &str, Color)> = vec![];
        for s in &self.spans {
            let (from, to) = (s.bytes.start.clamp(r.bytes.start, r.bytes.end), s.bytes.end.clamp(r.bytes.start, r.bytes.end));
            if from < at || from == to || !self.text.is_char_boundary(from) || !self.text.is_char_boundary(to) {
                continue;
            }
            if at < from {
                runs.push((at, &self.text[at..from], text_color));
            }
            runs.push((from, &self.text[from..to], s.color));
            at = to;
        }
        if at < r.bytes.end {
            runs.push((at, &self.text[at..r.bytes.end], text_color));
        }
        for (start, part, color) in runs {
            if part.trim().is_empty() {
                continue;
            }
            let cell = columns(&self.text[..start]);
            // Tabs inside a run are drawn as the spaces they stand for.
            let shown = if part.contains('\t') { expand_tabs(part, cell) } else { part.to_string() };
            out.push(text(shown).font(font(size).lh(m.line_h)).color(color).absolute(x(cell), 0.0));
        }
        for &(a, b, error) in &self.problems {
            let Some((at, w)) = clip(a, b) else { continue };
            let color = theme::rgb(if error { theme::DANGER } else { theme::WARN });
            out.push(el().absolute(at, m.line_h - 2.0).size(w.max(m.cell_w), 1.5).fill(color));
        }
        out
    }
}

fn expand_tabs(part: &str, start: usize) -> String {
    let mut out = String::new();
    let mut x = start;
    for c in part.chars() {
        let w = super::width(c, x);
        if c == '\t' {
            out.extend(std::iter::repeat_n(' ', w));
        } else {
            out.push(c);
        }
        x += w;
    }
    out
}

impl Editor {
    /// The editor filling its parent. `focused`: it has the keyboard (a caret, and the current line lit).
    pub fn render(&self, focused: bool, scale: f32) -> El {
        let key = self.key.clone();
        let m = self.metrics();
        let size = crate::panels::scale::px(self.font_size);
        let gutter = self.gutter_w();
        let lines = self.buffer.line_count();
        let rows = self.row_count();
        let first_row = ((self.scroll[1] / m.line_h).floor().max(0.0) as usize).min(rows - 1);
        let shown = (self.area[1] / m.line_h).ceil() as usize + 1;
        let last_row = (first_row + shown).min(rows);
        // Lines are drawn whole, so the first one may start above the top edge.
        let first = self.row_line(first_row).0;
        let top_row = self.line_row(first);
        // A whole number of device pixels, so text and its backgrounds move together.
        let top = crate::panels::glide::snap(top_row as f32 * m.line_h - self.scroll[1], scale);
        let y_of = |row: usize| m.line_h * (row - top_row) as f32;
        let selections: Vec<(Pos, Pos)> = self.cursors().iter().filter(|s| !s.is_empty()).map(|s| s.range()).collect();
        let caret = self.sel.head;
        let caret_row = self.caret_row();

        let mut numbers = el().absolute(0.0, top).w(gutter).h_full();
        let mut text_rows = el().absolute(-self.scroll[0].round(), top).w_full();
        let mut guides: Vec<usize> = vec![];
        let mut line = first;
        while line < lines && self.line_row(line) < last_row {
            if self.hidden(line) {
                line += 1;
                continue;
            }
            let row = self.line_row(line);
            let y = y_of(row);
            let number_color = if line == caret.line { theme::TEXT_DIM } else { theme::TEXT_FAINT };
            numbers = numbers.child(
                el().absolute(0.0, y)
                    .w(gutter - super::GUTTER_PAD - super::FOLD_W)
                    .h(m.line_h)
                    .justify_end()
                    .child(text((line + 1).to_string()).font(font(size).lh(m.line_h)).color(theme::rgb(number_color))),
            );
            if let Ok(i) = self.marks.binary_search_by_key(&line, |(l, _)| *l) {
                let mark = &self.marks[i].1;
                numbers = numbers.child(mark_bar(*mark, y, m).key(format!("{key}/mark{line}")));
            }
            // As in gpui-kit: a block's chevron shows while the pointer is over the gutter, on the caret's row, or
            // while the block is folded.
            if self.fold_spot(line).is_some() {
                let folded = self.is_folded(line);
                if folded || self.gutter_hover || row == caret_row {
                    let glyph = if folded { &icons::CHEVRON_RIGHT } else { &icons::CHEVRON_DOWN };
                    let x = gutter - super::GUTTER_PAD - super::FOLD_W + (super::FOLD_W - FOLD_ICON) / 2.0;
                    numbers = numbers.child(el().key(format!("{key}/fold{line}")).absolute(x, y + (m.line_h - FOLD_ICON) / 2.0).child(icon(glyph, FOLD_ICON, theme::TEXT_FAINT)));
                }
            }
            let text_ = self.buffer.line(line);
            let selected = selections
                .iter()
                .filter(|(a, b)| a.line <= line && line <= b.line)
                .map(|(sel_a, sel_b)| {
                    let a = if line == sel_a.line { columns(&text_[..sel_a.col]) } else { 0 };
                    let b = if line == sel_b.line { columns(&text_[..sel_b.col]) } else { columns(text_) + 1 };
                    (a, b)
                })
                .collect();
            let problems = self
                .problems
                .iter()
                .filter(|p| p.from.line <= line && line <= p.to.line)
                .map(|p| {
                    let a = if p.from.line == line { self.column(p.from) } else { 0 };
                    let b = if p.to.line == line { self.column(p.to) } else { columns(text_) };
                    (a, b.max(a + 1), p.error)
                })
                .collect();
            let found = self.found_on(line, text_);
            guides = indent_guides(guides, text_, self.indent_unit);
            let (breaks, indent) = self.line_breaks(line);
            let row_list = std::iter::once(0)
                .chain(breaks.iter().copied())
                .enumerate()
                .map(|(k, start)| Row { bytes: self.row_bytes(line, k), cell: columns(&text_[..start]), shift: if k == 0 { 0 } else { indent } })
                .collect();
            let paint = LinePaint { text: text_.to_string(), rows: row_list, spans: self.spans.get(line).cloned().unwrap_or_default(), selected, problems, found, guides: guides.clone() };
            let version = paint.version(m);
            text_rows = text_rows.child(el().absolute(0.0, y).w_full().child(rune::memo(format!("{key}/l{line}"), version, move || paint.build(m, size))));
            line += 1;
        }

        let mut body = el().absolute(gutter, 0.0).size((self.area[0] - gutter).max(0.0), self.area[1]).clip_rounded(0.0);
        let caret_line_row = self.line_row(caret.line);
        let caret_hidden = self.hidden(caret.line);
        if focused && self.sel.is_empty() && !caret_hidden && caret_line_row < last_row && caret_line_row >= top_row {
            let h = m.line_h * (self.line_breaks(caret.line).0.len() + 1) as f32;
            body = body.child(el().absolute(0.0, top + y_of(caret_line_row)).w_full().h(h).fill(theme::rgb(theme::ROW_ACTIVE)));
        }
        body = body.child(text_rows);
        // The other cursors' carets, wherever the main one is.
        for (i, (row, cell)) in self.other_carets().into_iter().enumerate() {
            if focused && self.caret_on && (top_row..last_row).contains(&row) {
                let x = (cell as f32 * m.cell_w - self.scroll[0]).round();
                body = body.child(el().key(format!("{key}/caret{i}")).absolute(x - super::CARET_W / 2.0, top + y_of(row)).size(super::CARET_W, m.line_h).fill(theme::rgb(theme::ACCENT)));
            }
        }
        // A caret a fold hides isn't drawn, as in gpui-kit.
        if !caret_hidden && (top_row..last_row).contains(&caret_row) {
            let x = (self.caret_cell() as f32 * m.cell_w - self.scroll[0]).round();
            let y = top + y_of(caret_row);
            if let Some(marked) = self.marked.clone() {
                let width = m.cell_w * columns(&marked) as f32;
                let fg = theme::rgb(theme::TEXT);
                body = body.child(
                    el().absolute(x, y)
                        .h(m.line_h)
                        .fill(theme::rgb(theme::TERMINAL_BG))
                        .child(text(marked).font(font(size).lh(m.line_h)).color(fg))
                        .child(el().absolute(0.0, m.line_h - 2.0).size(width, 1.0).fill(fg)),
                );
            }
            // The bar stays laid out while it blinks off, so the input method keeps its place.
            let bar = el().absolute(x - super::CARET_W / 2.0, y).size(super::CARET_W, m.line_h);
            body = body.child(match focused {
                true if self.caret_on => bar.fill(theme::rgb(theme::ACCENT)).ime_cursor(),
                true => bar.ime_cursor(),
                false => bar,
            });
        }
        let body_area = [(self.area[0] - gutter).max(0.0), self.area[1]];
        body = body.children(self.render_assist(m.cell_w, m.line_h, top_row, top, body_area));

        let root = el()
            .key(key.clone())
            .w_full()
            .h_full()
            .fill(theme::rgb(theme::TERMINAL_BG))
            .clip_rounded(0.0)
            .cursor(Cursor::Text)
            .child(numbers)
            .child(body);
        handlers(root, key)
    }
}

impl Editor {
    /// The search matches on `line` (whose text is `text`), in cells. Matches are in order, so the search starts at the
    /// first that could reach this line.
    fn found_on(&self, line: usize, text: &str) -> Vec<(usize, usize, bool)> {
        let from = self.found.partition_point(|(_, b)| b.line < line);
        self.found[from..]
            .iter()
            .enumerate()
            .take_while(|(_, (a, _))| a.line <= line)
            .map(|(i, &(a, b))| {
                let start = if a.line == line { columns(&text[..a.col]) } else { 0 };
                let end = if b.line == line { columns(&text[..b.col]) } else { columns(text) + 1 };
                (start, end, self.found_current == Some(from + i))
            })
            .collect()
    }
}

/// A line's indent guides, in cells, as main's editor drew them: one at each level of its indent, starting at the
/// first column; an empty line keeps `above`, the guides of the line above it.
fn indent_guides(above: Vec<usize>, text: &str, unit: usize) -> Vec<usize> {
    if text.is_empty() {
        return above;
    }
    let indent = columns(&text[..text.len() - text.trim_start_matches([' ', '\t']).len()]);
    (0..indent).step_by(unit.max(1)).collect()
}

/// The bar beside a changed line's number, or the wedge between two lines where some were deleted.
fn mark_bar(mark: Mark, y: f32, m: Metrics) -> El {
    match mark {
        Mark::Added => el().absolute(4.0, y).size(3.0, m.line_h).fill(theme::rgb(theme::OK)),
        Mark::Modified => el().absolute(4.0, y).size(3.0, m.line_h).fill(theme::rgb(0x61afef)),
        Mark::Removed => el().absolute(3.0, y - 2.0).size(6.0, 4.0).radius(2.0).fill(theme::rgb(theme::DANGER)),
    }
}

/// The mouse and wheel, routed to the editor by its key.
fn handlers(root: El, key: String) -> El {
    let (k1, k2, k3, k4, k5, k6) = (key.clone(), key.clone(), key.clone(), key.clone(), key.clone(), key);
    root.on_layout(move |ws: &mut Workspace, r: [f32; 4]| {
        if let Some(e) = ws.editor_mut(&k1) {
            e.set_area(r[2], r[3]);
        }
    })
    .on_drag(move |ws: &mut Workspace, d: &mut rune::Drag, tx: &mut rune::Tx| {
        let mods = tx.mods();
        let extend = mods.shift;
        // Cmd+click (Ctrl+click elsewhere) goes to the definition of what was clicked, as in VS Code.
        let definition = if cfg!(target_os = "macos") { mods.logo && !mods.ctrl } else { mods.ctrl && !mods.logo };
        ws.focus_editor(&k2);
        if let Some(e) = ws.editor_mut(&k2) {
            // Alt+Shift starts a block selection; Linux also takes Ctrl+Alt, as gpui-kit did.
            let block = mods.alt && (mods.shift || (cfg!(target_os = "linux") && mods.ctrl));
            if d.phase == rune::Phase::Down {
                e.press_done = false;
                e.block_from = None;
                if let Some(line) = e.fold_at(d.x, d.y) {
                    e.press_done = true;
                    e.toggle_fold(line);
                } else if block {
                    e.press_done = true;
                    e.start_block(d.x, d.y);
                } else if mods.alt && !definition && d.clicks == 1 {
                    // Alt+click adds a cursor, as in main's editor; dragging on from it selects a block.
                    e.press_done = true;
                    let p = e.pos_at(d.x, d.y);
                    e.add_cursor_at(p);
                    e.start_block_here(d.x, d.y);
                }
            }
            match d.phase {
                rune::Phase::Move if e.press_done => e.drag_block(d.x, d.y),
                // Letting go ends the block where the last move left it, even outside the editor, as in gpui-kit.
                rune::Phase::Up if e.press_done => e.end_block(),
                _ if e.press_done => {}
                rune::Phase::Down if definition => {
                    let p = e.pos_at(d.x, d.y);
                    e.mouse_down(d.x, d.y, 1, false);
                    e.requests.push(super::assist::Request::Definition(p));
                }
                rune::Phase::Down => e.mouse_down(d.x, d.y, d.clicks, extend),
                rune::Phase::Move => e.mouse_drag(d.x, d.y),
                rune::Phase::Up => e.mouse_drag(d.x, d.y),
            }
        }
        ws.after_input(tx);
    })
    .on_secondary_press_local(move |ws: &mut Workspace, p: rune::PointerEvent, tx: &mut rune::Tx| {
        ws.focus_editor(&k6);
        if let Some(e) = ws.editor_mut(&k6) {
            e.right_click(p.x, p.y);
        }
        let at = p.window();
        ws.open_editor_menu(at.x, at.y);
        ws.after_input(tx);
    })
    .on_pointer_move_local(move |ws: &mut Workspace, p: rune::PointerEvent, tx: &mut rune::Tx| {
        if let Some(e) = ws.editor_mut(&k4) {
            e.set_gutter_hover(p.x < e.gutter_w());
            // Over the line numbers there is no word to tell about.
            if p.x < e.gutter_w() {
                e.pointer_left();
            } else {
                e.pointer_moved(p.x, p.y);
            }
            if !e.dirty && e.requests.is_empty() {
                tx.live_only();
            }
        }
        ws.after_input(tx);
    })
    .on_hover_change(move |ws: &mut Workspace, inside: bool, tx: &mut rune::Tx| {
        if !inside && let Some(e) = ws.editor_mut(&k5) {
            e.set_gutter_hover(false);
            e.pointer_left();
        }
        ws.after_input(tx);
    })
    .on_wheel(move |ws: &mut Workspace, e: rune::WheelEvent, tx: &mut rune::Tx| {
        if let Some(ed) = ws.editor_mut(&k3) {
            // Shift turns a mouse wheel sideways.
            let (dx, dy) = if e.mods.shift && e.dx == 0.0 { (e.dy, 0.0) } else { (e.dx, e.dy) };
            ed.wheel(dx, dy, e.precise);
        }
        ws.after_input(tx);
        true
    })
}

#[cfg(test)]
mod tests {
    use super::indent_guides;

    #[test]
    fn guides_mark_each_level_and_carry_over_empty_lines() {
        assert_eq!(indent_guides(vec![], "    x", 2), [0, 2]);
        assert_eq!(indent_guides(vec![], "   x", 2), [0, 2], "a part level still gets its guide");
        assert_eq!(indent_guides(vec![0, 2], "", 2), [0, 2], "an empty line keeps the line above's");
        assert_eq!(indent_guides(vec![0, 2], "x", 2), Vec::<usize>::new());
        assert_eq!(indent_guides(vec![], "\tx", 4), [0], "a tab is four columns");
    }
}
