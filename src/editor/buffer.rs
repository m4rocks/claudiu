//! The text an editor holds, as lines without their line breaks, and the edits made to it with their undo history.
//! Positions are a line and a byte offset in it, always on a character boundary. Pure data: no UI, no files.

use std::time::{Duration, Instant};

/// A place in the text: `line` from 0, `col` a byte offset into that line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// What is selected: from `anchor`, where it began, to `head`, where the caret is. Empty when they are the same.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
}

impl Selection {
    pub const fn caret(at: Pos) -> Self {
        Self { anchor: at, head: at }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The selection's start and end, in text order.
    pub fn range(&self) -> (Pos, Pos) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }
}

/// One replacement: at `at`, `removed` gave way to `inserted`.
#[derive(Clone, Debug)]
struct Edit {
    at: Pos,
    removed: String,
    inserted: String,
}

/// What one undo takes back: its edits in the order they were made, and the cursors before and after them (the
/// first is the main one; there are more after an edit at several cursors).
#[derive(Clone, Debug)]
struct Step {
    edits: Vec<Edit>,
    before: Vec<Selection>,
    after: Vec<Selection>,
    /// A run of typing grows this step while the user keeps typing (see `Buffer::replace`).
    typing: bool,
    at: Instant,
    /// Names the text this step leaves behind, so undoing back to the saved text counts as unchanged.
    id: u64,
}

/// Typing within this long of the last keystroke undoes together with it.
const TYPING_PAUSE: Duration = Duration::from_millis(1000);

pub struct Buffer {
    lines: Vec<String>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// Bumped by every change, so what was worked out from the text (highlighting, a save) can tell it is stale.
    version: u64,
    /// The step id of the text when it was last loaded or saved (0 is the text as loaded).
    saved_id: u64,
    next_id: u64,
    /// An edit at several cursors is under way (`begin_group`): its replacements make one step.
    group: Option<Group>,
    /// What the last undo or redo replaced, in order: where each replacement started, where the text it replaced
    /// ended, and where the new text ends.
    pub changed: Vec<(Pos, Pos, Pos)>,
}

/// The cursors an edit at several cursors started from, and whether it has made its step yet.
struct Group {
    before: Vec<Selection>,
    started: bool,
}

impl Buffer {
    /// `text` with `\n` line breaks (a file's `\r\n` is turned into `\n` before it gets here).
    pub fn new(text: &str) -> Self {
        Self { lines: text.split('\n').map(str::to_string).collect(), undo: vec![], redo: vec![], version: 0, saved_id: 0, next_id: 1, group: None, changed: vec![] }
    }

    fn current_id(&self) -> u64 {
        self.undo.last().map_or(0, |s| s.id)
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map_or("", String::as_str)
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Changed since it was loaded or saved (undoing back to that point counts as unchanged).
    pub fn is_dirty(&self) -> bool {
        self.current_id() != self.saved_id
    }

    /// The text now is what is on disk. Typing after this starts a new undo step, so it makes the text dirty again.
    pub fn mark_saved(&mut self) {
        self.break_typing();
        self.saved_id = self.current_id();
    }

    pub fn last(&self) -> Pos {
        let line = self.lines.len() - 1;
        Pos::new(line, self.lines[line].len())
    }

    /// `p` moved inside the text and onto a character boundary.
    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        let text = &self.lines[line];
        let mut col = p.col.min(text.len());
        while !text.is_char_boundary(col) {
            col -= 1;
        }
        Pos::new(line, col)
    }

    /// The text between `a` and `b`, with `\n` between lines.
    pub fn slice(&self, a: Pos, b: Pos) -> String {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if a.line == b.line {
            return self.lines[a.line][a.col..b.col].to_string();
        }
        let mut out = self.lines[a.line][a.col..].to_string();
        for line in &self.lines[a.line + 1..b.line] {
            out.push('\n');
            out.push_str(line);
        }
        out.push('\n');
        out.push_str(&self.lines[b.line][..b.col]);
        out
    }

    /// Where the text after `at` ends up once `text` is put there.
    fn end_of(at: Pos, text: &str) -> Pos {
        match text.rfind('\n') {
            Some(i) => Pos::new(at.line + text.matches('\n').count(), text.len() - i - 1),
            None => Pos::new(at.line, at.col + text.len()),
        }
    }

