//! Editor mode's line actions, as in VS Code: cut or copy a line, move, duplicate or delete lines, select lines.
//! `apply` works on plain text and offsets, so the rules test without an editor; `Workspace::edit_lines` runs them on the
//! shown file's editor.

use std::ops::Range;

use gpui::ClipboardItem;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineAction {
    /// Ctrl+X: with nothing selected, the line goes to the clipboard.
    CutLine,
    /// Ctrl+C: with nothing selected, the line is copied.
    CopyLine,
    MoveUp,
    MoveDown,
    DuplicateUp,
    DuplicateDown,
    Delete,
    /// Ctrl+L: the lines under the selection; again, one more line.
    Select,
    /// Ctrl+Shift+Up / Down: select whole lines, growing (or, against the grain, shrinking) a line at a time.
    ExtendUp,
    ExtendDown,
}

/// A line action as a key binding's action.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = claudiu, no_json)]
pub struct EditLines(pub LineAction);

/// VS Code's keys for the line actions, over the editor only (the pane's find box is an input too). Ctrl+X and Ctrl+C act
/// on the line when nothing is selected and otherwise as before. Ctrl+Shift+Left / Right already select by word, so
/// Ctrl+Shift+Up / Down select by line. Shift+Alt+Up / Down replace the editor's add-cursor keys on Linux.
pub fn line_action_bindings() -> Vec<gpui::KeyBinding> {
    use LineAction::*;
    let key = |keys, action| gpui::KeyBinding::new(keys, EditLines(action), Some("EditorPane > Input"));
    vec![
        key("secondary-x", CutLine),
        key("secondary-c", CopyLine),
        key("alt-up", MoveUp),
        key("alt-down", MoveDown),
        key("shift-alt-up", DuplicateUp),
        key("shift-alt-down", DuplicateDown),
        key("secondary-shift-k", Delete),
        key("secondary-l", Select),
        key("secondary-shift-up", ExtendUp),
        key("secondary-shift-down", ExtendDown),
    ]
}

/// A selection: `anchor` stays put while `cursor` moves, so `cursor` may come first. Byte offsets into the text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub anchor: usize,
    pub cursor: usize,
}

impl Selection {
    fn range(self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }
}

/// What an action does: replace a range of the text, put text on the clipboard, and leave this selection (in the new text).
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub edit: Option<(Range<usize>, String)>,
    pub clipboard: Option<String>,
    pub selection: Selection,
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
/// `CutLine` and `CopyLine` also answer `None` for a selection, which the editor's own cut and copy handle.
pub fn apply(action: LineAction, text: &str, selection: Selection) -> Option<Outcome> {
    use LineAction::*;
    let lines = Lines::new(text);
    let (first, last) = lines.touched_by(selection.range());
    let collapsed = |at| Selection { anchor: at, cursor: at };
    let unchanged = |edit| Outcome { edit, clipboard: None, selection };
    let shifted = |edit, by: isize| {
        let shift = |at: usize| at.checked_add_signed(by).unwrap();
        Outcome { edit: Some(edit), clipboard: None, selection: Selection { anchor: shift(selection.anchor), cursor: shift(selection.cursor) } }
    };
    let block = || &text[lines.start(first)..lines.end(last)];
    match action {
        CopyLine | CutLine if !selection.range().is_empty() => None,
        CopyLine => Some(Outcome { clipboard: Some(format!("{}\n", block())), ..unchanged(None) }),
        CutLine | Delete => {
            let column = text[lines.start(lines.containing(selection.cursor))..selection.cursor].chars().count();
            let (removed, cursor) = if last + 1 < lines.count() {
                (lines.start(first)..lines.start(last + 1), lines.start(first) + (lines.at_column(last + 1, column) - lines.start(last + 1)))
            } else if first > 0 {
                (lines.end(first - 1)..lines.end(last), lines.at_column(first - 1, column))
            } else {
                (0..text.len(), 0)
            };
            let clipboard = (action == CutLine).then(|| format!("{}\n", block()));
            Some(Outcome { edit: Some((removed, String::new())), clipboard, selection: collapsed(cursor) })
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
            let whole = Selection { anchor: lines.start(first), cursor: lines.end_with_newline(last) };
            (whole != selection).then(|| Outcome { selection: whole, ..unchanged(None) })
        }
        ExtendDown => {
            let whole = lines.is_whole(selection.range(), first, last);
            let selection = if whole && selection.cursor < selection.anchor && last > first {
                // Selected upwards: coming back down gives up the top line.
                Selection { anchor: selection.anchor, cursor: lines.start(first + 1) }
            } else if whole {
                (last + 1 < lines.count()).then(|| Selection { anchor: lines.start(first), cursor: lines.end_with_newline(last + 1) })?
            } else {
                Selection { anchor: lines.start(first), cursor: lines.end_with_newline(last) }
            };
            Some(Outcome { selection, ..unchanged(None) })
        }
        ExtendUp => {
            let whole = lines.is_whole(selection.range(), first, last);
            let selection = if whole && selection.cursor > selection.anchor && last > first {
                // Selected downwards: coming back up gives up the bottom line.
                Selection { anchor: selection.anchor, cursor: lines.start(last) }
            } else if whole {
                Selection { anchor: lines.end_with_newline(last), cursor: lines.start(first.checked_sub(1)?) }
            } else {
                Selection { anchor: lines.end_with_newline(last), cursor: lines.start(first) }
            };
            Some(Outcome { selection, ..unchanged(None) })
        }
    }
}

