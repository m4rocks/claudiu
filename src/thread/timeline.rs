#[cfg(test)]
use std::ops::Range;
use std::path::PathBuf;

use crate::harness::thread::{Entry, ToolCall, parse_subagent_report};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunKind {
    Exploration,
    Work,
}

pub(super) fn run_kind(entry: &Entry, has_card: bool) -> Option<RunKind> {
    match entry {
        Entry::Tool(_) if has_card => None,
        Entry::Tool(call) if matches!(call.kind.as_str(), "read" | "search" | "fetch") => Some(RunKind::Exploration),
        Entry::Tool(_) | Entry::Thought(_) => Some(RunKind::Work),
        Entry::User(text) if parse_subagent_report(text).is_some() => Some(RunKind::Work),
        _ => None,
    }
}

/// The run of steps around `ix`: every thought, tool call and subagent report between two things the agent or the
/// user said folds into one line, whatever kind of step each is.
#[cfg(test)]
pub(super) fn tool_run(len: usize, ix: usize, kind_at: impl Fn(usize) -> Option<RunKind>) -> Option<Range<usize>> {
    kind_at(ix)?;
    let start = (0..ix).rev().take_while(|&row| kind_at(row).is_some()).last().unwrap_or(ix);
    let end = (ix + 1..len).take_while(|&row| kind_at(row).is_some()).last().unwrap_or(ix) + 1;
    Some(start..end)
}

pub(super) fn exploration_label<'a>(calls: impl Iterator<Item = &'a ToolCall>) -> String {
    let (mut reads, mut searches, mut fetches) = (0, 0, 0);
    for call in calls {
        match call.kind.as_str() {
            "read" => reads += 1,
            "search" => searches += 1,
            "fetch" => fetches += 1,
            _ => {}
        }
    }
    let counts = [(reads, "file read", "file reads"), (searches, "search", "searches"), (fetches, "fetch", "fetches")];
    let activity = counts.into_iter().filter(|(count, _, _)| *count > 0)
        .map(|(count, one, many)| format!("{count} {}", if count == 1 { one } else { many }))
        .collect::<Vec<_>>().join(", ");
    format!("Explored: {activity}")
}

/// Split an agent markdown message into ordinary markdown segments and closed ```` ```ui ```` (or `~~~ui`) blocks
/// so inline generative UI renders natively in the transcript with no preview button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum InlineSegment {
    Markdown(String),
    Ui(String),
}

pub(super) fn split_inline_ui(markdown: &str) -> Vec<InlineSegment> {
    let mut segments = Vec::new();
    let mut text_buf = String::new();
    let mut ui_buf = String::new();
    let mut open: Option<(char, usize, bool)> = None;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim();
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let len = marker.map_or(0, |m| trimmed.chars().take_while(|c| *c == m).count());
        if let Some((delimiter, count, is_ui)) = open {
            if marker == Some(delimiter) && len >= count && trimmed[len..].trim().is_empty() {
                if is_ui {
                    let fence = delimiter.to_string().repeat(count);
                    if !text_buf.trim().is_empty() {
                        segments.push(InlineSegment::Markdown(std::mem::take(&mut text_buf)));
                    } else {
                        text_buf.clear();
                    }
                    let body = std::mem::take(&mut ui_buf);
                    segments.push(InlineSegment::Ui(format!("{fence}ui\n{body}{fence}")));
                } else {
                    text_buf.push_str(line);
                }
                open = None;
            } else if is_ui {
                if ui_buf.len() + line.len() < 128 * 1024 {
                    ui_buf.push_str(line);
                }
            } else {
                text_buf.push_str(line);
            }
        } else if len >= 3 && let Some(m) = marker {
            let lang = trimmed[len..].trim();
            if lang.eq_ignore_ascii_case("ui") {
                open = Some((m, len, true));
                ui_buf.clear();
            } else {
                open = Some((m, len, false));
                text_buf.push_str(line);
            }
        } else {
            text_buf.push_str(line);
        }
    }
    if let Some((delimiter, count, true)) = open {
        let fence = delimiter.to_string().repeat(count);
        text_buf.push_str(&format!("{fence}ui\n"));
        text_buf.push_str(&ui_buf);
    }
    if !text_buf.is_empty() || segments.is_empty() {
        segments.push(InlineSegment::Markdown(text_buf));
    }
    segments
}

pub(super) fn file_link(target: &str) -> Option<(PathBuf, Option<u32>)> {
    let (path, line) = target.rsplit_once("#L").map_or((target, None), |(path, line)| {
        (path, line.split('-').next().and_then(|value| value.parse::<u32>().ok()).filter(|value| *value > 0))
    });
    if path.starts_with("file:") {
        return url::Url::parse(path).ok()?.to_file_path().ok().map(|path| (path, line));
    }
    let parsed = PathBuf::from(path);
    if path.is_empty() || path.starts_with('#') || path.chars().any(char::is_control)
        || (!parsed.is_absolute() && (path.contains(':') || path.starts_with("//")))
    {
        return None;
    }
    Some((parsed, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_ui_waits_for_a_complete_ui_fence() {
        assert_eq!(
            split_inline_ui("~~~ui\n<p>Still streaming"),
            vec![InlineSegment::Markdown("~~~ui\n<p>Still streaming".into())]
        );
        assert_eq!(
            split_inline_ui("Before\n```ui\n<card>Hi</card>\n```\nAfter"),
            vec![
                InlineSegment::Markdown("Before\n".into()),
                InlineSegment::Ui("```ui\n<card>Hi</card>\n```".into()),
                InlineSegment::Markdown("After".into()),
            ]
        );
    }

    #[test]
    fn markdown_file_links_keep_the_start_of_a_line_range() {
        assert_eq!(file_link("src/main.rs#L42-L48"), Some((PathBuf::from("src/main.rs"), Some(42))));
        assert!(file_link("javascript:example").is_none());
    }

    #[test]
    fn every_step_between_messages_folds_into_one_run() {
        let tool = |kind: &str| Entry::Tool(ToolCall { kind: kind.into(), ..Default::default() });
        let entries = [
            tool("read"), tool("search"), Entry::Thought("hm".into()), tool("edit"), tool("read"),
            Entry::Agent("halfway".into()), tool("execute"), Entry::Thought("ok".into()), Entry::Agent("done".into()),
        ];
        let range = |ix| tool_run(entries.len(), ix, |row| run_kind(&entries[row], false));
        for ix in 0..5 {
            assert_eq!(range(ix), Some(0..5));
        }
        assert_eq!(range(5), None);
        assert_eq!(range(6), Some(6..8));
        assert_eq!(range(7), Some(6..8));
        assert_eq!(range(8), None);
    }
}