    /// Replace `a..b` with `text` without touching the history. Returns the end of the inserted text.
    fn splice(&mut self, a: Pos, b: Pos, text: &str) -> Pos {
        let tail = self.lines[b.line][b.col..].to_string();
        let head = self.lines[a.line][..a.col].to_string();
        let mut new: Vec<String> = text.split('\n').map(str::to_string).collect();
        new[0].insert_str(0, &head);
        let end = Self::end_of(a, text);
        new.last_mut().expect("split gives at least one piece").push_str(&tail);
        self.lines.splice(a.line..=b.line, new);
        self.version += 1;
        end
    }

    /// Replace the text between `a` and `b` with `text`, as one undoable step from `before` to the caret after it.
    /// `typing` joins it to the previous step when that was typing too, ended where this begins, and came moments
    /// ago, so undo takes back a typed word rather than a letter.
    pub fn replace(&mut self, a: Pos, b: Pos, text: &str, before: Selection, typing: bool) -> Pos {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let (a, b) = (self.clamp(a), self.clamp(b));
        let removed = self.slice(a, b);
        let end = self.splice(a, b, text);
        let edit = Edit { at: a, removed, inserted: text.to_string() };
        let after = vec![Selection::caret(end)];
        self.redo.clear();
        let now = Instant::now();
        // A new line is a step of its own, neither joining the typing before it nor letting the next keystroke join it.
        let typing = typing && !text.contains('\n');
        // In a group, every replacement after the first goes into the step the first made or joined.
        let in_group = self.group.as_ref().is_some_and(|g| g.started);
        let before = match &mut self.group {
            Some(g) => {
                g.started = true;
                g.before.clone()
            }
            None => vec![before],
        };
        let joins = in_group
            || typing
                && self.undo.last().is_some_and(|s| {
                    s.typing && s.after == before && before.iter().all(Selection::is_empty) && now.duration_since(s.at) < TYPING_PAUSE
                });
        match self.undo.last_mut() {
            Some(step) if joins => {
                step.typing &= typing;
                step.edits.push(edit);
                step.after = after;
                step.at = now;
            }
            _ => {
                let id = self.next_id;
                self.next_id += 1;
                self.undo.push(Step { edits: vec![edit], before, after, typing, at: now, id });
            }
        }
        end
    }

    /// Make the replacements until `end_group` one undo step, from the cursors `before` (the main one first).
    pub fn begin_group(&mut self, before: Vec<Selection>) {
        self.group = Some(Group { before, started: false });
    }

    /// The edit at several cursors is done and left them at `after`.
    pub fn end_group(&mut self, after: Vec<Selection>) {
        if self.group.take().is_some_and(|g| g.started)
            && let Some(step) = self.undo.last_mut()
        {
            step.after = after;
        }
    }

    /// Ends the typing run, so the next keystroke starts a new undo step (the caret moved, or the file was saved).
    pub fn break_typing(&mut self) {
        if let Some(step) = self.undo.last_mut() {
            step.typing = false;
        }
    }

    /// Take back the last step; the cursors to show, the main one first, or `None` with nothing to undo.
    pub fn undo(&mut self) -> Option<Vec<Selection>> {
        let step = self.undo.pop()?;
        self.changed.clear();
        for e in step.edits.iter().rev() {
            let end = Self::end_of(e.at, &e.inserted);
            let new_end = self.splice(e.at, end, &e.removed);
            self.changed.push((e.at, end, new_end));
        }
        let before = step.before.clone();
        self.redo.push(step);
        Some(before)
    }

    pub fn redo(&mut self) -> Option<Vec<Selection>> {
        let mut step = self.redo.pop()?;
        self.changed.clear();
        for e in &step.edits {
            let end = Self::end_of(e.at, &e.removed);
            let new_end = self.splice(e.at, end, &e.inserted);
            self.changed.push((e.at, end, new_end));
        }
        step.typing = false;
        let after = step.after.clone();
        self.undo.push(step);
        Some(after)
    }

