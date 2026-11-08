//! What a key does in the editor. macOS moves by words with Option and to a line's ends with Cmd; elsewhere Ctrl
//! moves by words and Home and End reach the line's ends. Shift with any move selects.

use rune::Tx;

use super::buffer::{Pos, Selection};
use super::lines::LineAction;
use super::Editor;
use crate::app::Workspace;
use crate::terminal::keys::{Keystroke, Modifiers};

const MAC: bool = cfg!(target_os = "macos");

/// The modifier of app shortcuts (copy, undo, select all): Cmd on macOS, Ctrl elsewhere.
fn primary(m: &Modifiers) -> bool {
    if MAC { m.platform && !m.control } else { m.control && !m.platform }
}

/// The modifier that moves by words: Option on macOS, Ctrl elsewhere.
fn word(m: &Modifiers) -> bool {
    if MAC { m.alt && !m.platform && !m.control } else { m.control && !m.alt }
}

impl Editor {
    /// A key while the editor has the keyboard. Returns whether it was used; the rest go on to the app.
    pub fn on_key(&mut self, ks: &Keystroke, tx: &mut Tx) -> bool {
        let m = ks.modifiers;
        let key = ks.key.as_str();
        let plain = !m.control && !m.platform && !m.alt && !m.shift;
        self.hover = None;
        if plain && self.completion_key(key) {
            return true;
        }
        let used = self.key_inner(ks, tx);
        // Typing on narrows the completion list; moving off the word closes it.
        self.filter_completions();
        used
    }

    fn key_inner(&mut self, ks: &Keystroke, tx: &mut Tx) -> bool {
        let m = ks.modifiers;
        let key = ks.key.as_str();
        let shift = m.shift;
        // Ctrl+Space on every platform, as in VS Code: completions for the word at the caret.
        if m.control && !m.alt && !m.platform && !shift && key == "space" {
            self.requests.push(super::assist::Request::Complete(self.sel.head));
            return true;
        }
        if key == "f12" && !m.control && !m.platform && !m.alt && !shift {
            self.requests.push(super::assist::Request::Definition(self.sel.head));
            return true;
        }
        // Emacs keys that macOS text fields share: Ctrl+A and Ctrl+E go to the line's ends.
        if MAC && m.control && !m.platform && !m.alt && matches!(key, "a" | "e") {
            let p = self.sel.head;
            let target = if key == "a" { self.home(p) } else { Pos::new(p.line, self.buffer.line(p.line).len()) };
            let anchor = if shift { self.sel.anchor } else { target };
            self.select(Selection { anchor, head: target });
            return true;
        }
        if let Some(action) = line_action(&m, key) {
            self.edit_lines(action);
            return true;
        }
        if primary(&m) && !m.alt {
            return match (key, shift) {
                ("a", false) => {
                    self.select_all();
                    true
                }
                ("c", false) => {
                    tx.copy(&self.take_for_clipboard(false));
                    true
                }
                ("x", false) => {
                    tx.copy(&self.take_for_clipboard(true));
                    true
                }
                ("v", false) => {
                    let key = self.key.clone();
                    tx.read_clipboard(move |ws: &mut Workspace, content, _| {
                        if let Some(e) = ws.editor_mut(&key) {
                            e.paste_text(content.text());
                        }
                    });
                    true
                }
                ("z", false) => {
                    self.undo();
                    true
                }
                ("]", false) => {
                    self.indent_lines();
                    true
                }
                ("[", false) => {
                    self.outdent();
                    true
                }
                ("z", true) | ("y", false) => {
                    self.redo();
                    true
                }
                ("left" | "right" | "up" | "down" | "home" | "end" | "backspace" | "delete", _) => self.motion_key(ks),
                _ => false,
            };
        }
        if self.motion_key(ks) {
            return true;
        }
        match key {
            "escape" if !self.sel.is_empty() => {
                self.select(Selection::caret(self.sel.head));
                true
            }
            "enter" if !m.control && !m.platform && !m.alt => {
                self.newline();
                true
            }
            "tab" if !m.control && !m.platform && !m.alt => {
                if shift {
                    self.outdent();
                } else {
                    self.tab();
                }
                true
            }
            _ => match typed(ks) {
                Some(text) => {
                    if !self.type_paired(&text) {
                        self.insert(&text, true);
                    }
                    true
                }
                None => false,
            },
        }
    }

