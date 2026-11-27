//! Visual Turn Outline for a conversation: lists every visible user prompt with its 1-based turn number
//! and first-line preview so clicking any item jumps directly to that entry in the chat `ListState`.

use gpui::{Context, px};

use super::ThreadView;
use super::user_actions::is_visible_user_entry;
use crate::harness::thread::Entry;

/// Default maximum character length for a turn's single-line outline preview.
const DEFAULT_PREVIEW_CHARS: usize = 72;

/// One visible user turn in the conversation outline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnOutlineItem {
    /// 1-based turn number among visible user prompts.
    pub turn: usize,
    /// Index of this `Entry::User` inside `Thread::entries`.
    pub row: usize,
    /// Single-line preview derived from the prompt's first non-empty line.
    pub preview: String,
}

/// Extract the visual turn outline from a slice of conversation entries.
/// Only visible user prompts (`is_visible_user_entry`) are included; hidden system prompts,
/// subagent reports, assistant replies, tool calls, thoughts, and notices are skipped.
pub fn collect_turn_outline(entries: &[Entry]) -> Vec<TurnOutlineItem> {
    let mut items = Vec::new();
    for (row, entry) in entries.iter().enumerate() {
        if !is_visible_user_entry(entry) {
            continue;
        }
        if let Entry::User(text) = entry {
            items.push(TurnOutlineItem {
                turn: items.len() + 1,
                row,
                preview: turn_preview(text, DEFAULT_PREVIEW_CHARS),
            });
        }
    }
    items
}

/// Build a compact single-line preview from a user prompt: takes the first non-empty line,
/// collapses internal whitespace runs, and truncates to `max_chars` characters with `…`.
pub fn turn_preview(text: &str, max_chars: usize) -> String {
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let collapsed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "(empty message)".to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut chars = collapsed.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        let trimmed = prefix.trim_end();
        format!("{trimmed}…")
    } else {
        prefix
    }
}

impl ThreadView {
    /// Visual turn outline for this thread, in chronological order.
    pub fn turn_outline(&self) -> Vec<TurnOutlineItem> {
        collect_turn_outline(&self.thread.entries)
    }

    /// Scroll the chat `ListState` straight to the user prompt at `row`.
    pub fn jump_to_turn(&mut self, row: usize, cx: &mut Context<Self>) {
        if row >= self.thread.entries.len() {
            return;
        }
        self.chat_glide.cancel();
        self.list.pause_following_tail();
        self.list.scroll_to(gpui::ListOffset {
            item_ix: row,
            offset_in_item: px(0.0),
        });
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::thread::ToolCall;
    use std::path::PathBuf;

    #[test]
    fn collect_turn_outline_includes_only_visible_user_entries() {
        let entries = vec![
            Entry::Notice {
                text: "Continued from previous session".into(),
                error: false,
                event: None,
            },
            Entry::User("[m4code notice] hidden compaction summary".into()),
            Entry::User("First prompt\nwith a second line".into()),
            Entry::Thought("thinking...".into()),
            Entry::Tool(ToolCall::default()),
            Entry::Agent("Here is the answer.".into()),
            Entry::User("[m4code subagent report]\nid: child\nharness: claude\ntitle: Helper\nstate: completed\n\ndone".into()),
            Entry::WorkspaceBoundary(PathBuf::from("/repo")),
            Entry::User("[m4code usage notice] rate limit warning".into()),
            Entry::User("\n  \n  Second   prompt   with   spaces  \nmore lines".into()),
        ];

        let outline = collect_turn_outline(&entries);
        assert_eq!(
            outline,
            vec![
                TurnOutlineItem {
                    turn: 1,
                    row: 2,
                    preview: "First prompt".into(),
                },
                TurnOutlineItem {
                    turn: 2,
                    row: 9,
                    preview: "Second prompt with spaces".into(),
                },
            ]
        );
    }

    #[test]
    fn turn_preview_handles_truncation_unicode_and_empty_text() {
        assert_eq!(turn_preview("   \n\t  ", 20), "(empty message)");
        assert_eq!(turn_preview("short line", 20), "short line");
        assert_eq!(turn_preview("abcdef", 4), "abcd…");
        assert_eq!(turn_preview("café naïve résumé", 6), "café n…");
        assert_eq!(turn_preview("hello   world\nsecond line", 5), "hello…");
    }
}
