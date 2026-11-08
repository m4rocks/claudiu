//! The rows of the screen each line takes, as main's GPUI editor laid them out (gpui-kit's wrap and fold maps). A line
//! wider than the text area wraps at the last space that fits, or mid-word when no space does, and its continuation
//! rows start as far in as the line's own leading spaces. A fold hides every line strictly between its first and last;
//! its candidates are the blocks tree-sitter finds (`fold_spots`). `starts` holds the first row of each line, so a
//! hidden line takes none and a row finds its line by a binary search.

use std::ops::Range;

use super::buffer::{Buffer, Pos};
use super::{Editor, columns, width};

/// gpui's cap on how far a continuation row is indented, in cells.
const MAX_INDENT: usize = 256;

/// The rows the lines take: where each line breaks, how far its continuation rows are indented, and the first row
/// of each line.
#[derive(Default)]
pub(super) struct Rows {
    /// Cells a row holds; 0 before the editor knows its width, when nothing wraps.
    width: usize,
    /// Per line, the byte offsets its continuation rows start at; empty for a line on one row.
    breaks: Vec<Vec<usize>>,
    /// Per line, the cells its continuation rows are indented by.
    indents: Vec<usize>,
    /// The first row of each line, then the row count.
    starts: Vec<usize>,
}

impl Rows {
    /// Wrap lines `from..old_end` again as the text's lines `from..new_end`, and count the rows anew.
    fn rewrap(&mut self, buffer: &Buffer, from: usize, old_end: usize, new_end: usize, folds: &[(usize, usize)]) {
        let old_end = old_end.min(self.breaks.len());
        let from = from.min(old_end);
        let (breaks, indents): (Vec<_>, Vec<_>) = (from..new_end).map(|l| wrap(buffer.line(l), self.width)).unzip();
        self.breaks.splice(from..old_end, breaks);
        self.indents.splice(from..old_end, indents);
        self.count(folds);
    }

    fn count(&mut self, folds: &[(usize, usize)]) {
        let lines = self.breaks.len();
        let mut hidden = vec![false; lines];
        for &(start, end) in folds {
            for h in hidden.iter_mut().take(end.min(lines)).skip(start + 1) {
                *h = true;
            }
        }
        self.starts.clear();
        let mut row = 0;
        for (line, breaks) in self.breaks.iter().enumerate() {
            self.starts.push(row);
            if !hidden[line] {
                row += breaks.len() + 1;
            }
        }
        self.starts.push(row);
    }
}

/// Where `text` breaks to fit rows of `cells` cells, and how far its continuation rows are indented. As gpui-kit's
/// `measured_wrap_boundaries`: fit as much as the row holds, then go back to the last break opportunity after the
/// row's start (and, on the first row, past the indent); with none, break where the row is full, keeping at least one
/// character on every row.
pub(super) fn wrap(text: &str, cells: usize) -> (Vec<usize>, usize) {
    use unicode_segmentation::UnicodeSegmentation;
    if cells == 0 {
        return (vec![], 0);
    }
    let indent = text.chars().take_while(|&c| c == ' ').count().min(MAX_INDENT);
    // Each grapheme's end, as a byte offset and a cell from the line's start.
    let mut ends: Vec<(usize, usize)> = vec![];
    let mut x = 0;
    for (i, g) in text.grapheme_indices(true) {
        for c in g.chars() {
            x += width(c, x);
        }
        ends.push((i + g.len(), x));
    }
    let opportunities: Vec<usize> = unicode_linebreak::linebreaks(text).map(|(i, _)| i).filter(|i| ends.binary_search_by_key(i, |e| e.0).is_ok()).collect();
    let mut breaks = vec![];
    let (mut first, mut start, mut start_cell) = (0, 0, 0);
    while first < ends.len() {
        let room = if start == 0 { cells } else { cells.saturating_sub(indent) };
        let fit = ends[first..].partition_point(|&(_, cell)| cell - start_cell <= room);
        let fitting_end = ends[first + fit.max(1) - 1].0;
        if fitting_end == text.len() {
            break;
        }
        let candidate = opportunities.partition_point(|&o| o <= fitting_end);
        let end = candidate.checked_sub(1).map(|i| opportunities[i]).filter(|&o| o > start && (start != 0 || o > indent)).unwrap_or(fitting_end);
        breaks.push(end);
        first = ends.partition_point(|&(e, _)| e <= end);
        start = end;
        start_cell = ends[first - 1].1;
    }
    let indent = if breaks.is_empty() { 0 } else { indent };
    (breaks, indent)
}