    /// The position one character before `p`, across line breaks.
    pub fn prev(&self, p: Pos) -> Pos {
        if p.col == 0 {
            return if p.line == 0 { p } else { Pos::new(p.line - 1, self.lines[p.line - 1].len()) };
        }
        let text = &self.lines[p.line][..p.col];
        let len = text.chars().next_back().map_or(0, char::len_utf8);
        Pos::new(p.line, p.col - len)
    }

    /// The position one character after `p`, across line breaks.
    pub fn next(&self, p: Pos) -> Pos {
        let text = &self.lines[p.line];
        if p.col >= text.len() {
            return if p.line + 1 >= self.lines.len() { p } else { Pos::new(p.line + 1, 0) };
        }
        let len = text[p.col..].chars().next().map_or(0, char::len_utf8);
        Pos::new(p.line, p.col + len)
    }

    /// The start of the word before `p`: past any spaces, then past one run of word characters or of other symbols.
    /// At a line's start it goes to the end of the line above.
    pub fn word_left(&self, p: Pos) -> Pos {
        if p.col == 0 {
            return self.prev(p);
        }
        let text = &self.lines[p.line][..p.col];
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut i = chars.len();
        while i > 0 && chars[i - 1].1.is_whitespace() {
            i -= 1;
        }
        if i > 0 {
            let kind = class(chars[i - 1].1);
            while i > 0 && class(chars[i - 1].1) == kind && !chars[i - 1].1.is_whitespace() {
                i -= 1;
            }
        }
        Pos::new(p.line, chars.get(i).map_or(0, |c| c.0))
    }

    /// The end of the word after `p`, as `word_left` the other way.
    pub fn word_right(&self, p: Pos) -> Pos {
        let text = &self.lines[p.line];
        if p.col >= text.len() {
            return self.next(p);
        }
        let rest: Vec<(usize, char)> = text[p.col..].char_indices().collect();
        let mut i = 0;
        while i < rest.len() && rest[i].1.is_whitespace() {
            i += 1;
        }
        if i < rest.len() {
            let kind = class(rest[i].1);
            while i < rest.len() && class(rest[i].1) == kind && !rest[i].1.is_whitespace() {
                i += 1;
            }
        }
        Pos::new(p.line, p.col + rest.get(i).map_or(text.len() - p.col, |c| c.0))
    }

    /// The word, or run of symbols or of spaces, around `p` (a double click selects it).
    pub fn word_at(&self, p: Pos) -> (Pos, Pos) {
        let text = &self.lines[p.line];
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        if chars.is_empty() {
            return (p, p);
        }
        // The character under the caret, or the one before it at the line's end.
        let mut i = chars.iter().position(|c| c.0 >= p.col).unwrap_or(chars.len() - 1);
        if i == chars.len() {
            i -= 1;
        }
        let kind = (class(chars[i].1), chars[i].1.is_whitespace());
        let same = |c: char| (class(c), c.is_whitespace()) == kind;
        let mut start = i;
        while start > 0 && same(chars[start - 1].1) {
            start -= 1;
        }
        let mut end = i + 1;
        while end < chars.len() && same(chars[end].1) {
            end += 1;
        }
        (Pos::new(p.line, chars[start].0), Pos::new(p.line, chars.get(end).map_or(text.len(), |c| c.0)))
    }

    /// Where the line's text starts after its indentation.
    pub fn indent_end(&self, line: usize) -> usize {
        let text = &self.lines[line];
        text.len() - text.trim_start_matches([' ', '\t']).len()
    }
}

