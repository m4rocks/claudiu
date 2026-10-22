//! The stand-in for Claude Code's prompt while `claude` starts: what the user types lands in a `Draft`, drawn like
//! Claude Code's box, and moves into the real prompt the moment it appears (`TerminalView::hand_over`).

use std::time::Duration;

use gpui::{Animation, AnimationExt, IntoElement, div, prelude::*, px};

use crate::terminal_view::FONT;
use crate::theme;

/// The message being typed. Edits are by character; `at` is a byte offset on a character boundary.
#[derive(Default)]
pub struct Draft {
    text: String,
    at: usize,
    /// Enter was pressed: the message goes to Claude Code as soon as it is ready, and edits are locked.
    pub sent: bool,
}

impl Draft {
    pub fn insert(&mut self, s: &str) {
        if self.sent {
            return;
        }
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.at, &s);
        self.at += s.len();
    }

    pub fn backspace(&mut self) {
        if let (false, Some(c)) = (self.sent, self.text[..self.at].chars().next_back()) {
            self.at -= c.len_utf8();
            self.text.remove(self.at);
        }
    }

    pub fn delete(&mut self) {
        if !self.sent && self.at < self.text.len() {
            self.text.remove(self.at);
        }
    }

    pub fn left(&mut self) {
        if let Some(c) = self.text[..self.at].chars().next_back() {
            self.at -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.at..].chars().next() {
            self.at += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.at = self.text[..self.at].rfind('\n').map_or(0, |i| i + 1);
    }

    pub fn end(&mut self) {
        self.at += self.text[self.at..].find('\n').unwrap_or(self.text.len() - self.at);
    }

    /// Enter: queue what is typed (an empty message is not queued).
    pub fn send(&mut self) {
        self.sent = !self.text.is_empty();
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Hand the message over: its text and whether Enter was pressed.
    pub fn take(&mut self) -> (String, bool) {
        let sent = self.sent;
        let text = std::mem::take(&mut self.text);
        self.clear();
        (text, sent)
    }

    /// Each line as (before the cursor, under it, after it); only the cursor's line has `Some`.
    fn rows(&self) -> Vec<(&str, Option<char>, &str)> {
        let mut start = 0;
        self.text
            .split('\n')
            .map(|line| {
                let (from, end) = (start, start + line.len());
                start = end + 1;
                if (from..=end).contains(&self.at) {
                    let (before, rest) = line.split_at(self.at - from);
                    let mut rest = rest.chars();
                    let under = rest.next().unwrap_or(' ');
                    (before, Some(under), rest.as_str())
                } else {
                    (line, None, "")
                }
            })
            .collect()
    }
}

// Claude Code's own spinner glyphs. ✳ (U+2733) has an emoji presentation that Windows draws as a green emoji;
// Claude Code uses `*` there too.
#[cfg(target_os = "windows")]
const FRAMES: [&str; 6] = ["·", "✢", "*", "✶", "✻", "✽"];
#[cfg(not(target_os = "windows"))]
const FRAMES: [&str; 6] = ["·", "✢", "✳", "✶", "✻", "✽"];

/// Over the terminal until Claude Code draws: the previous screen if there is one, what is being loaded, and a
/// prompt box that takes the user's typing. `booted` is whether `claude` has started to answer.
pub fn overlay(draft: &Draft, preview: Option<&str>, elapsed: Duration, booted: bool) -> impl IntoElement {
    let (text, dim, faint) = (theme::hsla(theme::TEXT), theme::hsla(theme::TEXT_DIM), theme::hsla(theme::TEXT_FAINT));
    let hint = if draft.sent {
        "Queued: it goes to Claude Code the moment it is ready · Esc takes it back"
    } else if draft.text.is_empty() {
        "You can type while Claude Code loads · Enter queues your message"
    } else {
        "Enter queues it · Shift+Enter for a new line"
    };
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .bg(theme::hsla(theme::TERMINAL_BG))
        // Where the session left off, faint, until the real screen replaces it.
        .when_some(preview, |d, text| {
            d.child(
                div()
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .font_family(FONT)
                    .text_size(px(14.0))
                    .text_color(faint)
                    .opacity(0.45)
                    .children(text.lines().map(|l| div().child(if l.is_empty() { " ".to_string() } else { l.to_string() }))),
            )
        })
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .child(div().font_family(FONT).text_size(px(28.0)).text_color(theme::hsla(theme::ACCENT)).with_animation(
                    "claude-loading",
                    Animation::new(Duration::from_millis(1200)).repeat(),
                    |d, t| {
                        // Ping-pong through the frames like Claude Code does.
                        let i = ((t * 10.0) as usize).min(9);
                        d.child(FRAMES[if i < 6 { i } else { 10 - i }])
                    },
                ))
                .child(div().text_size(px(13.0)).text_color(dim).child(if booted {
                    "Loading skills, plugins and MCP servers…"
                } else {
                    "Starting Claude Code…"
                }))
                .child(div().text_size(px(11.0)).text_color(faint).child(format!("{}s", elapsed.as_secs()))),
        )
        .child(
            div()
                .px(px(16.0))
                .pb(px(12.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(if draft.sent { theme::hsla(theme::ACCENT) } else { theme::hsla(0x3a3a3a) })
                        .bg(theme::hsla(theme::TERMINAL_BG))
                        .font_family(FONT)
                        .text_size(px(14.0))
                        .text_color(if draft.sent { dim } else { text })
                        .child(div().text_color(dim).child(">"))
                        .child(div().flex_1().min_w_0().overflow_hidden().flex().flex_col().children(draft.rows().into_iter().map(
                            |(before, under, after)| {
                                div().flex().h(px(20.0)).whitespace_nowrap().child(before.to_string()).when_some(under, |d, c| {
                                    // The cursor: a block over the character it stands on (hidden once queued).
                                    d.child(div().when(!draft.sent, |d| d.bg(text).text_color(theme::hsla(theme::TERMINAL_BG))).child(c.to_string()))
                                        .child(after.to_string())
                                })
                            },
                        ))),
                )
                .child(div().px(px(4.0)).text_size(px(12.0)).text_color(faint).child(hint)),
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
        assert_eq!(d.rows().last(), Some(&("> abl", Some(' '), "")));
        d.backspace();
        d.left();
        d.delete();
        assert_eq!(d.text, "hél\n> a");
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
}