impl Editor {
    /// Cells a row of text holds in the editor's area; 0 before it knows its width.
    fn wrap_width(&self) -> usize {
        let text_w = self.area[0] - self.gutter_w();
        if text_w <= 0.0 {
            return 0;
        }
        // A cell spare at the right for the caret after the row's last character.
        ((text_w / self.metrics().cell_w).floor() as usize).saturating_sub(1).max(1)
    }

    /// Whether lines wrap: once the editor knows its width.
    pub(super) fn rows_wrap(&self) -> bool {
        self.rows.width > 0
    }

    /// The area changed width by enough to change how many cells a row holds.
    pub(super) fn wrap_width_changed(&self) -> bool {
        self.rows.width != self.wrap_width()
    }

    /// Wrap every line again: the width or the whole text changed.
    pub(super) fn relayout(&mut self) {
        self.rows.width = self.wrap_width();
        let lines = self.buffer.line_count();
        let old = self.rows.breaks.len();
        self.rows.rewrap(&self.buffer, 0, old, lines, &self.folds);
    }

    /// An edit replaced lines `a..=b` with `a..=end`: wrap only those again, and drop or move the folds and fold
    /// candidates as gpui-kit's `adjust_folds_for_edit` does. A fold or candidate touching the edited lines goes; one
    /// below them moves with the lines.
    pub(super) fn relayout_edit(&mut self, a: usize, b: usize, end: usize) {
        let delta = end as isize - b as isize;
        for list in [&mut self.folds, &mut self.fold_spots] {
            list.retain(|&(s, e)| !(s <= b && e >= a));
            if delta != 0 {
                for (s, e) in list.iter_mut().filter(|(s, _)| *s > b) {
                    *s = s.saturating_add_signed(delta);
                    *e = e.saturating_add_signed(delta);
                }
            }
        }
        // Rows laid out for other text than this edit started from, or for another width (a line count with one more
        // digit widens the gutter), are wrapped whole.
        if self.rows.breaks.len() + end != self.buffer.line_count() + b || self.wrap_width_changed() {
            return self.relayout();
        }
        self.rows.rewrap(&self.buffer, a, b + 1, end + 1, &self.folds);
    }

    /// New fold candidates from the parser. A fold whose block is gone opens.
    pub fn set_fold_spots(&mut self, spots: Vec<(usize, usize)>) {
        self.fold_spots = spots;
        let before = self.folds.len();
        self.folds.retain(|f| self.fold_spots.iter().any(|s| s.0 == f.0));
        if self.folds.len() != before {
            self.rows.count(&self.folds);
        }
        self.dirty = true;
    }

    /// The fold candidate starting on `line`, if any.
    pub fn fold_spot(&self, line: usize) -> Option<(usize, usize)> {
        self.fold_spots.binary_search_by_key(&line, |s| s.0).ok().map(|i| self.fold_spots[i])
    }

    pub fn is_folded(&self, line: usize) -> bool {
        self.folds.iter().any(|f| f.0 == line)
    }

    /// Fold the block starting on `line`, or open it. As in gpui-kit, a caret the fold hides stays where it is,
    /// undrawn, until it moves or the text there changes.
    pub fn toggle_fold(&mut self, line: usize) {
        if self.is_folded(line) {
            self.folds.retain(|f| f.0 != line);
        } else if let Some(spot) = self.fold_spot(line) {
            self.folds.push(spot);
            self.folds.sort();
        }
        self.rows.count(&self.folds);
        self.dirty = true;
    }