/// Word characters, and everything else that isn't a space, run together for word moves.
fn class(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(line: usize, col: usize) -> Selection {
        Selection::caret(Pos::new(line, col))
    }

    #[test]
    fn replacing_text_across_lines_keeps_what_is_around_it() {
        let mut b = Buffer::new("one\ntwo\nthree");
        let end = b.replace(Pos::new(0, 1), Pos::new(2, 2), "X\nY", caret(0, 1), false);
        assert_eq!(b.text(), "oX\nYree");
        assert_eq!(end, Pos::new(1, 1));
        assert_eq!(b.slice(Pos::new(0, 1), Pos::new(1, 1)), "X\nY");
    }

    #[test]
    fn undo_and_redo_restore_the_text_and_the_selection() {
        let mut b = Buffer::new("hello\nworld");
        b.replace(Pos::new(0, 0), Pos::new(1, 0), "", caret(1, 0), false);
        assert_eq!(b.text(), "world");
        assert_eq!(b.undo(), Some(vec![caret(1, 0)]));
        assert_eq!(b.text(), "hello\nworld");
        assert_eq!(b.redo(), Some(vec![caret(0, 0)]));
        assert_eq!(b.text(), "world");
        assert!(b.redo().is_none(), "nothing more to redo");
    }

    #[test]
    fn a_typed_word_undoes_at_once_but_a_new_line_starts_a_new_step() {
        let mut b = Buffer::new("");
        let mut at = Pos::new(0, 0);
        for c in ["a", "b", "c"] {
            at = b.replace(at, at, c, Selection::caret(at), true);
        }
        let at = b.replace(at, at, "\n", Selection::caret(at), true);
        b.replace(at, at, "d", Selection::caret(at), true);
        assert_eq!(b.text(), "abc\nd");
        b.undo();
        assert_eq!(b.text(), "abc\n");
        b.undo();
        assert_eq!(b.text(), "abc");
        b.undo();
        assert_eq!(b.text(), "", "the three letters were one step");
    }

    #[test]
    fn moving_the_caret_ends_the_typing_run() {
        let mut b = Buffer::new("");
        let at = b.replace(Pos::default(), Pos::default(), "a", caret(0, 0), true);
        b.break_typing();
        b.replace(at, at, "b", Selection::caret(at), true);
        b.undo();
        assert_eq!(b.text(), "a");
    }

    #[test]
    fn undoing_back_to_the_saved_text_is_not_dirty() {
        let mut b = Buffer::new("x");
        b.mark_saved();
        b.replace(Pos::new(0, 1), Pos::new(0, 1), "y", caret(0, 1), false);
        assert!(b.is_dirty());
        b.undo();
        assert!(!b.is_dirty(), "back to the saved text");
        b.redo();
        assert!(b.is_dirty());
        b.mark_saved();
        assert!(!b.is_dirty());
        let end = b.replace(Pos::new(0, 2), Pos::new(0, 2), "z", caret(0, 2), true);
        assert!(b.is_dirty(), "typing right after a save is a change");
        b.undo();
        assert!(!b.is_dirty());
        assert_eq!(end, Pos::new(0, 3));
    }

    #[test]
    fn characters_move_by_whole_characters_and_across_lines() {
        let b = Buffer::new("aé\nb");
        assert_eq!(b.next(Pos::new(0, 1)), Pos::new(0, 3), "é is two bytes");
        assert_eq!(b.next(Pos::new(0, 3)), Pos::new(1, 0));
        assert_eq!(b.prev(Pos::new(1, 0)), Pos::new(0, 3));
        assert_eq!(b.prev(Pos::new(0, 0)), Pos::new(0, 0));
        assert_eq!(b.next(Pos::new(1, 1)), Pos::new(1, 1));
        assert_eq!(b.clamp(Pos::new(9, 9)), Pos::new(1, 1));
        assert_eq!(b.clamp(Pos::new(0, 2)), Pos::new(0, 1), "inside é goes back to its start");
    }

    #[test]
    fn words_stop_at_spaces_and_at_changes_between_letters_and_symbols() {
        let b = Buffer::new("let x_1 = foo.bar();");
        assert_eq!(b.word_right(Pos::new(0, 0)), Pos::new(0, 3));
        assert_eq!(b.word_right(Pos::new(0, 3)), Pos::new(0, 7), "x_1 is one word");
        assert_eq!(b.word_right(Pos::new(0, 13)), Pos::new(0, 14), "the dot alone");
        assert_eq!(b.word_left(Pos::new(0, 20)), Pos::new(0, 17), "`();` is one run of symbols");
        assert_eq!(b.word_left(Pos::new(0, 4)), Pos::new(0, 0));
        assert_eq!(b.word_at(Pos::new(0, 15)), (Pos::new(0, 14), Pos::new(0, 17)));
        assert_eq!(b.word_at(Pos::new(0, 20)), (Pos::new(0, 17), Pos::new(0, 20)), "at the end, the run before it");
        assert_eq!(Buffer::new("").word_at(Pos::default()), (Pos::default(), Pos::default()));
    }
}
