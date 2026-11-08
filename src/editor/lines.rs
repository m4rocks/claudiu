//! Line actions, as in VS Code: move, duplicate or delete the lines under the selection, and select whole lines.
//! `apply` works on plain text and byte offsets, so its rules test without an editor; `Editor::edit_lines` runs it on
//! the editor's text.

use std::ops::Range;

use super::Editor;
use super::buffer::{Pos, Selection};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineAction {
    /// Alt+Up / Down: swap the lines with the one above or below.
    MoveUp,
    MoveDown,
    /// Shift+Alt+Up / Down: copy the lines above or below them.
    DuplicateUp,
    DuplicateDown,
    /// Primary+Shift+K, and Primary+X with nothing selected (after copying the line).
    Delete,
    /// Primary+L: the lines under the selection; again, one more line.
    Select,
    /// Primary+Shift+Up / Down: select whole lines, growing (or, against the grain, shrinking) a line at a time.
    ExtendUp,
    ExtendDown,
}

/// A selection in byte offsets: `anchor` stays put while `cursor` moves, so `cursor` may come first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub anchor: usize,
    pub cursor: usize,
}

impl Span {
    fn range(self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }
}

/// What an action does: replace a range of the text, and leave this selection (in the new text).
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub edit: Option<(Range<usize>, String)>,
    pub selection: Span,
}

/// The text's lines. A line ends at `\n`, which belongs to no line; the last line has no `\n` after it.
struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1)).collect();
        Lines { text, starts }
    }

    fn count(&self) -> usize {
        self.starts.len()
    }

    fn start(&self, line: usize) -> usize {
        self.starts[line]
    }

    /// Where the line's text ends, before its `\n`.
    fn end(&self, line: usize) -> usize {
        if line + 1 < self.count() { self.starts[line + 1] - 1 } else { self.text.len() }
    }

    /// Where the line ends with its `\n`, which is where the next line starts.
    fn end_with_newline(&self, line: usize) -> usize {
        if line + 1 < self.count() { self.starts[line + 1] } else { self.text.len() }
    }

    fn text(&self, line: usize) -> &'a str {
        &self.text[self.start(line)..self.end(line)]
    }

    fn containing(&self, offset: usize) -> usize {
        self.starts.partition_point(|&start| start <= offset) - 1
    }

    /// The first and last line a selection touches. A selection that ends at the start of a line leaves that line out.
    fn touched_by(&self, selection: Range<usize>) -> (usize, usize) {
        let first = self.containing(selection.start);
        let last = self.containing(selection.end);
        if selection.start < selection.end && selection.end == self.start(last) { (first, last - 1) } else { (first, last) }
    }

    /// Whether the selection is exactly lines `first` to `last` with their line ends.
    fn is_whole(&self, selection: Range<usize>, first: usize, last: usize) -> bool {
        selection == (self.start(first)..self.end_with_newline(last))
    }

    /// The offset `column` characters into `line`, or its end when it is shorter.
    fn at_column(&self, line: usize, column: usize) -> usize {
        let text = self.text(line);
        self.start(line) + text.char_indices().nth(column).map_or(text.len(), |(at, _)| at)
    }
}

/// Run `action` on `text` with `selection`. `None` when there is nothing to do: the first line can't move up, and so on.
pub fn apply(action: LineAction, text: &str, selection: Span) -> Option<Outcome> {
    use LineAction::*;
    let lines = Lines::new(text);
    let (first, last) = lines.touched_by(selection.range());
    let unchanged = |edit| Outcome { edit, selection };
    let shifted = |edit, by: isize| {
        let shift = |at: usize| at.checked_add_signed(by).unwrap();
        Outcome { edit: Some(edit), selection: Span { anchor: shift(selection.anchor), cursor: shift(selection.cursor) } }
    };
    let block = || &text[lines.start(first)..lines.end(last)];
    match action {
        Delete => {
            let column = text[lines.start(lines.containing(selection.cursor))..selection.cursor].chars().count();
            let (removed, cursor) = if last + 1 < lines.count() {
                (lines.start(first)..lines.start(last + 1), lines.start(first) + (lines.at_column(last + 1, column) - lines.start(last + 1)))
            } else if first > 0 {
                (lines.end(first - 1)..lines.end(last), lines.at_column(first - 1, column))
            } else {
                (0..text.len(), 0)
            };
            Some(Outcome { edit: Some((removed, String::new())), selection: Span { anchor: cursor, cursor } })
        }
        MoveUp => {
            let above = lines.text(first.checked_sub(1)?);
            let moved = format!("{}\n{above}", block());
            Some(shifted((lines.start(first - 1)..lines.end(last), moved), -(above.len() as isize + 1)))
        }
        MoveDown => {
            if last + 1 == lines.count() {
                return None;
            }
            let below = lines.text(last + 1);
            let moved = format!("{below}\n{}", block());
            Some(shifted((lines.start(first)..lines.end(last + 1), moved), below.len() as isize + 1))
        }
        DuplicateUp => Some(unchanged(Some((lines.start(first)..lines.start(first), format!("{}\n", block()))))),
        DuplicateDown => Some(shifted((lines.end(last)..lines.end(last), format!("\n{}", block())), block().len() as isize + 1)),
        Select => {
            let last = if lines.is_whole(selection.range(), first, last) && last + 1 < lines.count() { last + 1 } else { last };
            let whole = Span { anchor: lines.start(first), cursor: lines.end_with_newline(last) };
            (whole != selection).then_some(Outcome { edit: None, selection: whole })
        }
        ExtendDown => {
            let whole = lines.is_whole(selection.range(), first, last);
            let selection = if whole && selection.cursor < selection.anchor && last > first {
                // Selected upwards: coming back down gives up the top line.
                Span { anchor: selection.anchor, cursor: lines.start(first + 1) }
            } else if whole {
                (last + 1 < lines.count()).then(|| Span { anchor: lines.start(first), cursor: lines.end_with_newline(last + 1) })?
            } else {
                Span { anchor: lines.start(first), cursor: lines.end_with_newline(last) }
            };
            Some(Outcome { edit: None, selection })
        }
        ExtendUp => {
            let whole = lines.is_whole(selection.range(), first, last);
            let selection = if whole && selection.cursor > selection.anchor && last > first {
                // Selected downwards: coming back up gives up the bottom line.
                Span { anchor: selection.anchor, cursor: lines.start(last) }
            } else if whole {
                Span { anchor: lines.end_with_newline(last), cursor: lines.start(first.checked_sub(1)?) }
            } else {
                Span { anchor: lines.end_with_newline(last), cursor: lines.start(first) }
            };
            Some(Outcome { edit: None, selection })
        }
    }
}

