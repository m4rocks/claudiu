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

pub(super) fn tool_run(len: usize, ix: usize, kind_at: impl Fn(usize) -> Option<RunKind>) -> Option<Range<usize>> {
    let kind = kind_at(ix)?;
    let start = (0..ix).rev().take_while(|&row| kind_at(row) == Some(kind)).last().unwrap_or(ix);
    let end = (ix + 1..len).take_while(|&row| kind_at(row) == Some(kind)).last().unwrap_or(ix) + 1;
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

/// Keep HTML as code in the transcript. An explicitly opened preview uses the same native
/// renderer as visual cards, which does not execute scripts or load external images.
pub(super) fn html_preview(markdown: &str) -> Option<String> {
    let mut open: Option<(char, usize, bool)> = None;
    let mut out = None;
    for line in markdown.lines() {
        let trimmed = line.trim();
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let len = marker.map_or(0, |marker| trimmed.chars().take_while(|c| *c == marker).count());
        if let Some((delimiter, count, html)) = open {
            if marker == Some(delimiter) && len >= count && trimmed[len..].trim().is_empty() {
                if html {
                    let html = out?;
                    let fence = delimiter.to_string().repeat(count);
                    return Some(format!("{fence}ui\n{html}{fence}"));
                }
                open = None;
            } else if html {
                let contents = out.get_or_insert_with(String::new);
                if contents.len() + line.len() >= 128 * 1024 {
                    return None;
                }
                contents.push_str(line);
                contents.push('\n');
            }
        } else if len >= 3 && let Some(marker) = marker {
            open = Some((marker, len, trimmed[len..].trim().eq_ignore_ascii_case("html")));
        }
    }
    None
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
    fn preview_waits_for_a_complete_html_fence() {
        assert!(html_preview("~~~html\n<p>Still streaming").is_none());
        assert_eq!(html_preview("~~~html\n<p>Ready</p>\n~~~").as_deref(), Some("~~~ui\n<p>Ready</p>\n~~~"));
    }

    #[test]
    fn markdown_file_links_keep_the_start_of_a_line_range() {
        assert_eq!(file_link("src/main.rs#L42-L48"), Some((PathBuf::from("src/main.rs"), Some(42))));
        assert!(file_link("javascript:example").is_none());
    }

    #[test]
    fn exploration_runs_stop_at_writes_and_messages() {
        let tool = |kind: &str| Entry::Tool(ToolCall { kind: kind.into(), ..Default::default() });
        let entries = [tool("read"), tool("search"), tool("edit"), tool("read"), Entry::Agent("done".into())];
        let range = |ix| tool_run(entries.len(), ix, |row| run_kind(&entries[row], false));
        assert_eq!(range(0), Some(0..2));
        assert_eq!(range(1), Some(0..2));
        assert_eq!(range(2), Some(2..3));
        assert_eq!(range(3), Some(3..4));
        assert_eq!(range(4), None);
    }
}
