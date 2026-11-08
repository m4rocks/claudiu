//! The text editor editor mode shows files in: one `Editor` per open file. It draws only the lines on screen, each
//! row memoized on what it shows, so a file of tens of thousands of lines types and scrolls like a short one.
//! Children: `buffer` (the text and its undo history), `keys` (what a key does), `element` (drawing and the mouse),
//! `highlight` (syntax colours), `assist` (what a language server adds: the hover card, completion, definitions).

pub mod assist;
pub mod buffer;
mod element;
pub mod highlight;
mod keys;

use std::ops::Range;

use rune::Color;

use crate::panels::glide::Glide;
use crate::terminal::view::{Metrics, metrics_for};
use buffer::{Buffer, Pos, Selection};

/// Spaces a Tab key indents by, and how wide a tab character shows.
pub const TAB_WIDTH: usize = 4;
/// Left of the text: the change marks, then the line numbers, then this gap.
const GUTTER_PAD: f32 = 14.0;
/// Lines kept between the caret and the top or bottom edge when the caret moves.
const MARGIN_LINES: f32 = 3.0;

/// A stretch of a line's text in one colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub bytes: Range<usize>,
    pub color: Color,
}

/// How a line differs from the last commit, drawn as a bar beside its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    Added,
    Modified,
    /// Lines were deleted just above this one.
    Removed,
}

/// A problem a language server reported, underlined in the text.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub from: Pos,
    pub to: Pos,
    pub error: bool,
    pub message: String,
}

/// What the mouse selects while a button is held: characters, or whole words or lines from where it was pressed.
#[derive(Clone, Copy)]
enum Grain {
    Chars,
    Words(Pos, Pos),
    Lines(usize),
}

pub struct Editor {
    pub buffer: Buffer,
    pub sel: Selection,
    /// Up and Down keep the caret in this column, so it comes back after a short line.
    goal: Option<usize>,
    /// Finds this editor again from its handlers.
    pub key: String,
    font_size: f32,
    /// The area the editor was laid out in, in points.
    area: [f32; 2],
    /// How far the text is scrolled, in points: right and down.
    pub scroll: [f32; 2],
    glide: Option<Glide>,
    /// The input method's composition, drawn at the caret until it commits.
    marked: Option<String>,
    grain: Grain,
    /// Colours per line, from the highlighter; a line with none shows in the text colour.
    pub spans: Vec<Vec<Span>>,
    /// The text version `spans` were worked out for, and whether the highlighter is at work.
    pub coloured: Option<u64>,
    pub colouring: bool,
    pub marks: Vec<(usize, Mark)>,
    pub problems: Vec<Problem>,
    /// The caret moved and must be scrolled into view once the editor knows its size.
    reveal: bool,
    /// The caret jumped to a line (go to line, a search hit) and its line goes a third of the way down once the editor
    /// knows its size.
    center: bool,
    /// The view must be built again.
    pub dirty: bool,
    /// The text changed since the owner last looked (it saves, re-highlights and tells a language server).
    pub edited: bool,
    /// Work for the frame (the clipboard); taken after each handler.
    pub fx: Vec<crate::app::rt::Fx>,
    /// What to ask the file's language server, taken after each handler (`assist`).
    pub requests: Vec<assist::Request>,
    /// The word under the pointer, and a count that tells one rest of the pointer from the next.
    hover_word: Option<(Pos, Pos)>,
    hover_serial: u64,
    pub hover: Option<assist::HoverCard>,
    pub completion: Option<assist::Completion>,
}

impl Editor {
    pub fn new(key: impl Into<String>, text: &str) -> Self {
        Self {
            buffer: Buffer::new(text),
            sel: Selection::default(),
            goal: None,
            key: key.into(),
            font_size: 13.0,
            area: [0.0, 0.0],
            scroll: [0.0, 0.0],
            glide: None,
            marked: None,
            grain: Grain::Chars,
            spans: vec![],
            coloured: None,
            colouring: false,
            marks: vec![],
            problems: vec![],
            reveal: false,
            center: false,
            dirty: true,
            edited: false,
            fx: vec![],
            requests: vec![],
            hover_word: None,
            hover_serial: 0,
            hover: None,
            completion: None,
        }
    }

    fn metrics(&self) -> Metrics {
        metrics_for(crate::panels::scale::px(self.font_size))
    }