    /// Caret moves, selection, and the keys that delete.
    fn motion_key(&mut self, ks: &Keystroke) -> bool {
        let m = ks.modifiers;
        let by_word = word(&m);
        let to_end = MAC && m.platform && !m.control && !m.alt;
        let doc = if MAC { to_end } else { m.control && !m.alt };
        let p = self.sel.head;
        let target = match ks.key.as_str() {
            "left" if !self.sel.is_empty() && !m.shift && !by_word && !to_end => self.sel.range().0,
            "right" if !self.sel.is_empty() && !m.shift && !by_word && !to_end => self.sel.range().1,
            "left" if to_end => self.home(p),
            "right" if to_end => Pos::new(p.line, self.buffer.line(p.line).len()),
            "left" if by_word => self.buffer.word_left(p),
            "right" if by_word => self.buffer.word_right(p),
            "left" => self.buffer.prev(p),
            "right" => self.buffer.next(p),
            "up" if to_end => Pos::default(),
            "down" if to_end => self.buffer.last(),
            "up" => return self.vertical(-1, m.shift),
            "down" => return self.vertical(1, m.shift),
            "pageup" => return self.vertical(-self.page(), m.shift),
            "pagedown" => return self.vertical(self.page(), m.shift),
            "home" if doc => Pos::default(),
            "end" if doc => self.buffer.last(),
            "home" => self.home(p),
            "end" => Pos::new(p.line, self.buffer.line(p.line).len()),
            "backspace" => {
                self.backspace(by_word, to_end);
                return true;
            }
            "delete" => {
                self.delete_forward(by_word, to_end);
                return true;
            }
            _ => return false,
        };
        let anchor = if m.shift { self.sel.anchor } else { target };
        self.select(Selection { anchor, head: target });
        true
    }

    /// Home goes to where the line's text starts, and from there to the very start.
    fn home(&self, p: Pos) -> Pos {
        let indent = self.buffer.indent_end(p.line);
        Pos::new(p.line, if p.col == indent { 0 } else { indent })
    }

    /// The lines one page moves.
    fn page(&self) -> isize {
        ((self.area[1] / self.metrics().line_h).floor() as isize - 2).max(1)
    }

    /// Up or down by `lines`, keeping the column the caret started in.
    fn vertical(&mut self, lines: isize, extend: bool) -> bool {
        let p = self.sel.head;
        let goal = self.goal.unwrap_or_else(|| self.column(p));
        let last = self.buffer.line_count() - 1;
        let target = match p.line.checked_add_signed(lines) {
            None => Pos::default(),
            Some(line) if line > last => self.buffer.last(),
            Some(line) => self.at_column(line, goal),
        };
        let anchor = if extend { self.sel.anchor } else { target };
        self.select(Selection { anchor, head: target });
        self.goal = Some(goal);
        true
    }

    /// Backspace: the selection, else the character (a run of indenting spaces back to the tab stop), the word or the
    /// line before the caret.
    fn backspace(&mut self, by_word: bool, to_start: bool) {
        if !self.sel.is_empty() {
            return self.insert("", false);
        }
        if !by_word && !to_start && let Some((a, b)) = self.pair_around_caret() {
            return self.replace(a, b, "", true);
        }
        let p = self.sel.head;
        let from = if to_start {
            Pos::new(p.line, 0)
        } else if by_word {
            self.buffer.word_left(p)
        } else {
            self.buffer.prev(p)
        };
        if from != p {
            self.replace(from, p, "", !by_word && !to_start);
        }
    }

