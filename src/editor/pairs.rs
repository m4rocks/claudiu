//! Brackets and quotes that close themselves, by the rules main's GPUI editor followed (gpui-kit's default language
//! rules). Typing an opener before whitespace, the end of the line or one of `CLOSE_BEFORE` adds its closer. Typing a
//! closer that is already next to the caret steps over it. Backspace between an empty pair takes both. None of this
//! happens inside a string or a comment, nor after a backslash, and a quote doesn't pair after a letter (`don't`).

use super::buffer::{Pos, Selection};
use super::{Editor, Syntax};

/// The pairs, as gpui-kit's default rules list them.
const PAIRS: [(char, char); 5] = [('(', ')'), ('[', ']'), ('{', '}'), ('"', '"'), ('\'', '\'')];
/// An opener closes itself only when one of these, whitespace or the line's end follows it.
const CLOSE_BEFORE: &str = ";:.,=}])>";

impl Editor {
    /// Typing `text` with nothing selected, when it is an opener or a closer. Returns whether the pair rules handled it;
    /// otherwise the caller types it as usual.
    pub(super) fn type_paired(&mut self, text: &str) -> bool {
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else { return false };
        if !self.sel.is_empty() || self.marked.is_some() {
            return false;
        }
        let at = self.sel.head;
        if self.step_over(c, at) {
            return true;
        }
        let Some(&(open, close)) = PAIRS.iter().find(|(o, _)| *o == c) else { return false };
        let line = self.buffer.line(at.line);
        let next = line[at.col..].chars().next();
        if next.is_some_and(|n| !n.is_whitespace() && !CLOSE_BEFORE.contains(n)) {
            return false;
        }
        let before = line[..at.col].chars().next_back();
        if open == close && (escaped(&line[..at.col]) || before.is_some_and(|b| b.is_alphanumeric() || b == '_')) {
            return false;
        }
        if self.syntax_at(at) != Syntax::Code {
            return false;
        }
        self.replace(at, at, &format!("{open}{close}"), true);
        let opener = at;
        let closer = Pos::new(at.line, at.col + open.len_utf8());
        self.sel = Selection::caret(closer);
        self.pairs.push((opener, closer));
        true
    }

    /// The closer `c` is already next to the caret: move past it instead of typing another.
    fn step_over(&mut self, c: char, at: Pos) -> bool {
        let line = self.buffer.line(at.line);
        if line[at.col..].chars().next() != Some(c) || escaped(&line[..at.col]) {
            return false;
        }
        let Some(&(open, close)) = PAIRS.iter().find(|(_, cl)| *cl == c) else { return false };
        let typed_for_user = self.pairs.iter().any(|(_, cl)| *cl == at);
        let syntax = self.syntax_at(at);
        // A quote closing a string steps over even when the user typed both.
        if !typed_for_user && syntax != Syntax::Code && !(syntax == Syntax::String && open == close) {
            return false;
        }
        self.select(Selection::caret(Pos::new(at.line, at.col + close.len_utf8())));
        true
    }

    /// Backspace between an empty pair, such as `(|)`: the range that takes both halves.
    pub(super) fn pair_around_caret(&self) -> Option<(Pos, Pos)> {
        if !self.sel.is_empty() {
            return None;
        }
        let at = self.sel.head;
        let line = self.buffer.line(at.line);
        let before = line[..at.col].chars().next_back()?;
        let after = line[at.col..].chars().next()?;
        let &(open, close) = PAIRS.iter().find(|&&(o, cl)| o == before && cl == after)?;
        let start = Pos::new(at.line, at.col - open.len_utf8());
        let typed_for_user = self.pairs.contains(&(start, at));
        if escaped(&line[..start.col]) || (!typed_for_user && self.syntax_at(start) != Syntax::Code) {
            return None;
        }
        Some((start, Pos::new(at.line, at.col + close.len_utf8())))
    }

    /// Whether `p` is inside a string or a comment, as the last colouring found. A comment runs to its line's end, so
    /// the end of it counts; a string's closing quote ends it.
    fn syntax_at(&self, p: Pos) -> Syntax {
        let Some(spans) = self.spans.get(p.line) else { return Syntax::Code };
        spans
            .iter()
            .find(|s| match s.syntax {
                Syntax::Code => false,
                Syntax::String => s.bytes.start < p.col && p.col < s.bytes.end,
                Syntax::Comment => s.bytes.start < p.col && p.col <= s.bytes.end,
            })
            .map_or(Syntax::Code, |s| s.syntax)
    }

