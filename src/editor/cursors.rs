//! Several cursors, as main's GPUI editor had them (gpui-kit's multi-selection): Alt+click adds one, and Cmd+Alt+Up /
//! Down on macOS (Ctrl+Alt elsewhere; Linux keeps Shift+Alt for duplicating lines, as main did) add one a row above or
//! below each. Typing, deleting, Enter, Tab and every caret move then act at each cursor, as one undo step that restores
//! them all. Escape, a plain click or any jump (go to line, a search step) leaves only the main cursor, the first one.
//!
//! Alt+Shift+drag selects a block, one selection per row; so does dragging on after an Alt+click.
//!
//! An edit runs once per cursor, from the last in the text to the first, through the same code as a single cursor;
//! `replace` moves the cursors not being edited along with the text.

use super::Editor;
use super::buffer::{Pos, Selection};

/// A cursor besides the main one: its selection, the cell Up and Down keep it in, and whether it shows at the end of
/// the row above a wrap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cursor {
    pub sel: Selection,
    goal: Option<usize>,
    row_end: bool,
}

impl Cursor {
    fn at(sel: Selection) -> Self {
        Self { sel, goal: None, row_end: false }
    }
}

impl Editor {
    /// Every cursor's selection, the main one first.
    pub fn cursors(&self) -> Vec<Selection> {
        std::iter::once(self.sel).chain(self.others.iter().map(|c| c.sel)).collect()
    }

    /// Show `cursors`, the first as the main one (undo and redo bring back the cursors an edit started or ended with).
    pub fn set_cursors(&mut self, cursors: &[Selection]) {
        let clamp = |s: &Selection| Selection { anchor: self.buffer.clamp(s.anchor), head: self.buffer.clamp(s.head) };
        let mut all = cursors.iter().map(clamp);
        self.sel = all.next().unwrap_or_default();
        self.others = all.map(Cursor::at).collect();
    }