    fn delete_forward(&mut self, by_word: bool, to_end: bool) {
        if !self.sel.is_empty() {
            return self.insert("", false);
        }
        let p = self.sel.head;
        let to = if to_end {
            Pos::new(p.line, self.buffer.line(p.line).len())
        } else if by_word {
            self.buffer.word_right(p)
        } else {
            self.buffer.next(p)
        };
        if to != p {
            self.replace(p, to, "", false);
        }
    }

    /// Enter keeps the line's indentation, and adds a level after an opening bracket (putting a closing one that
    /// follows the caret on its own line).
    fn newline(&mut self) {
        let (a, b) = self.sel.range();
        let line = self.buffer.line(a.line);
        let indent = &line[..self.buffer.indent_end(a.line).min(a.col)];
        let before = line[..a.col].trim_end().chars().last();
        let after = self.buffer.line(b.line)[b.col..].chars().next();
        let opens = matches!(before, Some('{' | '(' | '['));
        let unit = if indent.starts_with('\t') { "\t".to_string() } else { " ".repeat(self.indent_unit) };
        let text = match (opens, after) {
            (true, Some('}' | ')' | ']')) => {
                let inner = format!("\n{indent}{unit}");
                let text = format!("{inner}\n{indent}");
                self.replace(a, b, &text, false);
                let caret = Pos::new(a.line + 1, inner.len() - 1);
                self.sel = Selection::caret(caret);
                return;
            }
            (true, _) => format!("\n{indent}{unit}"),
            _ => format!("\n{indent}"),
        };
        self.replace(a, b, &text, false);
    }

    /// Tab: a level of indentation at the caret, or onto every line a selection touches.
    fn tab(&mut self) {
        if self.sel.is_empty() {
            return self.insert(&" ".repeat(self.indent_unit), false);
        }
        self.indent_lines();
    }

    /// A level of indentation onto every line the selection touches, or the caret's (Primary+], and Tab over a
    /// selection).
    fn indent_lines(&mut self) {
        let (first, last) = self.touched_lines();
        let unit = " ".repeat(self.indent_unit);
        self.reindent(first, last, |line| Some(format!("{unit}{line}")));
    }

    /// Shift+Tab and Primary+[: a level of indentation off every line the selection touches, or the caret's. A line
    /// that doesn't start with a whole level keeps what it has.
    fn outdent(&mut self) {
        let (first, last) = self.touched_lines();
        let unit = " ".repeat(self.indent_unit);
        self.reindent(first, last, |line| line.strip_prefix(unit.as_str()).map(str::to_string));
    }

    /// The lines from the selection's start to its end, the end's line included even when the selection only reaches
    /// its start, as main's editor indented them.
    fn touched_lines(&self) -> (usize, usize) {
        let (a, b) = self.sel.range();
        (a.line, b.line)
    }

    /// Rewrite lines `first..=last` with `change` (none: the line stays) as one undo step, keeping them selected.
    fn reindent(&mut self, first: usize, last: usize, change: impl Fn(&str) -> Option<String>) {
        let old: Vec<String> = (first..=last).map(|l| self.buffer.line(l).to_string()).collect();
        let new: Vec<String> = old.iter().map(|l| change(l).unwrap_or_else(|| l.clone())).collect();
        if new == old {
            return;
        }
        let shift = |line: usize, col: usize| -> usize {
            let (o, n) = (&old[line - first], &new[line - first]);
            (col + n.len()).saturating_sub(o.len())
        };
        let (anchor, head) = (self.sel.anchor, self.sel.head);
        let end = Pos::new(last, self.buffer.line(last).len());
        self.replace(Pos::new(first, 0), end, &new.join("\n"), false);
        let moved = |p: Pos| if (first..=last).contains(&p.line) { Pos::new(p.line, shift(p.line, p.col)) } else { p };
        self.sel = Selection { anchor: self.buffer.clamp(moved(anchor)), head: self.buffer.clamp(moved(head)) };
    }

    pub fn select_all(&mut self) {
        self.select(Selection { anchor: Pos::default(), head: self.buffer.last() });
    }