impl Workspace {
    /// Run a line action on the shown file's editor. Bound over the whole pane, whose find box is an input too, so a
    /// press while that has the keyboard, or while several cursors are active, does what the key did before: cut and
    /// copy work on the selection, the rest does nothing.
    pub fn edit_lines(&mut self, action: LineAction, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::input::{Copy, Cut};
        let Some(buffer) = self.active_id().and_then(|id| self.panes.get(id)).and_then(|p| p.shown()).map(|t| t.buffer.clone()) else { return };
        let outcome = buffer.update(cx, |b, cx| {
            if !b.focus_handle(cx).is_focused(window) || b.has_multiple_cursors() {
                return None;
            }
            let range = b.selected_range();
            let anchor = if b.cursor() == range.end { range.start } else { range.end };
            let outcome = apply(action, &b.value(), Selection { anchor, cursor: b.cursor() })?;
            if outcome.edit.is_some() && !b.is_editable() {
                return None;
            }
            if let Some(text) = &outcome.clipboard {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            }
            if let Some((range, text)) = &outcome.edit {
                b.replace_range(range.clone(), text, window, cx);
            }
            b.select_range(outcome.selection.anchor, outcome.selection.cursor, cx);
            Some(())
        });
        if outcome.is_none() {
            match action {
                LineAction::CutLine => window.dispatch_action(Box::new(Cut), cx),
                LineAction::CopyLine => window.dispatch_action(Box::new(Copy), cx),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LineAction::*;

    /// Text with its selection written in: `|` is a cursor; `{` marks the anchor and `}` the cursor of a selection.
    fn parse(marked: &str) -> (String, Selection) {
        let (mut text, mut anchor, mut cursor) = (String::new(), None, None);
        for c in marked.chars() {
            match c {
                '|' => (anchor, cursor) = (Some(text.len()), Some(text.len())),
                '{' => anchor = Some(text.len()),
                '}' => cursor = Some(text.len()),
                c => text.push(c),
            }
        }
        (text, Selection { anchor: anchor.expect("an anchor"), cursor: cursor.expect("a cursor") })
    }

    fn show(text: &str, selection: Selection) -> String {
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

    /// The marked text after the action, and what went to the clipboard; `None` when the action does nothing.
    fn run(action: LineAction, marked: &str) -> Option<(String, Option<String>)> {
        let (text, selection) = parse(marked);
        let outcome = apply(action, &text, selection)?;
        let mut after = text;
        if let Some((range, with)) = &outcome.edit {
            after.replace_range(range.clone(), with);
        }
        Some((show(&after, outcome.selection), outcome.clipboard))
    }

    fn after(action: LineAction, marked: &str) -> String {
        run(action, marked).expect("the action does something").0
    }

    #[test]
    fn cutting_without_a_selection_takes_the_line_and_its_newline() {
        assert_eq!(run(CutLine, "a\nb|b\nc"), Some(("a\nc|".into(), Some("bb\n".into()))));
        assert_eq!(run(CutLine, "a\nbbb|\nccccc"), Some(("a\nccc|cc".into(), Some("bbb\n".into()))), "the cursor keeps its column");
        assert_eq!(run(CutLine, "|a\nb"), Some(("|b".into(), Some("a\n".into()))));
    }

    #[test]
    fn cutting_the_last_line_takes_the_newline_before_it_too() {
        assert_eq!(run(CutLine, "a\nb|"), Some(("a|".into(), Some("b\n".into()))));
        assert_eq!(run(CutLine, "only|"), Some(("|".into(), Some("only\n".into()))));
    }

    #[test]
    fn copying_without_a_selection_leaves_the_text_and_cursor() {
        assert_eq!(run(CopyLine, "a\nb|b\nc"), Some(("a\nb|b\nc".into(), Some("bb\n".into()))));
    }

    #[test]
    fn cut_and_copy_with_a_selection_are_left_to_the_editor() {
        assert_eq!(run(CutLine, "a{b}c"), None);
        assert_eq!(run(CopyLine, "a{b}c"), None);
    }

    #[test]
    fn delete_removes_every_touched_line() {
        assert_eq!(after(Delete, "a\nb|\nc"), "a\nc|");
        assert_eq!(after(Delete, "a\n{b\nc}\nd"), "a\nd|");
        assert_eq!(after(Delete, "a\nb|"), "a|");
        assert_eq!(after(Delete, "{a\nb}"), "|");
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
    }

    #[test]
    fn selecting_a_part_of_a_line_selects_the_whole_line() {
        assert_eq!(after(Select, "a\nb{b}b\nc"), "a\n{bbb\n}c");
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
}