    /// Run `op`, which acts at the main cursor, at every cursor instead: from the last in the text to the first, as
    /// one undo step. Cursors that end up on the same place or overlapping become one.
    pub(super) fn each_cursor(&mut self, mut op: impl FnMut(&mut Self)) {
        if self.others.is_empty() {
            return op(self);
        }
        let main = Cursor { sel: self.sel, goal: self.goal, row_end: self.row_end };
        let mut all: Vec<Cursor> = std::iter::once(main).chain(std::mem::take(&mut self.others)).collect();
        self.buffer.begin_group(all.iter().map(|c| c.sel).collect());
        let mut order: Vec<usize> = (0..all.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(all[i].sel.range().0));
        self.at_each = true;
        for i in order {
            (self.sel, self.goal, self.row_end) = (all[i].sel, all[i].goal, all[i].row_end);
            self.others = all.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, c)| *c).collect();
            op(self);
            let mut rest = std::mem::take(&mut self.others).into_iter();
            let done = Cursor { sel: self.sel, goal: self.goal, row_end: self.row_end };
            all = (0..all.len()).map(|j| if j == i { done } else { rest.next().expect("one cursor per place") }).collect();
        }
        self.at_each = false;
        let mut all = merge(all).into_iter();
        let main = all.next().expect("the main cursor stays");
        (self.sel, self.goal, self.row_end) = (main.sel, main.goal, main.row_end);
        self.others = all.collect();
        self.buffer.end_group(self.cursors());
        self.reveal_caret();
        self.dirty = true;
    }

    /// Move the cursors not being edited with an edit that replaced `a..b` and ended at `end`: one inside the replaced
    /// text goes to its end.
    pub(super) fn move_cursors(&mut self, a: Pos, b: Pos, end: Pos) {
        let moved = |p: Pos| {
            if p <= a {
                p
            } else if p < b {
                end
            } else if p.line == b.line {
                Pos::new(end.line, p.col - b.col + end.col)
            } else {
                Pos::new(p.line - b.line + end.line, p.col)
            }
        };
        for c in &mut self.others {
            c.sel = Selection { anchor: moved(c.sel.anchor), head: moved(c.sel.head) };
        }
    }

    /// A cursor a row above (`rows` -1) or below (1) each cursor, in the same cell, as gpui-kit's
    /// `add_cursor_vertical`: none where a cursor would not move or one is already there. The newest is scrolled to.
    pub fn add_cursor_vertical(&mut self, rows: isize) {
        self.buffer.break_typing();
        let main = (self.sel, self.goal, self.row_end);
        let mut taken: Vec<Pos> = self.cursors().iter().map(|s| s.head).collect();
        let sources: Vec<Cursor> = std::iter::once(Cursor { sel: main.0, goal: main.1, row_end: main.2 }).chain(self.others.iter().copied()).collect();
        let mut newest = None;
        for c in sources {
            (self.sel, self.row_end) = (c.sel, c.row_end);
            let goal = c.goal.unwrap_or_else(|| self.caret_cell());
            let Some(row) = self.caret_row().checked_add_signed(rows).filter(|&r| r < self.row_count()) else { continue };
            let (target, row_end) = self.at_row_cell(row, goal);
            if taken.contains(&target) {
                continue;
            }
            taken.push(target);
            self.others.push(Cursor { sel: Selection::caret(target), goal: Some(goal), row_end });
            newest = Some(target);
        }
        (self.sel, self.goal, self.row_end) = main;
        if let Some(p) = newest {
            self.reveal_pos(p);
        }
        self.wake_caret();
        self.dirty = true;
    }

    /// Alt+click: another cursor at `p`, unless `p` is inside a selection or on a cursor already (gpui-kit's
    /// `add_cursor_at`).
    pub fn add_cursor_at(&mut self, p: Pos) {
        let p = self.buffer.clamp(p);
        let inside = |s: &Selection| {
            let (a, b) = s.range();
            if s.is_empty() { p == a } else { a <= p && p <= b }
        };
        if self.cursors().iter().any(inside) {
            return;
        }
        self.buffer.break_typing();
        self.others.push(Cursor::at(Selection::caret(p)));
        self.wake_caret();
        self.dirty = true;
    }

    /// The carets of the cursors besides the main one, and whether each shows at the end of the row above a wrap.
    pub(super) fn other_heads(&self) -> impl Iterator<Item = (Pos, bool)> + '_ {
        self.others.iter().map(|c| (c.sel.head, c.row_end))
    }

    /// Alt+Shift+press (Ctrl+Alt also on Linux): the caret goes there, and dragging selects a block from there
    /// (`drag_block`).
    pub fn start_block(&mut self, x: f32, y: f32) {
        let p = self.pos_at(x, y);
        self.select(Selection::caret(p));
        self.block_from = Some(self.row_cell_at(x, y));
    }

    /// Alt+click: dragging on from here selects a block.
    pub fn start_block_here(&mut self, x: f32, y: f32) {
        self.block_from = Some(self.row_cell_at(x, y));
    }

    /// Dragging after an Alt+press: one selection per row from the press to the pointer, over the same cells, as
    /// gpui-kit's `build_columnar_selection`. Cells past a short row's end still count, so the block keeps its width;
    /// each row selects what it has of it.
    pub fn drag_block(&mut self, x: f32, y: f32) {
        let Some((row0, cell0)) = self.block_from else { return };
        let (row1, cell1) = self.row_cell_at(x, y);
        // A press that hasn't moved yet keeps the cursor an Alt+click added.
        if (row1, cell1) == (row0, cell0) {
            return;
        }
        let (top, bottom) = (row0.min(row1), row0.max(row1));
        let (left, right) = (cell0.min(cell1), cell0.max(cell1));
        let rows: Vec<Selection> = (top..=bottom)
            .map(|row| Selection { anchor: self.at_row_cell(row, left).0, head: self.at_row_cell(row, right).0 })
            .collect();
        self.buffer.break_typing();
        self.set_cursors(&rows);
        self.goal = None;
        self.wake_caret();
        self.dirty = true;
    }

    /// Escape with several cursors: only the main one stays. Whether there were others.
    pub fn single_cursor(&mut self) -> bool {
        let had = !self.others.is_empty();
        self.others.clear();
        self.dirty |= had;
        had
    }
}

