#[cfg(test)]
use std::ops::Range;
use std::path::PathBuf;

use crate::harness::thread::{Entry, ToolCall, parse_subagent_report};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunKind {
    Exploration,
    Work,
}

pub(super) fn run_kind(entry: &Entry) -> Option<RunKind> {
    match entry {
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

/// A worktree switch marks the end of the preceding checkout's messages.
pub(super) fn workspace_at<'a>(entries: &'a [Entry], row: usize, current: &'a std::path::Path) -> &'a std::path::Path {
    entries.iter().skip(row.saturating_add(1)).find_map(|entry| match entry {
        Entry::WorkspaceBoundary(path) => Some(path.as_path()),
        _ => None,
    }).unwrap_or(current)
}

pub(super) fn file_link(target: &str) -> Option<(PathBuf, Option<u32>)> {
    let (path, line) = target.rsplit_once("#L").map_or((target, None), |(path, line)| {
        (path, line.split('-').next().and_then(|value| value.parse::<u32>().ok()).filter(|value| *value > 0))
    });
    if path.starts_with("file:") {
        return url::Url::parse(path).ok()?.to_file_path().ok().map(|path| (path, line));
    }
    let parsed = PathBuf::from(path);
    if path.is_empty() || path.starts_with("//") || path.starts_with('#') || path.chars().any(char::is_control)
        || (!parsed.is_absolute() && path.contains(':'))
    {
        return None;
    }
    Some((parsed, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_links_stay_in_the_checkout_of_their_message() {
        use std::path::Path;
        let entries = [
            Entry::Agent("first".into()),
            Entry::WorkspaceBoundary("one".into()),
            Entry::Agent("second".into()),
            Entry::WorkspaceBoundary("two".into()),
            Entry::Agent("third".into()),
        ];
        assert_eq!(workspace_at(&entries, 0, Path::new("three")), Path::new("one"));
        assert_eq!(workspace_at(&entries, 2, Path::new("three")), Path::new("two"));
        assert_eq!(workspace_at(&entries, 4, Path::new("three")), Path::new("three"));
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
        let range = |ix| tool_run(entries.len(), ix, |row| run_kind(&entries[row]));
        for ix in 0..5 {
            assert_eq!(range(ix), Some(0..5));
        }
        assert_eq!(range(5), None);
        assert_eq!(range(6), Some(6..8));
        assert_eq!(range(7), Some(6..8));
        assert_eq!(range(8), None);
    }
}
