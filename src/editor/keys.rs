//! What a key does in the editor. macOS moves by words with Option and to a line's ends with Cmd; elsewhere Ctrl
//! moves by words and Home and End reach the line's ends. Shift with any move selects.

use rune::Tx;

use super::buffer::{Pos, Selection};
use super::{Editor, TAB_WIDTH};
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
        if primary(&m) && !m.alt {
            return match (key, shift) {
                ("a", false) => {
                    self.select(Selection { anchor: Pos::default(), head: self.buffer.last() });
                    true
                }
                ("c", false) => {
                    self.copy(tx, false);
                    true
                }
                ("x", false) => {
                    self.copy(tx, true);
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
                    self.insert(&text, true);
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
        let p = self.sel.head;
        let from = if to_start {
            Pos::new(p.line, 0)
        } else if by_word {
            self.buffer.word_left(p)
        } else if p.col > 0 && p.col <= self.buffer.indent_end(p.line) && self.buffer.line(p.line)[..p.col].bytes().all(|b| b == b' ') {
            let col = self.column(p);
            Pos::new(p.line, p.col - (col - 1) % TAB_WIDTH - 1)
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
        let unit = if indent.starts_with('\t') { "\t".to_string() } else { " ".repeat(TAB_WIDTH) };
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

    /// Tab: spaces to the next tab stop, or a level of indentation for every selected line.
    fn tab(&mut self) {
        let (first, last) = self.selected_lines();
        if self.sel.is_empty() || first == last && self.sel.range().0.col > self.buffer.indent_end(first) {
            let col = self.column(self.sel.range().0);
            return self.insert(&" ".repeat(TAB_WIDTH - col % TAB_WIDTH), false);
        }
        self.reindent(first, last, |line| Some(format!("{}{line}", " ".repeat(TAB_WIDTH))));
    }

    /// Shift+Tab: a level of indentation off every selected line, or the caret's.
    fn outdent(&mut self) {
        let (first, last) = self.selected_lines();
        self.reindent(first, last, |line| {
            let spaces = line.len() - line.trim_start_matches(' ').len();
            let cut = if line.starts_with('\t') { 1 } else { spaces.min(TAB_WIDTH) };
            (cut > 0).then(|| line[cut..].to_string())
        });
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

    /// Copy (and with `cut`, remove) the selection, or with none the caret's whole line.
    fn copy(&mut self, tx: &mut Tx, cut: bool) {
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
        tx.copy(text);
        if cut {
            self.replace(a, b, "", false);
        }
    }
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
        assert_eq!(e.buffer.text(), "    if x {\n        \n    }");
        assert_eq!(e.caret(), Pos::new(1, 8));
        let mut e = editor("  a", 0, 3);
        e.newline();
        assert_eq!(e.buffer.text(), "  a\n  ");
    }

    #[test]
    fn backspace_in_indentation_goes_back_to_the_tab_stop() {
        let mut e = editor("      x", 0, 6);
        e.backspace(false, false);
        assert_eq!(e.buffer.text(), "    x");
        e.backspace(false, false);
        assert_eq!(e.buffer.text(), "x");
        let mut e = editor("ab", 0, 2);
        e.backspace(false, false);
        assert_eq!(e.buffer.text(), "a", "outside indentation, one character");
    }

    #[test]
    fn tab_indents_selected_lines_and_shift_tab_takes_it_back() {
        let mut e = editor("a\n  b\nc", 0, 0);
        e.select(Selection { anchor: Pos::new(0, 0), head: Pos::new(1, 3) });
        e.tab();
        assert_eq!(e.buffer.text(), "    a\n      b\nc");
        assert_eq!(e.sel, Selection { anchor: Pos::new(0, 4), head: Pos::new(1, 7) });
        e.outdent();
        e.outdent();
        assert_eq!(e.buffer.text(), "a\nb\nc");
        e.undo();
        assert_eq!(e.buffer.text(), "a\n  b\nc", "each outdent is one undo step");
        e.undo();
        e.undo();
        assert_eq!(e.buffer.text(), "a\n  b\nc", "and so is the indent");
        let mut e = editor("ab", 0, 1);
        e.tab();
        assert_eq!(e.buffer.text(), "a   b", "a caret inside text gets spaces to the tab stop");
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
    fn only_text_keys_type() {
        let key = |spec: &str| Keystroke::parse(spec).unwrap();
        assert_eq!(typed(&key("a")).as_deref(), Some("a"));
        assert_eq!(typed(&key("cmd-a")), None);
        assert_eq!(typed(&key("ctrl-a")), None);
        assert_eq!(typed(&key("enter")), None);
    }
}