/// Cursors in place, the main one first, with those that overlap or meet at one caret made one. A merged cursor that
/// took in the main one is the main one.
fn merge(all: Vec<Cursor>) -> Vec<Cursor> {
    let mut sorted: Vec<(bool, Cursor)> = all.into_iter().enumerate().map(|(i, c)| (i == 0, c)).collect();
    sorted.sort_by_key(|(_, c)| c.sel.range());
    let mut out: Vec<(bool, Cursor)> = vec![];
    for (main, c) in sorted {
        if let Some((last_main, last)) = out.last_mut() {
            let (la, lb) = last.sel.range();
            let (a, b) = c.sel.range();
            if a < lb || a == la || (a == lb && (c.sel.is_empty() || last.sel.is_empty())) {
                let (from, to) = (la.min(a), lb.max(b));
                let forward = if main { c.sel.head >= c.sel.anchor } else { last.sel.head >= last.sel.anchor };
                last.sel = if forward { Selection { anchor: from, head: to } } else { Selection { anchor: to, head: from } };
                if main {
                    (last.goal, last.row_end) = (c.goal, c.row_end);
                }
                *last_main |= main;
                continue;
            }
        }
        out.push((main, c));
    }
    let main = out.iter().position(|(m, _)| *m).unwrap_or(0);
    let first = out.remove(main).1;
    std::iter::once(first).chain(out.into_iter().map(|(_, c)| c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gpui-kit's test notation: `|` marks each cursor, the first the main one.
    fn editor(spec: &str) -> Editor {
        let text = spec.replace('|', "");
        let mut e = Editor::new("e", &text);
        let cursors: Vec<Selection> = places(spec).into_iter().map(Selection::caret).collect();
        e.set_cursors(&cursors);
        e
    }

    fn places(spec: &str) -> Vec<Pos> {
        let mut out = vec![];
        let (mut line, mut col) = (0, 0);
        for c in spec.chars() {
            match c {
                '|' => out.push(Pos::new(line, col)),
                '\n' => (line, col) = (line + 1, 0),
                c => col += c.len_utf8(),
            }
        }
        out
    }

    /// The text with `|` at each cursor, as gpui-kit's `assert_cursors` compares them (in text order).
    fn shown(e: &Editor) -> String {
        let mut heads: Vec<Pos> = e.cursors().iter().map(|s| s.head).collect();
        heads.sort();
        let mut out = String::new();
        for line in 0..e.buffer.line_count() {
            if line > 0 {
                out.push('\n');
            }
            let text = e.buffer.line(line);
            let mut at = 0;
            for h in heads.iter().filter(|h| h.line == line) {
                out.push_str(&text[at..h.col]);
                out.push('|');
                at = h.col;
            }
            out.push_str(&text[at..]);
        }
        out
    }

    fn type_text(e: &mut Editor, text: &str) {
        e.each_cursor(|e| e.insert(text, true));
    }

    // Each test below is gpui-kit 0.7.1's of the same name (src/input/base/state.rs), with its cursors and expected text.

    #[test]
    fn multi_cursor_insert_text() {
        let mut e = editor("|hello |world|");
        type_text(&mut e, ">>>");
        assert_eq!(shown(&e), ">>>|hello >>>|world>>>|");
    }

    #[test]
    fn multi_cursor_delete_backward() {
        let mut e = editor("|islands| cars|");
        e.each_cursor(|e| e.backspace(false, false));
        assert_eq!(shown(&e), "|island| car|", "the first cursor has nothing to delete");
    }

    #[test]
    fn multi_cursor_delete_forward_merges() {
        let mut e = editor("hello| |world");
        e.each_cursor(|e| e.delete_forward(false, false));
        assert_eq!(shown(&e), "hello|orld");
        assert!(e.others.is_empty(), "the two cursors met and became one");
    }

    #[test]
    fn multi_cursor_multiline_insert_and_delete_is_one_undo_step() {
        let mut e = editor("|1\n|2\n|3");
        type_text(&mut e, "a");
        assert_eq!(shown(&e), "a|1\na|2\na|3");
        e.each_cursor(|e| e.backspace(false, false));
        assert_eq!(shown(&e), "|1\n|2\n|3");
    }

    #[test]
    fn add_cursor_below_preserves_column() {
        let mut e = editor("ab|cd\nabcd");
        e.add_cursor_vertical(1);
        assert_eq!(shown(&e), "ab|cd\nab|cd");
        e.add_cursor_vertical(1);
        assert_eq!(e.others.len(), 1, "nothing below the last row");
    }

    #[test]
    fn add_cursor_at_rejects_duplicates() {
        let mut e = editor("he|llo");
        e.add_cursor_at(Pos::new(0, 2));
        assert!(e.others.is_empty());
        e.add_cursor_at(Pos::new(0, 4));
        assert_eq!(e.others.len(), 1);
    }

    #[test]
    fn multi_cursor_undo_redo_restores_selections() {
        let mut e = editor("abc123|\nabc12345|\nabc1234567|");
        type_text(&mut e, "a");
        assert_eq!(shown(&e), "abc123a|\nabc12345a|\nabc1234567a|");
        e.undo();
        assert_eq!(shown(&e), "abc123|\nabc12345|\nabc1234567|");
        e.redo();
        assert_eq!(shown(&e), "abc123a|\nabc12345a|\nabc1234567a|");
    }

    #[test]
    fn multi_cursor_undo_multiple_inserts() {
        let mut e = editor("|1\n|2\n|3");
        for c in ["a", "b", "c"] {
            type_text(&mut e, c);
        }
        assert_eq!(shown(&e), "abc|1\nabc|2\nabc|3");
        e.undo();
        assert_eq!(shown(&e), "|1\n|2\n|3", "the keystrokes were one typing run");
        e.redo();
        assert_eq!(shown(&e), "abc|1\nabc|2\nabc|3");
    }

    #[test]
    fn multi_cursor_backspace_run_is_one_undo() {
        let mut e = editor("abc|1\nabc|2\nabc|3");
        for _ in 0..3 {
            e.each_cursor(|e| e.backspace(false, false));
        }
        assert_eq!(shown(&e), "|1\n|2\n|3");
        e.undo();
        assert_eq!(shown(&e), "abc|1\nabc|2\nabc|3");
    }

    #[test]
    fn adding_a_cursor_splits_the_typing_gesture() {
        let mut e = editor("|1\n2\n3");
        type_text(&mut e, "a");
        e.add_cursor_vertical(1);
        type_text(&mut e, "b");
        assert_eq!(shown(&e), "ab|1\n2b|\n3");
        e.undo();
        assert_eq!(shown(&e), "a|1\n2|\n3");
    }

    #[test]
    fn multi_cursor_indent_then_outdent_roundtrips() {
        let mut e = editor("|1\n|2\n|3");
        e.each_cursor(|e| e.tab());
        assert_eq!(shown(&e), "  |1\n  |2\n  |3");
        e.each_cursor(|e| e.outdent());
        assert_eq!(shown(&e), "|1\n|2\n|3");
        e.each_cursor(|e| e.tab());
        e.undo();
        assert_eq!(shown(&e), "|1\n|2\n|3", "the indent at every cursor is one undo step");
    }

    #[test]
    fn multi_cursor_edit_preserves_the_active_cursor() {
        let mut e = editor("x|\n|y");
        e.set_cursors(&[Selection::caret(Pos::new(1, 0)), Selection::caret(Pos::new(0, 1))]);
        type_text(&mut e, "!");
        assert_eq!(e.sel, Selection::caret(Pos::new(1, 1)), "the main cursor stays the main one");
    }

    #[test]
    fn a_block_selects_the_same_cells_on_every_row_and_what_a_short_row_has() {
        let mut e = editor("|abcdef\nab\nabcdef");
        e.area = [800.0, 400.0];
        let (m, gutter) = (e.metrics(), e.gutter_w());
        let at = |row: f32, cell: f32| (gutter + m.cell_w * cell, m.line_h * (row + 0.5));
        let (x, y) = at(0.0, 1.0);
        e.start_block(x, y);
        let (x, y) = at(2.0, 4.0);
        e.drag_block(x, y);
        let texts: Vec<String> = e.cursors().iter().map(|s| e.buffer.slice(s.range().0, s.range().1)).collect();
        assert_eq!(texts, ["bcd", "b", "bcd"], "the short row selects what it has of the block");
        e.each_cursor(|e| e.insert("X", true));
        assert_eq!(e.buffer.text(), "aXef\naX\naXef");
    }

    #[test]
    fn alt_drag_extends_upward_from_an_existing_cursor() {
        let mut e = editor("abcd\nabcd\na|bcd");
        e.area = [800.0, 400.0];
        let (m, gutter) = (e.metrics(), e.gutter_w());
        let x = gutter + m.cell_w;
        // The Alt+press lands on the cursor already there, so it adds none, but dragging still makes a block.
        e.add_cursor_at(e.pos_at(x, m.line_h * 2.5));
        e.start_block_here(x, m.line_h * 2.5);
        e.drag_block(x, m.line_h * 0.5);
        assert_eq!(shown(&e), "a|bcd\na|bcd\na|bcd");
    }

    #[test]
    fn alt_drag_over_a_short_row_keeps_the_block_width() {
        let mut e = editor("|abcdef\nab\nabcdef");
        e.area = [800.0, 400.0];
        let (m, gutter) = (e.metrics(), e.gutter_w());
        e.start_block(gutter + m.cell_w * 1.0, m.line_h * 0.5);
        e.drag_block(gutter + m.cell_w * 5.0, m.line_h * 1.5);
        let ranges: Vec<(Pos, Pos)> = e.cursors().iter().map(|s| s.range()).collect();
        assert_eq!(ranges, [(Pos::new(0, 1), Pos::new(0, 5)), (Pos::new(1, 1), Pos::new(1, 2))], "the short row is clipped to its end");
    }

    #[test]
    fn multi_cursor_actions_reveal_hidden_carets() {
        let mut e = editor("ab\na|b\nab");
        let actions: [fn(&mut Editor); 6] = [
            |e| e.add_cursor_vertical(-1),
            |e| e.add_cursor_vertical(1),
            |e| e.each_cursor(|e| {
                e.vertical(-1, true);
            }),
            |e| e.each_cursor(|e| {
                e.vertical(1, true);
            }),
            |e| e.each_cursor(|e| e.insert("x", true)),
            |e| e.each_cursor(|e| e.backspace(false, false)),
        ];
        for (i, act) in actions.iter().enumerate() {
            let later = std::time::Instant::now() + std::time::Duration::from_millis(400);
            e.blink(later, true);
            assert!(!e.caret_shown(), "action {i} starts with the caret off");
            act(&mut e);
            assert!(e.caret_shown(), "action {i} shows the caret");
        }
    }

    // The next five are tests main's vendored gpui-base added (vendor/gpui-base/src/input/base/state.rs on main).

    #[test]
    fn multi_cursor_escape_collapses() {
        let mut e = editor("|a\n|b\n|c");
        assert!(e.single_cursor());
        assert_eq!(e.cursors().len(), 1);
    }

    #[test]
    fn multi_cursor_paste_distributes_lines() {
        let mut e = editor("|1\n|2\n|3");
        e.paste_text("x\ny\nz");
        assert_eq!(shown(&e), "x|1\ny|2\nz|3", "one clipboard line per cursor");
    }

    #[test]
    fn multi_cursor_selection_commands() {
        let mut e = editor("on|e two three\none t|wo three\non|e two three");
        e.each_cursor(|e| {
            let to = e.home(e.sel.head);
            e.select(Selection { anchor: e.sel.anchor, head: to });
        });
        assert_eq!(shown(&e), "|one two three\n|one two three\n|one two three");
        // Selecting to the start of the text, outside the per-cursor loop, leaves the main cursor only.
        let mut e = editor("on|e two three\none t|wo three\non|e two three");
        e.select(Selection { anchor: e.sel.anchor, head: Pos::default() });
        assert_eq!(e.cursors().iter().map(|s| s.head).collect::<Vec<_>>(), [Pos::default()]);
    }

    #[test]
    fn multi_cursor_replace_selection() {
        let mut e = editor("|a\n|b\n|c");
        e.each_cursor(|e| {
            let to = e.next_visible(e.sel.head);
            e.select(Selection { anchor: e.sel.anchor, head: to });
        });
        let ranges: Vec<(Pos, Pos)> = e.cursors().iter().map(|s| s.range()).collect();
        assert_eq!(ranges, [(Pos::new(0, 0), Pos::new(0, 1)), (Pos::new(1, 0), Pos::new(1, 1)), (Pos::new(2, 0), Pos::new(2, 1))]);
        type_text(&mut e, "x");
        assert_eq!(shown(&e), "x|\nx|\nx|");
    }

    #[test]
    fn alt_drag_selects_a_block_and_replaces_each_row() {
        for shift in [false, true] {
            let mut e = editor("|abcd\nabcd\nabcd");
            e.area = [800.0, 400.0];
            let (m, gutter) = (e.metrics(), e.gutter_w());
            let at = |row: f32, cell: f32| (gutter + m.cell_w * cell, m.line_h * (row + 0.5));
            let (x, y) = at(0.0, 1.0);
            if shift {
                e.start_block(x, y);
            } else {
                e.add_cursor_at(e.pos_at(x, y));
                e.start_block_here(x, y);
            }
            let (x, y) = at(2.0, 3.0);
            e.drag_block(x, y);
            type_text(&mut e, "X");
            assert_eq!(shown(&e), "aX|d\naX|d\naX|d", "shift {shift}");
        }
    }

    #[test]
    fn multi_cursor_word_movement() {
        let mut e = editor("on|e two three\none t|wo three\non|e two three");
        e.each_cursor(|e| {
            let to = e.buffer.word_right(e.sel.head);
            e.select(Selection::caret(to));
        });
        assert_eq!(shown(&e), "one| two three\none two| three\none| two three");
        e.each_cursor(|e| {
            let to = e.buffer.word_left(e.sel.head);
            e.select(Selection::caret(to));
        });
        assert_eq!(shown(&e), "|one two three\none |two three\n|one two three");
        // A jump outside the per-cursor loop, as to the end of the file, leaves one cursor.
        e.select(Selection::caret(e.buffer.last()));
        assert!(e.others.is_empty());
    }
}