    /// The text as loaded or reloaded from disk: the history starts over and the caret stays where it was if it can.
    pub fn set_text(&mut self, text: &str) {
        let caret = self.sel.head;
        self.buffer = Buffer::new(text);
        self.sel = Selection::caret(self.buffer.clamp(caret));
        self.edited = true;
        self.dirty = true;
    }

    pub fn caret(&self) -> Pos {
        self.sel.head
    }

    pub fn selected_text(&self) -> String {
        let (a, b) = self.sel.range();
        self.buffer.slice(a, b)
    }

    /// The first and last line (from 0) the selection covers; a selection ending at a line's start leaves that line out.
    pub fn selected_lines(&self) -> (usize, usize) {
        let (a, b) = self.sel.range();
        let last = if b.line > a.line && b.col == 0 { b.line - 1 } else { b.line };
        (a.line, last)
    }

    /// Put the caret at the start of `line` (from 0) and scroll it to a third of the way down.
    pub fn go_to_line(&mut self, line: usize) {
        let at = self.buffer.clamp(Pos::new(line, 0));
        let at = Pos::new(at.line, self.buffer.indent_end(at.line));
        self.select(Selection::caret(at));
        self.glide = None;
        self.center = true;
        self.center_caret();
    }

    /// Scroll the caret's line to a third of the way down, once the editor knows its height.
    fn center_caret(&mut self) {
        if !self.center || self.area[1] <= 0.0 {
            return;
        }
        self.center = false;
        let m = self.metrics();
        self.scroll[1] = (self.sel.head.line as f32 * m.line_h - self.area[1] / 3.0).clamp(0.0, self.max_scroll_y());
        self.dirty = true;
    }

    /// Show `sel`, ending a typing run and forgetting the Up/Down column.
    pub fn select(&mut self, sel: Selection) {
        self.sel = Selection { anchor: self.buffer.clamp(sel.anchor), head: self.buffer.clamp(sel.head) };
        self.goal = None;
        self.buffer.break_typing();
        self.reveal_caret();
        self.dirty = true;
    }

    /// Replace the selection with `text`, as typing (`typing` joins it to the keystrokes before it) or as one step.
    pub fn insert(&mut self, text: &str, typing: bool) {
        let (a, b) = self.sel.range();
        self.replace(a, b, text, typing);
    }

    /// Replace `a..b` with `text` and put the caret after it.
    pub fn replace(&mut self, a: Pos, b: Pos, text: &str, typing: bool) {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let end = self.buffer.replace(a, b, text, self.sel, typing);
        move_colours(&mut self.spans, a, b, end);
        self.sel = Selection::caret(end);
        self.after_edit();
    }

    fn after_edit(&mut self) {
        self.goal = None;
        self.marked = None;
        self.reveal_caret();
        self.dirty = true;
        self.edited = true;
        // Undo and redo don't move colours (`move_colours`), so until the highlighter catches up some lines may keep
        // colours a few letters off; drawing clips them to the line.
        self.spans.truncate(self.buffer.line_count());
    }

    pub fn undo(&mut self) {
        if let Some(sel) = self.buffer.undo() {
            self.sel = Selection { anchor: self.buffer.clamp(sel.anchor), head: self.buffer.clamp(sel.head) };
            self.after_edit();
        }
    }

    pub fn redo(&mut self) {
        if let Some(sel) = self.buffer.redo() {
            self.sel = Selection { anchor: self.buffer.clamp(sel.anchor), head: self.buffer.clamp(sel.head) };
            self.after_edit();
        }
    }

    // ---------------------------------------------------------------------------------------------- geometry

    /// The cell `p` starts at on its line: tabs reach the next multiple of `TAB_WIDTH`, wide characters take two.
    pub fn column(&self, p: Pos) -> usize {
        columns(&self.buffer.line(p.line)[..p.col.min(self.buffer.line(p.line).len())])
    }

    /// The position on `line` nearest cell `col`.
    pub fn at_column(&self, line: usize, col: usize) -> Pos {
        let text = self.buffer.line(line);
        let mut x = 0;
        for (i, c) in text.char_indices() {
            let w = width(c, x);
            // A column inside a wide character or a tab lands on the nearer of its two edges.
            if col <= x + w / 2 {
                return Pos::new(line, i);
            }
            x += w;
        }
        Pos::new(line, text.len())
    }

