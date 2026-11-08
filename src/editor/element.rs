//! The editor as Rune elements: the line numbers and change marks, then the text. Only the lines on screen are built,
//! and each is memoized on what it shows, so typing rebuilds one line and scrolling builds the few that came in.

use std::hash::{Hash, Hasher};

use rune::{Color, Cursor, El, Family, TextStyle, el, text};

use super::{Editor, Mark, Span, columns};
use crate::app::Workspace;
use crate::panels::theme;
use crate::terminal::view::Metrics;

/// The editor's font at `size` points.
fn font(size: f32) -> TextStyle {
    TextStyle::new(size, 400.0).family(Family::Named(crate::platform::monospace_font()))
}

/// What one line of text paints, in cells from the line's start.
struct LinePaint {
    text: String,
    spans: Vec<Span>,
    /// The selected cells, when the selection touches this line (a trailing cell shows its line break).
    selected: Option<(usize, usize)>,
    /// Underlined problems: from and to cell, and whether it is an error.
    problems: Vec<(usize, usize, bool)>,
}

impl LinePaint {
    fn version(&self, m: Metrics) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (m.cell_w.to_bits(), m.line_h.to_bits(), &self.text, self.selected, &self.problems).hash(&mut h);
        for s in &self.spans {
            (&s.bytes, s.color).hash(&mut h);
        }
        h.finish()
    }

    fn build(self, m: Metrics, size: f32) -> El {
        let mut row = el().w_full().h(m.line_h);
        let x = |cell: usize| (m.cell_w * cell as f32).round();
        if let Some((a, b)) = self.selected {
            row = row.child(el().absolute(x(a), 0.0).size(x(b) - x(a), m.line_h).fill(theme::rgb(theme::SELECTION)));
        }
        // The text in runs of one colour, each placed at its cell so tabs and wide characters line up.
        let text_color = theme::rgb(theme::TEXT);
        let mut at = 0;
        let mut runs: Vec<(usize, &str, Color)> = vec![];
        for s in &self.spans {
            let (from, to) = (s.bytes.start.min(self.text.len()), s.bytes.end.min(self.text.len()));
            if from < at || !self.text.is_char_boundary(from) || !self.text.is_char_boundary(to) {
                continue;
            }
            if at < from {
                runs.push((at, &self.text[at..from], text_color));
            }
            runs.push((from, &self.text[from..to], s.color));
            at = to;
        }
        if at < self.text.len() {
            runs.push((at, &self.text[at..], text_color));
        }
        for (start, part, color) in runs {
            if part.trim().is_empty() {
                continue;
            }
            let cell = columns(&self.text[..start]);
            // Tabs inside a run are drawn as the spaces they stand for.
            let shown = if part.contains('\t') { expand_tabs(part, cell) } else { part.to_string() };
            row = row.child(text(shown).font(font(size).lh(m.line_h)).color(color).absolute(x(cell), 0.0));
        }
        for (a, b, error) in self.problems {
            let color = theme::rgb(if error { theme::DANGER } else { theme::WARN });
            row = row.child(el().absolute(x(a), m.line_h - 2.0).size((x(b) - x(a)).max(m.cell_w), 1.5).fill(color));
        }
        row
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
        let first = ((self.scroll[1] / m.line_h).floor().max(0.0) as usize).min(lines - 1);
        let shown = (self.area[1] / m.line_h).ceil() as usize + 1;
        let last = (first + shown).min(lines);
        // A whole number of device pixels, so text and its backgrounds move together.
        let top = crate::panels::glide::snap(first as f32 * m.line_h - self.scroll[1], scale);
        let (sel_a, sel_b) = self.sel.range();
        let caret = self.sel.head;

        let mut numbers = el().absolute(0.0, top).w(gutter).h_full();
        let mut rows = el().absolute(-self.scroll[0].round(), top).w_full();
        for line in first..last {
            let y = m.line_h * (line - first) as f32;
            let number_color = if line == caret.line { theme::TEXT_DIM } else { theme::TEXT_FAINT };
            numbers = numbers.child(
                el().absolute(0.0, y)
                    .w(gutter - super::GUTTER_PAD)
                    .h(m.line_h)
                    .justify_end()
                    .child(text((line + 1).to_string()).font(font(size).lh(m.line_h)).color(theme::rgb(number_color))),
            );
            if let Ok(i) = self.marks.binary_search_by_key(&line, |(l, _)| *l) {
                let mark = &self.marks[i].1;
                numbers = numbers.child(mark_bar(*mark, y, m));
            }
            let text_ = self.buffer.line(line);
            let selected = (!self.sel.is_empty() && sel_a.line <= line && line <= sel_b.line).then(|| {
                let a = if line == sel_a.line { columns(&text_[..sel_a.col]) } else { 0 };
                let b = if line == sel_b.line { columns(&text_[..sel_b.col]) } else { columns(text_) + 1 };
                (a, b)
            });
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
            let paint = LinePaint { text: text_.to_string(), spans: self.spans.get(line).cloned().unwrap_or_default(), selected, problems };
            let version = paint.version(m);
            rows = rows.child(el().absolute(0.0, y).w_full().child(rune::memo(format!("{key}/l{line}"), version, move || paint.build(m, size))));
        }

        let mut body = el().absolute(gutter, 0.0).size((self.area[0] - gutter).max(0.0), self.area[1]).clip_rounded(0.0);
        if focused && self.sel.is_empty() && (first..last).contains(&caret.line) {
            let y = top + m.line_h * (caret.line - first) as f32;
            body = body.child(el().absolute(0.0, y).w_full().h(m.line_h).fill(theme::rgb(theme::ROW_ACTIVE)));
        }
        body = body.child(rows);
        if (first..last).contains(&caret.line) {
            let x = (self.column(caret) as f32 * m.cell_w - self.scroll[0]).round();
            let y = top + m.line_h * (caret.line - first) as f32;
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
            let bar = el().absolute(x - 1.0, y).size(2.0, m.line_h);
            body = body.child(if focused { bar.fill(theme::rgb(theme::ACCENT)).ime_cursor() } else { bar });
        }

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
    let (k1, k2, k3) = (key.clone(), key.clone(), key);
    root.on_layout(move |ws: &mut Workspace, r: [f32; 4]| {
        if let Some(e) = ws.editor_mut(&k1) {
            e.set_area(r[2], r[3]);
        }
    })
    .on_drag(move |ws: &mut Workspace, d: &mut rune::Drag, tx: &mut rune::Tx| {
        let extend = tx.mods().shift;
        ws.focus_editor(&k2);
        if let Some(e) = ws.editor_mut(&k2) {
            match d.phase {
                rune::Phase::Down => e.mouse_down(d.x, d.y, d.clicks, extend),
                rune::Phase::Move => e.mouse_drag(d.x, d.y),
                rune::Phase::Up => e.mouse_drag(d.x, d.y),
            }
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