    /// The line whose fold chevron is under a point of the editor's area, if any: the column right of the line
    /// numbers, on the first row of a block that can fold.
    pub fn fold_at(&self, x: f32, y: f32) -> Option<usize> {
        let right = self.gutter_w() - super::GUTTER_PAD;
        if !(right - super::FOLD_W..right).contains(&x) {
            return None;
        }
        let row = ((y + self.scroll[1]) / self.metrics().line_h).floor().max(0.0) as usize;
        let (line, k) = self.row_line(row.min(self.row_count() - 1));
        (k == 0 && self.fold_spot(line).is_some()).then_some(line)
    }

    /// The pointer moved over the editor: over the gutter every chevron shows. Whether that changed.
    pub fn set_gutter_hover(&mut self, over: bool) -> bool {
        let changed = self.gutter_hover != over;
        self.gutter_hover = over;
        self.dirty |= changed;
        changed
    }

    /// The position a character before `p`, as Left and Backspace reach it: from the line after a fold, the end of
    /// the fold's first line, so the hidden lines are stepped over (gpui-kit's `clamp_offset_to_visible_backward`).
    pub(super) fn prev_visible(&self, p: Pos) -> Pos {
        let q = self.buffer.prev(p);
        match self.folds.iter().find(|&&(s, e)| self.hidden(q.line) && s < q.line && q.line <= e) {
            Some(&(s, _)) => Pos::new(s, self.buffer.line(s).len()),
            None => q,
        }
    }

    /// The position a character after `p`, as Right and Delete reach it: into a fold, the start of its last line.
    pub(super) fn next_visible(&self, p: Pos) -> Pos {
        let q = self.buffer.next(p);
        match self.folds.iter().find(|&&(s, e)| self.hidden(q.line) && s < q.line && q.line <= e) {
            Some(&(_, e)) => Pos::new(e, 0),
            None => q,
        }
    }

    pub(super) fn hidden(&self, line: usize) -> bool {
        self.rows.starts.get(line).is_some_and(|&s| self.rows.starts.get(line + 1) == Some(&s))
    }

    /// Rows in the whole text.
    pub(super) fn row_count(&self) -> usize {
        self.rows.starts.last().copied().unwrap_or(1).max(1)
    }

    /// The first row of `line`.
    pub(super) fn line_row(&self, line: usize) -> usize {
        self.rows.starts.get(line).copied().unwrap_or(line)
    }

    /// The line row `row` belongs to, and which of the line's rows it is.
    pub(super) fn row_line(&self, row: usize) -> (usize, usize) {
        if self.rows.starts.is_empty() {
            return (row.min(self.buffer.line_count() - 1), 0);
        }
        let line = (self.rows.starts.partition_point(|&s| s <= row) - 1).min(self.buffer.line_count() - 1);
        (line, row - self.rows.starts[line])
    }

    /// Where `line`'s rows start, as byte offsets, and how far its continuation rows are indented.
    pub(super) fn line_breaks(&self, line: usize) -> (&[usize], usize) {
        match self.rows.breaks.get(line) {
            Some(b) => (b, self.rows.indents[line]),
            None => (&[], 0),
        }
    }

    /// The bytes of row `k` of `line`.
    pub(super) fn row_bytes(&self, line: usize, k: usize) -> Range<usize> {
        let (breaks, _) = self.line_breaks(line);
        let start = if k == 0 { 0 } else { breaks[k - 1] };
        start..breaks.get(k).copied().unwrap_or(self.buffer.line(line).len())
    }

    /// Which of its line's rows `p` shows on. A caret on a row break shows at the start of the next row, unless it
    /// went to the end of the row above (`row_end`).
    pub(super) fn row_in_line(&self, p: Pos, row_end: bool) -> usize {
        let (breaks, _) = self.line_breaks(p.line);
        let k = breaks.partition_point(|&b| b <= p.col);
        if row_end && k > 0 && breaks[k - 1] == p.col { k - 1 } else { k }
    }