    /// Where the text starts, right of the gutter.
    pub fn gutter_w(&self) -> f32 {
        let digits = self.buffer.line_count().to_string().len().max(3);
        (self.metrics().cell_w * digits as f32 + GUTTER_PAD * 2.0).round()
    }

    fn max_scroll_y(&self) -> f32 {
        // The last line can scroll up to the top, as in most code editors.
        (self.buffer.line_count().saturating_sub(1) as f32 * self.metrics().line_h).max(0.0)
    }

    fn max_scroll_x(&self) -> f32 {
        let m = self.metrics();
        let widest = (0..self.buffer.line_count()).map(|l| self.buffer.line(l).len()).max().unwrap_or(0);
        (widest as f32 * m.cell_w + m.cell_w * 4.0 - (self.area[0] - self.gutter_w())).max(0.0)
    }

    /// The text position under a point of the editor's area.
    pub fn pos_at(&self, x: f32, y: f32) -> Pos {
        let m = self.metrics();
        let line = ((y + self.scroll[1]) / m.line_h).floor().max(0.0) as usize;
        let line = line.min(self.buffer.line_count() - 1);
        let col = ((x - self.gutter_w() + self.scroll[0]) / m.cell_w).round().max(0.0) as usize;
        self.at_column(line, col)
    }

    /// The editor was laid out at `w` by `h` points.
    pub fn set_area(&mut self, w: f32, h: f32) {
        if self.area != [w, h] {
            self.area = [w, h];
            self.dirty = true;
        }
        self.center_caret();
        if self.reveal && h > 0.0 {
            self.reveal_caret();
        }
    }

    /// Scroll so the caret is on screen with a few lines to spare; before the editor knows its size, once it does.
    fn reveal_caret(&mut self) {
        self.reveal = false;
        if self.area[1] <= 0.0 {
            self.reveal = true;
            return;
        }
        let m = self.metrics();
        let p = self.sel.head;
        let y = p.line as f32 * m.line_h;
        let margin = (m.line_h * MARGIN_LINES).min(self.area[1] / 3.0);
        let top = self.glide.as_ref().map_or(self.scroll[1], |g| g.target);
        let mut to = top;
        if y - margin < top {
            to = y - margin;
        } else if y + m.line_h + margin > top + self.area[1] {
            to = y + m.line_h + margin - self.area[1];
        }
        let to = to.clamp(0.0, self.max_scroll_y());
        if to != top {
            self.glide = None;
            self.scroll[1] = to;
        }
        let x = self.column(p) as f32 * m.cell_w;
        let text_w = self.area[0] - self.gutter_w();
        let pad = m.cell_w * 4.0;
        if x - pad < self.scroll[0] {
            self.scroll[0] = (x - pad).max(0.0);
        } else if x + pad > self.scroll[0] + text_w {
            self.scroll[0] = x + pad - text_w;
        }
        self.dirty = true;
    }

    /// The wheel: `dy`, `dx` points, positive moving the text down and right. A notch eases, a trackpad follows the
    /// fingers.
    pub fn wheel(&mut self, dx: f32, dy: f32, precise: bool) {
        let m = self.metrics();
        if dy != 0.0 {
            if precise {
                self.glide = None;
                self.scroll[1] = (self.scroll[1] - dy).clamp(0.0, self.max_scroll_y());
            } else {
                // A notch reports about 40 points a line; three lines a notch, as the terminal scrolls.
                let by = -(dy / 40.0) * 3.0 * m.line_h;
                let max = self.max_scroll_y();
                Glide::nudge(&mut self.glide, self.scroll[1], by, 0.0, max);
            }
        }
        if dx != 0.0 {
            self.scroll[0] = (self.scroll[0] - dx).clamp(0.0, self.max_scroll_x());
        }
        self.dirty = true;
    }

    /// One frame of an easing scroll; whether another is wanted.
    pub fn tick(&mut self) -> bool {
        let Some(g) = &mut self.glide else { return false };
        let moving = g.step(0.25);
        self.scroll[1] = g.shown;
        if !moving {
            self.glide = None;
        }
        self.dirty = true;
        moving
    }

    // ---------------------------------------------------------------------------------------------- the mouse