    /// The text to copy: the selection, or with none the caret's whole line. With `cut`, that text goes too.
    pub fn take_for_clipboard(&mut self, cut: bool) -> String {
        let (a, b, text) = if self.sel.is_empty() {
            let line = self.sel.head.line;
            let sel = self.line_selection(line, line);
            let (a, b) = sel.range();
            let mut text = self.buffer.slice(a, b);
            if !text.ends_with('\n') {
                text.push('\n');
            }
            (a, b, text)
        } else {
            let (a, b) = self.sel.range();
            (a, b, self.buffer.slice(a, b))
        };
        match cut {
            // The line goes with its line end, and the last line with the one before it.
            true if self.sel.is_empty() => {
                self.edit_lines(LineAction::Delete);
            }
            true => self.replace(a, b, "", false),
            false => {}
        }
        text
    }
}

/// The line action a key runs, with VS Code's keys. Primary+Shift+Up / Down select by line, since Primary+Shift+Left /
/// Right already select by word or to the line's ends.
fn line_action(m: &Modifiers, key: &str) -> Option<LineAction> {
    use LineAction::*;
    let alt_only = m.alt && !m.control && !m.platform;
    Some(match (key, m.shift) {
        ("up", false) if alt_only => MoveUp,
        ("down", false) if alt_only => MoveDown,
        ("up", true) if alt_only => DuplicateUp,
        ("down", true) if alt_only => DuplicateDown,
        ("k", true) if primary(m) && !m.alt => Delete,
        ("l", false) if primary(m) && !m.alt => Select,
        ("up", true) if primary(m) && !m.alt => ExtendUp,
        ("down", true) if primary(m) && !m.alt => ExtendDown,
        _ => return None,
    })
}

