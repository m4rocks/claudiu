//! The stand-in for Claude Code's prompt while `claude` starts: what the user types lands in a `Draft`, drawn like
//! Claude Code's box, and moves into the real prompt the moment it appears (`TerminalView::hand_over`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Animation, AnimationExt, IntoElement, Keystroke, div, prelude::*, px};

use crate::glyphs;
use crate::terminal_view::FONT;
use crate::theme;

/// The message being typed. Edits are by character; `at` is a byte offset on a character boundary.
#[derive(Default)]
pub struct Draft {
    text: String,
    at: usize,
    /// Bytes at the front that were already typed into Claude Code: they stay, edits go after them.
    frozen: usize,
    /// Enter was pressed: the message goes to Claude Code as soon as it is ready, and edits are locked.
    pub sent: bool,
    /// Pasted images, saved as files; the text holds `[Image #n]` for the nth, as Claude Code shows it. They go into
    /// Claude Code one by one through its own image-paste key (`TerminalView::hand_over`).
    images: Vec<PathBuf>,
    /// How many images were handed over, which is the number Claude Code gives the next.
    shown: usize,
}

/// One piece of the draft as it is typed into Claude Code.
#[derive(Debug, PartialEq)]
pub enum Step {
    Text(String),
    /// An image file, and the number Claude Code will show it as.
    Image(PathBuf, usize),
}

/// `[Image #n] ` at the start of `s`: n and the chip's length. Claude Code puts a space after its chip, so the chip
/// here includes it and a delete takes both.
fn chip_at(s: &str) -> Option<(usize, usize)> {
    let rest = s.strip_prefix("[Image #")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &rest[digits..];
    (digits > 0 && rest.starts_with(']')).then(|| (s[8..8 + digits].parse().unwrap_or(0), 8 + digits + 1 + usize::from(rest[1..].starts_with(' '))))
}

/// A path as Claude Code takes it when pasted: quoted if it has spaces.
pub fn quote_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.contains(' ') { format!("\"{s}\"") } else { s.into_owned() }
}

impl Draft {
    pub fn insert(&mut self, s: &str) {
        if self.sent {
            return;
        }
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.at = self.at.max(self.frozen);
        self.text.insert_str(self.at, &s);
        self.at += s.len();
    }

    /// An image pasted at the cursor.
    pub fn add_image(&mut self, path: PathBuf) {
        if !self.sent {
            self.images.push(path);
            self.insert(&format!("[Image #{}] ", self.images.len()));
        }
    }

    /// Where an `[Image #n]` ends at the cursor (and is not frozen) starts.
    fn chip_before(&self) -> Option<usize> {
        let head = &self.text[self.frozen..self.at];
        let i = head.rfind("[Image #")?;
        let tail = &head[i + 8..];
        let digits = tail.strip_suffix("] ").or_else(|| tail.strip_suffix(']'))?;
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(self.frozen + i)
    }

    /// Where an `[Image #n]` starting at the cursor ends.
    fn chip_after(&self) -> Option<usize> {
        chip_at(&self.text[self.at..]).map(|(_, len)| self.at + len)
    }

    pub fn backspace(&mut self) {
        if let (false, Some(start)) = (self.sent, self.chip_before()) {
            self.text.drain(start..self.at);
            self.at = start;
        } else if let (false, true, Some(c)) = (self.sent, self.at > self.frozen, self.text[..self.at].chars().next_back()) {
            self.at -= c.len_utf8();
            self.text.remove(self.at);
        }
    }

    pub fn delete(&mut self) {
        if let (false, Some(end)) = (self.sent, self.chip_after()) {
            self.text.drain(self.at..end);
        } else if !self.sent && self.at < self.text.len() {
            self.text.remove(self.at);
        }
    }