    /// A press at `x`, `y`: the `clicks`th of a run selects a character, word or line; `extend` (Shift) keeps the anchor.
    pub fn mouse_down(&mut self, x: f32, y: f32, clicks: u32, extend: bool) {
        let p = self.pos_at(x, y);
        self.completion = None;
        self.hover = None;
        let anchor = if extend { self.sel.anchor } else { p };
        match clicks {
            2 => {
                let (a, b) = self.buffer.word_at(p);
                self.grain = Grain::Words(a, b);
                self.select(Selection { anchor: a, head: b });
            }
            n if n >= 3 => {
                self.grain = Grain::Lines(p.line);
                self.select(self.line_selection(p.line, p.line));
            }
            _ => {
                self.grain = Grain::Chars;
                self.select(Selection { anchor, head: p });
            }
        }
        self.marked = None;
    }

    /// Whole lines `a` to `b`, the one further down taken with its line break.
    fn line_selection(&self, from: usize, to: usize) -> Selection {
        let (a, b) = (from.min(to), from.max(to));
        let end = if b + 1 < self.buffer.line_count() { Pos::new(b + 1, 0) } else { Pos::new(b, self.buffer.line(b).len()) };
        if to < from { Selection { anchor: end, head: Pos::new(a, 0) } } else { Selection { anchor: Pos::new(a, 0), head: end } }
    }

    /// The pointer moved with the button held: the selection follows it in the grain the press chose, and the text
    /// scrolls when it is dragged past an edge.
    pub fn mouse_drag(&mut self, x: f32, y: f32) {
        let m = self.metrics();
        if y < 0.0 {
            self.scroll[1] = (self.scroll[1] + y.max(-m.line_h * 2.0)).max(0.0);
        } else if y > self.area[1] {
            self.scroll[1] = (self.scroll[1] + (y - self.area[1]).min(m.line_h * 2.0)).min(self.max_scroll_y());
        }
        let p = self.pos_at(x, y.clamp(0.0, self.area[1].max(1.0) - 1.0));
        let sel = match self.grain {
            Grain::Chars => Selection { anchor: self.sel.anchor, head: p },
            Grain::Words(a, b) => {
                let (wa, wb) = self.buffer.word_at(p);
                if p < a { Selection { anchor: b, head: wa } } else { Selection { anchor: a, head: wb.max(b) } }
            }
            Grain::Lines(line) => self.line_selection(line, p.line),
        };
        self.sel = sel;
        self.goal = None;
        self.dirty = true;
    }

    // ---------------------------------------------------------------------------------------------- the input method