    /// The row `p` shows on; for a line a fold hides, the last row before the fold, as gpui-kit's
    /// `nearest_visible_display_row`.
    pub(super) fn row_of(&self, p: Pos) -> usize {
        self.visible_row(p, false)
    }

    /// The row the caret shows on (or would, when a fold hides it).
    pub(super) fn caret_row(&self) -> usize {
        self.visible_row(self.sel.head, self.row_end)
    }

    fn visible_row(&self, p: Pos, row_end: bool) -> usize {
        if self.hidden(p.line) {
            return self.line_row(p.line).saturating_sub(1);
        }
        self.line_row(p.line) + self.row_in_line(p, row_end)
    }

    /// The cell `p` shows at on row `k` of its line, counting the row's indent.
    pub(super) fn cell_in_row(&self, p: Pos, k: usize) -> usize {
        let (_, indent) = self.line_breaks(p.line);
        let start = self.row_bytes(p.line, k).start;
        let text = self.buffer.line(p.line);
        let shift = if k == 0 { 0 } else { indent };
        columns(&text[..p.col.min(text.len())]) - columns(&text[..start]) + shift
    }

    /// Where the cursors besides the main one show: their row and cell.
    pub(super) fn other_carets(&self) -> Vec<(usize, usize)> {
        self.other_heads().filter(|(p, _)| !self.hidden(p.line)).map(|(p, row_end)| {
            let k = self.row_in_line(p, row_end);
            (self.line_row(p.line) + k, self.cell_in_row(p, k))
        }).collect()
    }

    /// The cell `p` shows at on its row.
    pub(super) fn cell_of(&self, p: Pos) -> usize {
        self.cell_in_row(p, self.row_in_line(p, false))
    }

    /// The cell the caret shows at on its row.
    pub(super) fn caret_cell(&self) -> usize {
        self.cell_in_row(self.sel.head, self.row_in_line(self.sel.head, self.row_end))
    }