/// The text a key types, if it types one: plain keys, Shift, and on macOS Option (⌥P types π); AltGr elsewhere.
fn typed(ks: &Keystroke) -> Option<String> {
    let m = &ks.modifiers;
    let text = ks.key_char.as_ref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control))?;
    let plain = !m.control && !m.platform && !m.function && (!m.alt || MAC);
    let altgr = !MAC && m.control && m.alt && !m.platform;
    (plain || altgr).then(|| text.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str, line: usize, col: usize) -> Editor {
        let mut e = Editor::new("e", text);
        e.select(Selection::caret(Pos::new(line, col)));
        e
    }

    #[test]
    fn enter_keeps_the_indent_and_opens_a_block_between_brackets() {
        let mut e = editor("    if x {}", 0, 10);
        e.newline();
        assert_eq!(e.buffer.text(), "    if x {\n      \n    }", "one level is two spaces, as in main");
        assert_eq!(e.caret(), Pos::new(1, 6));
        let mut e = editor("  a", 0, 3);
        e.newline();
        assert_eq!(e.buffer.text(), "  a\n  ");
    }

    #[test]
    fn backspace_takes_one_character_even_in_indentation() {
        let mut e = editor("    x", 0, 4);
        e.backspace(false, false);
        assert_eq!(e.buffer.text(), "   x", "as main's editor did");
        let mut e = editor("ab", 0, 2);
        e.backspace(false, false);
        assert_eq!(e.buffer.text(), "a");
    }

    #[test]
    fn tab_indents_selected_lines_and_shift_tab_takes_it_back() {
        let mut e = editor("a\n  b\nc", 0, 0);
        e.select(Selection { anchor: Pos::new(0, 0), head: Pos::new(1, 3) });
        e.tab();
        assert_eq!(e.buffer.text(), "  a\n    b\nc");
        assert_eq!(e.sel, Selection { anchor: Pos::new(0, 2), head: Pos::new(1, 5) });
        e.outdent();
        e.outdent();
        assert_eq!(e.buffer.text(), "a\nb\nc", "a line without a whole level keeps what it has");
        e.undo();
        assert_eq!(e.buffer.text(), "a\n  b\nc", "each outdent is one undo step");
        e.undo();
        e.undo();
        assert_eq!(e.buffer.text(), "a\n  b\nc", "and so is the indent");
        let mut e = editor("ab", 0, 1);
        e.tab();
        assert_eq!(e.buffer.text(), "a  b", "a caret gets one level of spaces where it is");
        let mut e = editor("ab\ncd", 0, 0);
        e.select(Selection { anchor: Pos::new(0, 1), head: Pos::new(1, 0) });
        e.tab();
        assert_eq!(e.buffer.text(), "  ab\n  cd", "a selection indents every line it touches, the last one too");
        let mut e = editor(" a", 0, 1);
        e.outdent();
        assert_eq!(e.buffer.text(), " a", "less than a level stays");
    }

    #[test]
    fn up_and_down_come_back_to_the_column_after_a_short_line() {
        let mut e = editor("long line\nab\nanother line", 0, 7);
        e.vertical(1, false);
        assert_eq!(e.caret(), Pos::new(1, 2));
        e.vertical(1, false);
        assert_eq!(e.caret(), Pos::new(2, 7));
        e.vertical(-5, false);
        assert_eq!(e.caret(), Pos::new(0, 0), "past the top is the start");
    }

    #[test]
    fn copy_and_cut_without_a_selection_take_the_whole_line() {
        let mut e = editor("a\nbb\nc", 1, 1);
        assert_eq!(e.take_for_clipboard(false), "bb\n");
        assert_eq!((e.buffer.text().as_str(), e.caret()), ("a\nbb\nc", Pos::new(1, 1)), "copying leaves text and caret");
        assert_eq!(e.take_for_clipboard(true), "bb\n");
        assert_eq!(e.buffer.text(), "a\nc");
        let mut e = editor("a\nb", 1, 1);
        assert_eq!(e.take_for_clipboard(true), "b\n", "the last line too, with a line end");
        assert_eq!(e.buffer.text(), "a", "and the line end before it goes");
        let mut e = editor("abc", 0, 0);
        e.select(Selection { anchor: Pos::new(0, 1), head: Pos::new(0, 2) });
        assert_eq!(e.take_for_clipboard(true), "b", "a selection is cut as it is");
        assert_eq!(e.buffer.text(), "ac");
    }

    #[test]
    fn brackets_indent_and_outdent_the_caret_line_wherever_the_caret_is() {
        let mut e = editor("a\n  b", 1, 3);
        e.indent_lines();
        assert_eq!(e.buffer.text(), "a\n    b");
        assert_eq!(e.caret(), Pos::new(1, 5), "the caret stays on its letter");
        e.outdent();
        e.outdent();
        assert_eq!(e.buffer.text(), "a\nb");
    }

    #[test]
    fn line_actions_have_vs_codes_keys() {
        let action = |spec: &str| {
            let ks = Keystroke::parse(spec).unwrap();
            line_action(&ks.modifiers, &ks.key)
        };
        let primary = if MAC { "cmd" } else { "ctrl" };
        assert_eq!(action("alt-up"), Some(LineAction::MoveUp));
        assert_eq!(action("alt-shift-down"), Some(LineAction::DuplicateDown));
        assert_eq!(action(&format!("{primary}-shift-k")), Some(LineAction::Delete));
        assert_eq!(action(&format!("{primary}-l")), Some(LineAction::Select));
        assert_eq!(action(&format!("{primary}-shift-up")), Some(LineAction::ExtendUp));
        assert_eq!(action("up"), None);
        assert_eq!(action(&format!("{primary}-up")), None, "to the start of the file, as before");
        assert_eq!(action(&format!("{primary}-k")), None);
    }

    #[test]
    fn only_text_keys_type() {
        let key = |spec: &str| Keystroke::parse(spec).unwrap();
        assert_eq!(typed(&key("a")).as_deref(), Some("a"));
        assert_eq!(typed(&key("cmd-a")), None);
        assert_eq!(typed(&key("ctrl-a")), None);
        assert_eq!(typed(&key("enter")), None);
    }
}