    /// Keep the pairs the editor closed in step with an edit that replaced `a..b` and ended at `end`. An edit between
    /// the two halves keeps the pair; one that touches either half forgets it.
    pub(super) fn adjust_pairs(&mut self, a: Pos, b: Pos, end: Pos) {
        let moved = |p: Pos| if p < b { p } else if p.line == b.line { Pos::new(end.line, p.col - b.col + end.col) } else { Pos::new(p.line - b.line + end.line, p.col) };
        let next = |p: Pos| Pos::new(p.line, p.col + 1);
        self.pairs.retain_mut(|(open, close)| {
            if b <= *open {
                (*open, *close) = (moved(*open), moved(*close));
            } else if a >= next(*open) && b <= *close {
                *close = moved(*close);
            } else if a < next(*close) {
                return false;
            }
            true
        });
    }
}

/// Whether the character after `before` is escaped: an odd run of backslashes ends `before`.
fn escaped(before: &str) -> bool {
    before.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Span;

    fn typed(text: &str, col: usize, keys: &str) -> Editor {
        let mut e = Editor::new("e", text);
        e.select(Selection::caret(Pos::new(0, col)));
        for c in keys.chars() {
            let c = c.to_string();
            if !e.type_paired(&c) {
                e.insert(&c, true);
            }
        }
        e
    }

    #[test]
    fn an_opener_closes_itself_and_typing_the_closer_steps_over_it() {
        let e = typed("", 0, "(");
        assert_eq!((e.buffer.text().as_str(), e.caret()), ("()", Pos::new(0, 1)));
        let e = typed("", 0, "(a)");
        assert_eq!((e.buffer.text().as_str(), e.caret()), ("(a)", Pos::new(0, 3)), "the closer is stepped over");
        let e = typed("", 0, "f([\"x\"])");
        assert_eq!(e.buffer.text(), "f([\"x\"])");
        let e = typed("x;", 1, "(");
        assert_eq!(e.buffer.text(), "x();", "before one of ;:.,=}}])>");
    }

    #[test]
    fn an_opener_before_a_word_stays_single() {
        assert_eq!(typed("abc", 0, "(").buffer.text(), "(abc");
        assert_eq!(typed("abc", 0, "\"").buffer.text(), "\"abc");
    }

    #[test]
    fn a_quote_after_a_letter_or_a_backslash_stays_single() {
        assert_eq!(typed("don", 3, "'").buffer.text(), "don'");
        assert_eq!(typed("\\", 1, "\"").buffer.text(), "\\\"");
    }

    #[test]
    fn nothing_pairs_inside_a_string_or_a_comment() {
        let mut e = Editor::new("e", "\"ab\" // c ");
        let span = |bytes, syntax| Span { bytes, color: rune::Color::WHITE, syntax };
        e.spans = vec![vec![span(0..4, Syntax::String), span(5..10, Syntax::Comment)]];
        e.select(Selection::caret(Pos::new(0, 2)));
        assert!(!e.type_paired("("), "inside the string");
        e.select(Selection::caret(Pos::new(0, 10)));
        assert!(!e.type_paired("("), "at the end of the comment");
        e.select(Selection::caret(Pos::new(0, 3)));
        assert!(e.type_paired("\""), "a quote closing the string steps over it");
        assert_eq!(e.caret(), Pos::new(0, 4));
    }

    #[test]
    fn backspace_between_an_empty_pair_takes_both() {
        let e = typed("", 0, "(");
        assert_eq!(e.pair_around_caret(), Some((Pos::new(0, 0), Pos::new(0, 2))));
        let mut e = typed("", 0, "(x");
        assert_eq!(e.pair_around_caret(), None, "not empty");
        e.select(Selection::caret(Pos::new(0, 2)));
        assert_eq!(e.pair_around_caret(), None);
    }

    #[test]
    fn a_pair_follows_the_edits_around_it() {
        let mut e = typed("", 0, "(");
        e.replace(Pos::new(0, 0), Pos::new(0, 0), "ab", false);
        assert_eq!(e.pairs, [(Pos::new(0, 2), Pos::new(0, 3))], "text before moves it");
        e.replace(Pos::new(0, 3), Pos::new(0, 3), "x", false);
        assert_eq!(e.pairs, [(Pos::new(0, 2), Pos::new(0, 4))], "text inside moves the closer");
        e.replace(Pos::new(0, 4), Pos::new(0, 5), "", false);
        assert!(e.pairs.is_empty(), "deleting the closer forgets the pair");
    }
}