/// `p` as a byte offset into `text`.
fn offset_of(text: &str, p: Pos) -> usize {
    let lines = Lines::new(text);
    lines.start(p.line) + p.col
}

/// Byte offset `at` in `text` as a line and column.
fn pos_of(text: &str, at: usize) -> Pos {
    let lines = Lines::new(text);
    let line = lines.containing(at);
    Pos::new(line, at - lines.start(line))
}

impl Editor {
    /// Run a line action on the text, as one undo step. Returns whether it did anything.
    pub fn edit_lines(&mut self, action: LineAction) -> bool {
        let text = self.buffer.text();
        let selection = Span { anchor: offset_of(&text, self.sel.anchor), cursor: offset_of(&text, self.sel.head) };
        let Some(outcome) = apply(action, &text, selection) else { return false };
        let after = match outcome.edit {
            Some((range, with)) => {
                self.replace(pos_of(&text, range.start), pos_of(&text, range.end), &with, false);
                let mut after = text;
                after.replace_range(range, &with);
                after
            }
            None => text,
        };
        self.select(Selection { anchor: pos_of(&after, outcome.selection.anchor), head: pos_of(&after, outcome.selection.cursor) });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LineAction::*;

    /// Text with its selection written in: `|` is a cursor; `{` marks the anchor and `}` the cursor of a selection.
    fn parse(marked: &str) -> (String, Span) {
        let (mut text, mut anchor, mut cursor) = (String::new(), None, None);
        for c in marked.chars() {
            match c {
                '|' => (anchor, cursor) = (Some(text.len()), Some(text.len())),
                '{' => anchor = Some(text.len()),
                '}' => cursor = Some(text.len()),
                c => text.push(c),
            }
        }
        (text, Span { anchor: anchor.expect("an anchor"), cursor: cursor.expect("a cursor") })
    }

    fn show(text: &str, selection: Span) -> String {
        let mut out = text.to_string();
        if selection.anchor == selection.cursor {
            out.insert(selection.cursor, '|');
        } else {
            // The later offset first, so the earlier one is still where it was.
            let mut marks = [(selection.anchor, '{'), (selection.cursor, '}')];
            marks.sort_by_key(|m| std::cmp::Reverse(m.0));
            for (at, mark) in marks {
                out.insert(at, mark);
            }
        }
        out
    }

    /// The marked text after the action; `None` when the action does nothing.
    fn run(action: LineAction, marked: &str) -> Option<String> {
        let (text, selection) = parse(marked);
        let outcome = apply(action, &text, selection)?;
        let mut after = text;
        if let Some((range, with)) = &outcome.edit {
            after.replace_range(range.clone(), with);
        }
        Some(show(&after, outcome.selection))
    }

    fn after(action: LineAction, marked: &str) -> String {
        run(action, marked).expect("the action does something")
    }

    #[test]
    fn delete_removes_every_touched_line_and_keeps_the_column() {
        assert_eq!(after(Delete, "a\nb|\nc"), "a\nc|");
        assert_eq!(after(Delete, "a\nbbb|\nccccc"), "a\nccc|cc");
        assert_eq!(after(Delete, "a\n{b\nc}\nd"), "a\nd|");
        assert_eq!(after(Delete, "|a\nb"), "|b");
    }

    #[test]
    fn deleting_the_last_line_takes_the_newline_before_it_too() {
        assert_eq!(after(Delete, "a\nb|"), "a|");
        assert_eq!(after(Delete, "{a\nb}"), "|");
        assert_eq!(after(Delete, "only|"), "|");
    }

    #[test]
    fn a_selection_ending_at_the_start_of_a_line_leaves_that_line_alone() {
        assert_eq!(after(Delete, "{a\nb\n}c"), "|c");
    }

    #[test]
    fn move_up_swaps_with_the_line_above_and_carries_the_selection() {
        assert_eq!(after(MoveUp, "a\nb|b\nc"), "b|b\na\nc");
        assert_eq!(after(MoveUp, "a\nb\n{c\nd}"), "a\n{c\nd}\nb");
        assert_eq!(after(MoveUp, "a\nb\n}c\nd{"), "a\n}c\nd{\nb", "a backwards selection stays backwards");
    }

    #[test]
    fn move_down_swaps_with_the_line_below() {
        assert_eq!(after(MoveDown, "a|a\nb\nc"), "b\na|a\nc");
        assert_eq!(after(MoveDown, "{a\nb}\nc\nd"), "c\n{a\nb}\nd");
    }

    #[test]
    fn move_stops_at_the_ends() {
        assert_eq!(run(MoveUp, "a|\nb"), None);
        assert_eq!(run(MoveDown, "a\nb|"), None);
        assert_eq!(run(MoveUp, "|"), None);
    }

    #[test]
    fn moving_a_line_across_the_last_line_adds_no_newline() {
        assert_eq!(after(MoveUp, "a\nb|"), "b|\na");
        assert_eq!(after(MoveDown, "a|\nb"), "b\na|");
    }

    #[test]
    fn duplicate_down_copies_below_and_selects_the_copy() {
        assert_eq!(after(DuplicateDown, "a\nb|b\nc"), "a\nbb\nb|b\nc");
        assert_eq!(after(DuplicateDown, "a|"), "a\na|");
        assert_eq!(after(DuplicateDown, "{a\nb}"), "a\nb\n{a\nb}");
    }

    #[test]
    fn duplicate_up_copies_above_and_keeps_the_cursor_on_the_upper_copy() {
        assert_eq!(after(DuplicateUp, "a\nb|b\nc"), "a\nb|b\nbb\nc");
        assert_eq!(after(DuplicateUp, "a|"), "a|\na");
    }

    #[test]
    fn select_takes_the_line_then_one_more_each_time() {
        assert_eq!(after(Select, "a\nb|b\nc"), "a\n{bb\n}c");
        assert_eq!(after(Select, "a\n{bb\n}c"), "a\n{bb\nc}");
        assert_eq!(run(Select, "a\n{bb\nc}"), None, "nothing is left to add");
        assert_eq!(after(Select, "a\nb\nc|"), "a\nb\n{c}");
        assert_eq!(after(Select, "a\nb{b}b\nc"), "a\n{bbb\n}c", "part of a line selects the whole line");
    }

    #[test]
    fn extend_down_takes_whole_lines_and_grows_a_line_at_a_time() {
        assert_eq!(after(ExtendDown, "a\nb|b\nc\nd"), "a\n{bb\n}c\nd");
        assert_eq!(after(ExtendDown, "a\n{bb\n}c\nd"), "a\n{bb\nc\n}d");
        assert_eq!(after(ExtendDown, "a\n{bb\nc\n}d"), "a\n{bb\nc\nd}");
        assert_eq!(run(ExtendDown, "a\n{bb\nc\nd}"), None, "the end of the file");
    }

    #[test]
    fn extend_up_takes_whole_lines_and_grows_a_line_at_a_time() {
        assert_eq!(after(ExtendUp, "a\nb\nc|c\nd"), "a\nb\n}cc\n{d");
        assert_eq!(after(ExtendUp, "a\nb\n}cc\n{d"), "a\n}b\ncc\n{d");
        assert_eq!(after(ExtendUp, "a\n}b\ncc\n{d"), "}a\nb\ncc\n{d");
        assert_eq!(run(ExtendUp, "}a\nb\ncc\n{d"), None, "the start of the file");
    }

    #[test]
    fn extending_against_the_grain_gives_a_line_back() {
        assert_eq!(after(ExtendUp, "a\n{bb\nc\n}d"), "a\n{bb\n}c\nd");
        assert_eq!(after(ExtendDown, "a\n}bb\nc\n{d"), "a\nbb\n}c\n{d");
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        assert_eq!(after(Delete, "a\nbéé|\ncéééé"), "a\ncéé|éé");
        assert_eq!(after(MoveUp, "a\né|é"), "é|é\na");
    }

    #[test]
    fn the_editor_moves_lines_as_one_undo_step() {
        let mut e = Editor::new("e", "one\ntwo\nthree");
        e.select(Selection::caret(Pos::new(2, 2)));
        assert!(e.edit_lines(MoveUp));
        assert_eq!(e.buffer.text(), "one\nthree\ntwo");
        assert_eq!(e.caret(), Pos::new(1, 2), "the caret moves with its line");
        e.undo();
        assert_eq!(e.buffer.text(), "one\ntwo\nthree");
        e.select(Selection::caret(Pos::new(0, 1)));
        assert!(e.edit_lines(Select));
        assert_eq!(e.selected_text(), "one\n");
    }
}