    /// The position on `row` nearest `cell`, and whether it is the end of a row that wraps (where the caret shows at
    /// the end of this row, not the start of the next).
    pub(super) fn at_row_cell(&self, row: usize, cell: usize) -> (Pos, bool) {
        let (line, k) = self.row_line(row);
        let text = self.buffer.line(line);
        let bytes = self.row_bytes(line, k);
        let (_, indent) = self.line_breaks(line);
        let shift = if k == 0 { 0 } else { indent };
        let mut x = columns(&text[..bytes.start]);
        let target = x + cell.saturating_sub(shift);
        for (i, c) in text[bytes.clone()].char_indices() {
            let w = width(c, x);
            if target <= x + w / 2 {
                return (Pos::new(line, bytes.start + i), false);
            }
            x += w;
        }
        let wraps = bytes.end < text.len();
        (Pos::new(line, bytes.end), wraps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_line_wraps_after_the_last_space_that_fits_and_keeps_its_indent() {
        assert_eq!(wrap("short", 10), (vec![], 0));
        // "  aaa bbb ccc" in rows of 8: "  aaa ", then "bbb ccc" would need 9 cells after the indent, so "bbb ", "ccc".
        assert_eq!(wrap("  aaa bbb ccc", 8), (vec![6, 10], 2));
        assert_eq!(wrap("abcdefghij", 4), (vec![4, 8], 0), "a word longer than a row breaks where the row is full");
        assert_eq!(wrap("abc", 0), (vec![], 0), "no width, no wrapping");
        assert_eq!(wrap("", 4), (vec![], 0));
        assert_eq!(wrap("日本語日本", 4), (vec![6, 12], 0), "wide characters take two cells and break anywhere");
        assert_eq!(wrap("\tab cd ef", 8), (vec![4], 0), "a tab counts to its stop");
        assert_eq!(wrap("aaaa-bbbb", 6), (vec![5], 0), "Unicode line breaking allows a break after a hyphen");
        // Each é is a letter and a combining accent, two cells here. A row of 3 cells would split the second é if
        // wrapping went by character; by grapheme each row holds one whole é.
        assert_eq!(wrap("e\u{301}e\u{301}e\u{301}", 3), (vec![3, 6], 0), "a letter and its accent stay together");
    }

    #[test]
    fn rows_map_to_lines_and_back_through_wraps_and_folds() {
        let mut e = Editor::new("e", "aaaa bbbb cccc\nx\ny\nz\nw");
        e.rows.width = 6;
        e.relayout_with_width();
        assert_eq!(e.row_count(), 7, "three rows for the long line");
        assert_eq!(e.row_line(2), (0, 2));
        assert_eq!(e.row_line(3), (1, 0));
        assert_eq!(e.row_of(Pos::new(0, 7)), 1);
        assert_eq!(e.row_of(Pos::new(0, 5)), 1, "a break shows at the start of the next row");
        assert_eq!(e.row_in_line(Pos::new(0, 5), true), 0, "or at the end of the row above");
        assert_eq!(e.at_row_cell(1, 2), (Pos::new(0, 7), false));
        assert_eq!(e.at_row_cell(0, 99), (Pos::new(0, 5), true));
        e.fold_spots = vec![(1, 4)];
        e.toggle_fold(1);
        assert_eq!(e.row_count(), 5, "lines 2 and 3 hidden");
        assert_eq!(e.row_line(4), (4, 0));
        assert!(e.hidden(2) && e.hidden(3) && !e.hidden(1) && !e.hidden(4));
        e.toggle_fold(1);
        assert_eq!(e.row_count(), 7);
        e.toggle_fold(1);
        e.select(crate::editor::buffer::Selection::caret(Pos::new(3, 0)));
        assert!(e.is_folded(1), "a jump into a fold leaves it folded, as in gpui-kit");
        assert_eq!(e.caret_row(), 3, "and the caret counts as on the fold's first line");
        assert_eq!(e.next_visible(Pos::new(1, 1)), Pos::new(4, 0), "Right steps over the hidden lines");
        assert_eq!(e.prev_visible(Pos::new(4, 0)), Pos::new(1, 1), "and Left back");
    }

    #[test]
    fn an_edit_drops_the_folds_it_touches_and_moves_those_below() {
        let mut e = Editor::new("e", "a\nb\nc\nd\ne\nf\ng\nh");
        e.fold_spots = vec![(0, 2), (4, 7)];
        e.toggle_fold(0);
        e.toggle_fold(4);
        e.replace(Pos::new(1, 0), Pos::new(1, 0), "x\n", false);
        assert_eq!(e.folds, [(5, 8)], "the edit inside the first fold opened it and moved the second");
        assert_eq!(e.fold_spots, [(5, 8)]);
        assert!(e.hidden(6) && e.hidden(7) && !e.hidden(8));
    }

    #[test]
    fn undo_and_redo_move_folds_as_edits_do() {
        let mut e = Editor::new("e", "a\nb\nc\nd\ne\nf\ng\nh");
        e.fold_spots = vec![(4, 7)];
        e.toggle_fold(4);
        e.replace(Pos::new(0, 0), Pos::new(0, 0), "x\n", false);
        assert_eq!(e.folds, [(5, 8)]);
        e.undo();
        assert_eq!(e.folds, [(4, 7)], "undo moves the fold back and keeps it folded");
        e.redo();
        assert_eq!(e.folds, [(5, 8)]);
        assert!(e.hidden(6) && e.hidden(7));
    }
}

#[cfg(test)]
impl Editor {
    /// Lay out at the width tests set by hand.
    fn relayout_with_width(&mut self) {
        let lines = self.buffer.line_count();
        let old = self.rows.breaks.len();
        self.rows.rewrap(&self.buffer, 0, old, lines, &self.folds);
    }
}