    pub fn ime(&mut self, ime: &rune::Ime) -> bool {
        match ime {
            rune::Ime::Preedit(text, _) => {
                self.marked = (!text.is_empty()).then(|| text.clone());
                self.dirty = true;
                true
            }
            rune::Ime::Commit(text) => {
                self.marked = None;
                if !text.is_empty() {
                    self.insert(text, true);
                }
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// Text from the clipboard, with a pasted file's `\r\n` line ends made `\n`.
    pub fn paste_text(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.insert(&text, false);
    }
}

/// Keep the colours of text an edit moved (`a..b` replaced, the new text ending at `end`) with that text until the
/// highlighter catches up: what was before `a` stays, what was after `b` moves to after `end`, a stretch the edit fell
/// inside on one line grows or shrinks with it, and the new text has none.
fn move_colours(spans: &mut Vec<Vec<Span>>, a: Pos, b: Pos, end: Pos) {
    if a.line >= spans.len() {
        return;
    }
    let none = vec![];
    let after = spans.get(b.line).unwrap_or(&none);
    let shift = |s: &Span| Span { bytes: s.bytes.start - b.col + end.col..s.bytes.end - b.col + end.col, color: s.color };
    let mut first: Vec<Span> = vec![];
    let mut last: Vec<Span> = vec![];
    for s in &spans[a.line] {
        if s.bytes.end <= a.col {
            first.push(s.clone());
        } else if a.line == b.line && end.line == a.line && s.bytes.start < a.col && s.bytes.end >= b.col {
            // Typing inside a word keeps it coloured.
            first.push(Span { bytes: s.bytes.start..s.bytes.end - b.col + end.col, color: s.color });
        }
    }
    for s in after.iter().filter(|s| s.bytes.start >= b.col) {
        last.push(shift(s));
    }
    let mut lines = vec![vec![]; end.line - a.line + 1];
    if end.line == a.line {
        first.extend(last);
        lines[0] = first;
    } else {
        lines[0] = first;
        lines[end.line - a.line] = last;
    }
    let old_end = (b.line + 1).min(spans.len());
    spans.splice(a.line..old_end, lines);
}

/// The cells `text` takes from the start of a line.
fn columns(text: &str) -> usize {
    text.chars().fold(0, |x, c| x + width(c, x))
}

/// The cells `c` takes when it starts at cell `x`.
fn width(c: char, x: usize) -> usize {
    if c == '\t' { TAB_WIDTH - x % TAB_WIDTH } else { unicode_width::UnicodeWidthChar::width(c).unwrap_or(0).max(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_count_tabs_to_the_next_stop_and_wide_characters_twice() {
        let e = Editor::new("e", "a\tb\n日本x");
        assert_eq!(e.column(Pos::new(0, 1)), 1);
        assert_eq!(e.column(Pos::new(0, 2)), 4, "the tab reaches column 4");
        assert_eq!(e.column(Pos::new(1, 6)), 4, "two wide characters");
        assert_eq!(e.at_column(1, 4), Pos::new(1, 6));
        assert_eq!(e.at_column(1, 1), Pos::new(1, 0), "the near half of a wide character");
        assert_eq!(e.at_column(1, 2), Pos::new(1, 3));
        assert_eq!(e.at_column(0, 99), Pos::new(0, 3), "past the end is the end");
    }

    #[test]
    fn colours_move_with_the_text_an_edit_moves() {
        let c = |r: std::ops::Range<usize>| Span { bytes: r, color: Color::WHITE };
        let mut e = Editor::new("e", "let word = 1;\nnext");
        e.spans = vec![vec![c(0..3), c(4..8), c(11..12)], vec![c(0..4)]];
        // Typing inside `word` keeps it coloured and moves what follows.
        e.replace(Pos::new(0, 6), Pos::new(0, 6), "xx", true);
        assert_eq!(e.spans, [vec![c(0..3), c(4..10), c(13..14)], vec![c(0..4)]]);
        // A line break before `= 1;` takes its colours to the new line.
        e.replace(Pos::new(0, 10), Pos::new(0, 10), "\n", false);
        assert_eq!(e.spans, [vec![c(0..3), c(4..10)], vec![c(3..4)], vec![c(0..4)]]);
        // Joining the lines again brings them back.
        e.replace(Pos::new(0, 10), Pos::new(1, 0), "", false);
        assert_eq!(e.spans, [vec![c(0..3), c(4..10), c(13..14)], vec![c(0..4)]]);
        assert_eq!(e.buffer.text(), "let woxxrd = 1;\nnext");
    }

    #[test]
    fn a_selection_ending_at_a_line_start_leaves_that_line_out() {
        let mut e = Editor::new("e", "a\nb\nc");
        e.select(Selection { anchor: Pos::new(0, 0), head: Pos::new(2, 0) });
        assert_eq!(e.selected_lines(), (0, 1));
        e.select(Selection { anchor: Pos::new(2, 1), head: Pos::new(1, 0) });
        assert_eq!(e.selected_lines(), (1, 2));
        assert_eq!(e.selected_text(), "b\nc");
    }

    #[test]
    fn dragging_after_a_triple_click_selects_whole_lines_either_way() {
        let mut e = Editor::new("e", "one\ntwo\nthree");
        e.area = [400.0, 400.0];
        let line_h = e.metrics().line_h;
        e.mouse_down(60.0, line_h * 1.5, 3, false);
        assert_eq!(e.selected_text(), "two\n");
        e.mouse_drag(60.0, line_h * 2.5);
        assert_eq!(e.selected_text(), "two\nthree");
        e.mouse_drag(60.0, line_h * 0.5);
        assert_eq!(e.selected_text(), "one\ntwo\n");
    }

    #[test]
    fn a_double_click_selects_the_word_and_dragging_grows_by_words() {
        let mut e = Editor::new("e", "alpha beta gamma");
        e.area = [800.0, 400.0];
        let (gutter, cell) = (e.gutter_w(), e.metrics().cell_w);
        e.mouse_down(gutter + cell * 7.0, 4.0, 2, false);
        assert_eq!(e.selected_text(), "beta");
        e.mouse_drag(gutter + cell * 13.0, 4.0);
        assert_eq!(e.selected_text(), "beta gamma");
        e.mouse_drag(gutter + cell * 1.0, 4.0);
        assert_eq!(e.selected_text(), "alpha beta");
    }
}