    pub fn left(&mut self) {
        if let Some(start) = self.chip_before() {
            self.at = start;
        } else if let (true, Some(c)) = (self.at > self.frozen, self.text[..self.at].chars().next_back()) {
            self.at -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(end) = self.chip_after() {
            self.at = end;
        } else if let Some(c) = self.text[self.at..].chars().next() {
            self.at += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.at = self.text[..self.at].rfind('\n').map_or(0, |i| i + 1).max(self.frozen);
    }

    pub fn end(&mut self) {
        self.at += self.text[self.at..].find('\n').unwrap_or(self.text.len() - self.at);
    }

    /// Where the word before the cursor starts (words are split by whitespace); never into the frozen text.
    fn word_before(&self) -> usize {
        let head = self.text[self.frozen..self.at].trim_end();
        self.frozen + head.trim_end_matches(|c: char| !c.is_whitespace()).len()
    }

    /// Where the word after the cursor ends.
    fn word_after(&self) -> usize {
        let tail = &self.text[self.at..];
        let word = tail.trim_start();
        self.at + (tail.len() - word.len()) + word.find(char::is_whitespace).unwrap_or(word.len())
    }

    pub fn word_left(&mut self) {
        self.at = self.word_before();
    }

    pub fn word_right(&mut self) {
        self.at = self.word_after();
    }

    pub fn delete_word_back(&mut self) {
        if !self.sent {
            let start = self.chip_before().unwrap_or_else(|| self.word_before());
            self.text.drain(start..self.at);
            self.at = start;
        }
    }

    pub fn delete_word_forward(&mut self) {
        if !self.sent {
            let end = self.chip_after().unwrap_or_else(|| self.word_after());
            self.text.drain(self.at..end);
        }
    }

    /// Delete back to the start of the line (the line break itself when already there).
    pub fn kill_to_line_start(&mut self) {
        if !self.sent {
            let line = self.text[..self.at].rfind('\n').map_or(0, |i| i + 1).max(self.frozen);
            let start = if line == self.at { self.at.saturating_sub(1).max(self.frozen) } else { line };
            self.text.drain(start..self.at);
            self.at = start;
        }
    }

    /// Delete to the end of the line (the line break itself when already there).
    pub fn kill_to_line_end(&mut self) {
        if !self.sent {
            let end = self.at + self.text[self.at..].find('\n').unwrap_or(self.text.len() - self.at);
            self.text.drain(self.at..if end == self.at && end < self.text.len() { end + 1 } else { end });
        }
    }

    pub fn text_start(&mut self) {
        self.at = self.frozen;
    }

    pub fn text_end(&mut self) {
        self.at = self.text.len();
    }

    /// The same column one line up. shortcut: by line break, not by wrapped row.
    pub fn up(&mut self) {
        let Some(nl) = self.text[..self.at].rfind('\n') else { return };
        let col = self.text[nl + 1..self.at].chars().count();
        let prev = self.text[..nl].rfind('\n').map_or(0, |i| i + 1);
        self.at = (prev + byte_at(&self.text[prev..nl], col)).max(self.frozen);
    }

    pub fn down(&mut self) {
        let Some(nl) = self.text[self.at..].find('\n').map(|i| self.at + i) else { return };
        let col = self.text[self.text[..self.at].rfind('\n').map_or(0, |i| i + 1)..self.at].chars().count();
        let next = nl + 1;
        let end = self.text[next..].find('\n').map_or(self.text.len(), |i| next + i);
        self.at = next + byte_at(&self.text[next..end], col);
    }

    /// A key pressed while Claude Code starts. Word moves and deletes take Ctrl (Windows, Linux) or Alt (macOS),
    /// line moves and deletes Cmd (macOS); the Ctrl letters are Claude Code's (readline) ones. Nothing reaches the program.
    pub fn key(&mut self, ks: &Keystroke) {
        let m = &ks.modifiers;
        let (word, cmd) = (m.control || m.alt, m.platform);
        match ks.key.as_str() {
            "space" if !m.control && !m.alt => self.insert(" "),
            "enter" if m.shift || m.alt => self.insert("\n"),
            "enter" => self.send(),
            "escape" => self.sent = false,
            "backspace" if cmd => self.kill_to_line_start(),
            "backspace" if word => self.delete_word_back(),
            "backspace" => self.backspace(),
            "delete" if cmd => self.kill_to_line_end(),
            "delete" if word => self.delete_word_forward(),
            "delete" => self.delete(),
            "left" if cmd => self.home(),
            "left" if word => self.word_left(),
            "left" => self.left(),
            "right" if cmd => self.end(),
            "right" if word => self.word_right(),
            "right" => self.right(),
            "home" if m.control || cmd => self.text_start(),
            "home" => self.home(),
            "end" if m.control || cmd => self.text_end(),
            "end" => self.end(),
            "up" if cmd => self.text_start(),
            "up" => self.up(),
            "down" if cmd => self.text_end(),
            "down" => self.down(),
            "a" if m.control && !m.alt => self.home(),
            "e" if m.control && !m.alt => self.end(),
            "b" if m.control && !m.alt => self.left(),
            "f" if m.control && !m.alt => self.right(),
            "d" if m.control && !m.alt => self.delete(),
            "h" if m.control && !m.alt => self.backspace(),
            "k" if m.control && !m.alt => self.kill_to_line_end(),
            "u" if m.control && !m.alt => self.kill_to_line_start(),
            "w" if m.control && !m.alt => self.delete_word_back(),
            "j" if m.control && !m.alt => self.insert("\n"),
            "c" if m.control && !m.alt => self.clear(),
            "b" if m.alt && !m.control => self.word_left(),
            "f" if m.alt && !m.control => self.word_right(),
            "d" if m.alt && !m.control => self.delete_word_forward(),
            _ => {}
        }
    }

    /// Enter: queue what is typed (an empty message is not queued).
    pub fn send(&mut self) {
        self.sent = !self.text.is_empty();
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// What was typed since the last call, in pieces to type into Claude Code. That text stays drawn here, but is
    /// no longer editable, until the real prompt shows it.
    pub fn next_steps(&mut self) -> Vec<Step> {
        let rest = self.text[self.frozen..].to_string();
        (self.frozen, self.at) = (self.text.len(), self.text.len());
        let (mut steps, mut plain, mut s) = (Vec::new(), String::new(), rest.as_str());
        while let Some(c) = s.chars().next() {
            if let Some((n, len)) = chip_at(s)
                && let Some(path) = n.checked_sub(1).and_then(|i| self.images.get(i)).cloned()
            {
                if !plain.is_empty() {
                    steps.push(Step::Text(std::mem::take(&mut plain)));
                }
                self.shown += 1;
                steps.push(Step::Image(path, self.shown));
                s = &s[len..];
            } else {
                plain.push(c);
                s = &s[c.len_utf8()..];
            }
        }
        if !plain.is_empty() {
            steps.push(Step::Text(plain));
        }
        steps
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// What was typed after `freeze` (all of it if never frozen), and whether Enter was pressed. Empties the draft.
    pub fn take(&mut self) -> (String, bool) {
        let (rest, sent) = (self.text[self.frozen..].to_string(), self.sent);
        self.clear();
        (rest, sent)
    }

    /// The screen rows of the text, wrapped at `width` characters, each as (before the cursor, under it, after it);
    /// only the cursor's row has `Some`. shortcut: counts characters, not cell widths, so wide characters wrap late.
    fn rows(&self, width: usize) -> Vec<(&str, Option<char>, &str)> {
        let width = width.max(1);
        let mut out = Vec::new();
        let mut start = 0;
        for line in self.text.split('\n') {
            let (from, end) = (start, start + line.len());
            start = end + 1;
            let mut chunks = Vec::new();
            let (mut s, mut n) = (0, 0);
            for (i, _) in line.char_indices() {
                if n == width {
                    chunks.push((s, i));
                    (s, n) = (i, 0);
                }
                n += 1;
            }
            chunks.push((s, line.len()));
            // A cursor after a full last row starts the next one.
            if self.at == end && n == width {
                chunks.push((line.len(), line.len()));
            }
            let last = chunks.len() - 1;
            for (k, (cs, ce)) in chunks.into_iter().enumerate() {
                let (cs, ce) = (from + cs, from + ce);
                let row = &self.text[cs..ce];
                out.push(if self.at >= cs && (self.at < ce || (k == last && self.at == ce)) {
                    let (before, rest) = row.split_at(self.at - cs);
                    let mut rest = rest.chars();
                    let under = rest.next().unwrap_or(' ');
                    (before, Some(under), rest.as_str())
                } else {
                    (row, None, "")
                });
            }
        }
        out
    }
}

// Claude Code's own spinner glyphs. ✳ (U+2733) has an emoji presentation that Windows draws as a green emoji;
// Claude Code uses `*` there too.
#[cfg(target_os = "windows")]
const FRAMES: [&str; 6] = ["·", "✢", "*", "✶", "✻", "✽"];
#[cfg(not(target_os = "windows"))]
const FRAMES: [&str; 6] = ["·", "✢", "✳", "✶", "✻", "✽"];

/// The byte offset of the `col`th character of `line` (its end when shorter).
fn byte_at(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

/// The terminal's character grid as it is painted, so the skeleton lands on the same cells.
#[derive(Clone, Copy)]
pub struct Grid {
    pub cell_w: f32,
    pub line_h: f32,
    pub cols: u16,
    pub rows: u16,
}

/// Over the terminal until Claude Code is ready: laid out like its start screen (prompt box between two rules) on the
/// terminal's own grid, at the bottom where Claude Code keeps it, with what is being loaded above it. `phase` says what is going on.
pub fn overlay(draft: &Draft, elapsed: Duration, phase: &str, grid: Grid) -> impl IntoElement {
    let (text, dim, faint) = (theme::hsla(theme::TEXT), theme::hsla(theme::TEXT_DIM), theme::hsla(theme::TEXT_FAINT));
    let lh = grid.line_h;
    let hint = if draft.sent {
        "Queued: it goes to Claude Code the moment it is ready · Esc takes it back"
    } else if draft.is_empty() {
        "You can type while Claude Code loads · Enter queues your message"
    } else {
        "Enter queues it · Shift+Enter for a new line"
    };
    // A rule is drawn the way the terminal draws `─` (a rectangle per cell, `glyphs.rs`), not as font glyphs, whose
    // edges overlap and show as bright dots.
    let (rule_y, rule_h) = glyphs::rects('─', grid.cell_w.round(), lh).and_then(|r| r.first().map(|r| (r.y, r.h))).unwrap_or((lh / 2.0, 1.0));
    let rule = move || {
        div().relative().flex_none().h(px(lh)).child(
            div().absolute().left_0().top(px(rule_y)).h(px(rule_h)).w(px((grid.cell_w * f32::from(grid.cols)).round())).bg(faint),
        )
    };
    let rows = draft.rows((grid.cols as usize).saturating_sub(2));
    // The terminal fills whole rows only, so the grid ends above the window's bottom edge: the skeleton stops there too.
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(theme::hsla(theme::TERMINAL_BG))
        .child(
            div()
                .relative()
                .w(px((grid.cell_w * f32::from(grid.cols)).round()))
                .h(px(lh * f32::from(grid.rows)))
                .flex()
                .flex_col()
                // Claude Code keeps its prompt at the bottom of the screen.
                .justify_end()
                .overflow_hidden()
                .font_family(FONT)
                .text_size(crate::scale::px(14.0))
                .line_height(px(lh))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(8.0))
                        .pb(px(lh))
                        .pl(px(14.0))
                        .text_color(dim)
                        .child(div().text_color(theme::hsla(theme::ACCENT)).with_animation(
                            "claude-loading",
                            Animation::new(Duration::from_millis(1200)).repeat(),
                            |d, t| {
                                // Ping-pong through the frames like Claude Code does.
                                let i = ((t * 10.0) as usize).min(9);
                                d.child(FRAMES[if i < 6 { i } else { 10 - i }])
                            },
                        ))
                        .child(format!("{phase} {}s", elapsed.as_secs())),
                )
                .child(rule())
                .children(rows.into_iter().enumerate().map(|(i, (before, under, after))| {
                    div()
                        .flex()
                        .flex_none()
                        .h(px(lh))
                        .whitespace_nowrap()
                        .text_color(if draft.sent { dim } else { text })
                        .child(if i == 0 { "❯ " } else { "  " })
                        .child(before.to_string())
                        .when_some(under, |d, c| {
                            // The cursor: a block over the character it stands on (hidden once queued).
                            d.child(div().when(!draft.sent, |d| d.bg(text).text_color(theme::hsla(theme::TERMINAL_BG))).child(c.to_string()))
                                .child(after.to_string())
                        })
                }))
                .child(rule())
                .child(div().flex_none().h(px(lh)).whitespace_nowrap().text_color(faint).child(format!("  {hint}"))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_by_character_and_line() {
        let mut d = Draft::default();
        d.insert("héllo");
        d.backspace();
        d.left();
        d.insert("\nab");
        assert_eq!(d.text, "hél\nabl");
        d.home();
        d.insert("> ");
        assert_eq!(d.text, "hél\n> abl");
        d.end();
        d.delete();
        assert_eq!(d.text, "hél\n> abl");
        assert_eq!(d.rows(80).last(), Some(&("> abl", Some(' '), "")));
        d.backspace();
        d.left();
        d.delete();
        assert_eq!(d.text, "hél\n> a");
    }

    #[test]
    fn long_lines_wrap_and_the_cursor_follows() {
        let mut d = Draft::default();
        d.insert("abcdefgh\nij");
        let rows = d.rows(3);
        let text: Vec<_> = rows.iter().map(|(b, c, a)| format!("{b}{}{a}", c.map_or(String::new(), |c| c.to_string()))).collect();
        assert_eq!(text, ["abc", "def", "gh", "ij "]);
        assert_eq!(rows.last(), Some(&("ij", Some(' '), "")));
        // A cursor after a full row opens the next one.
        d.clear();
        d.insert("abc");
        assert_eq!(d.rows(3), [("abc", None, ""), ("", Some(' '), "")]);
        d.left();
        assert_eq!(d.rows(3), [("ab", Some('c'), "")]);
    }

    fn typed(d: &mut Draft, keys: &str) {
        for k in keys.split(' ') {
            d.key(&Keystroke::parse(k).unwrap());
        }
    }

    #[test]
    fn word_and_line_shortcuts() {
        let mut d = Draft::default();
        d.insert("one two  three\nfour");
        typed(&mut d, "ctrl-backspace");
        assert_eq!(d.text, "one two  three\n");
        typed(&mut d, "ctrl-backspace");
        assert_eq!(d.text, "one two  ");
        typed(&mut d, "ctrl-left");
        assert_eq!(d.at, 4);
        typed(&mut d, "ctrl-delete");
        assert_eq!(d.text, "one   ");
        typed(&mut d, "alt-right alt-backspace");
        assert_eq!(d.text, "");
        d.clear();
        d.insert("ab cd\nef");
        typed(&mut d, "ctrl-u");
        assert_eq!(d.text, "ab cd\n");
        typed(&mut d, "ctrl-u");
        assert_eq!(d.text, "ab cd");
        typed(&mut d, "ctrl-a ctrl-k");
        assert_eq!(d.text, "");
    }

    #[test]
    fn up_and_down_keep_the_column() {
        let mut d = Draft::default();
        d.insert("abcd\nef\nghij");
        typed(&mut d, "up");
        assert_eq!(d.at, 7);
        typed(&mut d, "up");
        assert_eq!(d.at, 2);
        typed(&mut d, "down down");
        assert_eq!(d.at, 10);
        typed(&mut d, "ctrl-home");
        assert_eq!(d.at, 0);
    }

    #[test]
    fn frozen_text_survives_word_deletes() {
        let mut d = Draft::default();
        d.insert("hello world");
        d.next_steps();
        typed(&mut d, "ctrl-backspace ctrl-u");
        assert_eq!(d.text, "hello world");
        d.insert("x yz");
        typed(&mut d, "ctrl-backspace ctrl-backspace ctrl-backspace");
        assert_eq!(d.text, "hello world");
    }

    #[test]
    fn image_chips_move_and_delete_whole_and_become_steps() {
        let mut d = Draft::default();
        d.insert("see ");
        d.add_image(PathBuf::from("a.png"));
        d.insert(" and ");
        d.add_image(PathBuf::from("b b.png"));
        assert_eq!(d.text, "see [Image #1]  and [Image #2] ");
        d.backspace();
        assert_eq!(d.text, "see [Image #1]  and ");
        d.home();
        d.right();
        d.right();
        d.right();
        d.right();
        d.delete();
        assert_eq!(d.text, "see  and ");
        d.add_image(PathBuf::from("c.png"));
        assert_eq!(d.text, "see [Image #3]  and ");
        assert!(d.next_steps().len() == 3);
        d.insert("x");
        d.add_image(PathBuf::from("d.png"));
        assert_eq!(d.next_steps(), [Step::Text("x".into()), Step::Image("d.png".into(), 2)]);
    }

    #[test]
    fn sent_locks_edits_until_taken_or_cleared() {
        let mut d = Draft::default();
        d.send();
        assert!(!d.sent, "an empty message is not queued");
        d.insert("hi");
        d.send();
        d.insert("x");
        d.backspace();
        assert_eq!(d.take(), ("hi".to_string(), true));
        assert_eq!(d.take(), (String::new(), false));
    }

    #[test]
    fn frozen_text_stays_and_only_the_rest_is_taken() {
        let mut d = Draft::default();
        d.insert("abc");
        assert_eq!(d.next_steps(), [Step::Text("abc".into())]);
        d.backspace();
        d.left();
        d.home();
        d.insert("de");
        d.left();
        d.left();
        d.left();
        d.backspace();
        assert_eq!(d.text, "abcde");
        assert_eq!(d.take(), ("de".to_string(), false));
    }
}
