//! File paths in the terminal's text, such as `src/main.rs:42` in Claude's reply, which Ctrl+click opens in the editor.

/// The path-like word at `column` of `row`, the line it names (1-based) after `:12`, `:12:5` or `#L12-30`, and the
/// columns the word covers. Only what looks like a path (a `/`, `\` or a file extension in it, not a URL); whether the
/// file exists is for the caller to check.
pub fn path_at(row: &str, column: usize) -> Option<(String, Option<usize>, std::ops::Range<usize>)> {
    let chars: Vec<char> = row.chars().collect();
    let edge = |c: char| c.is_whitespace() || "\"'`()[]{}<>,│".contains(c);
    if column >= chars.len() || edge(chars[column]) {
        return None;
    }
    let start = chars[..column].iter().rposition(|&c| edge(c)).map_or(0, |i| i + 1);
    let end = chars[column..].iter().position(|&c| edge(c)).map_or(chars.len(), |i| column + i);
    let raw: String = chars[start..end].iter().collect();
    // A Claude Code reference, or the end of a sentence.
    let unmarked = raw.trim_start_matches('@');
    let word = unmarked.trim_end_matches(['.', ';', ':', '!', '?']);
    let first = start + raw.chars().count() - unmarked.chars().count();
    let columns = first..first + word.chars().count();
    if word.contains("://") {
        return None;
    }
    let (path, line) = split_line(word);
    let extension = path.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty() && ext.chars().all(char::is_alphanumeric));
    (path.contains(['/', '\\']) || extension).then(|| (path.to_string(), line, columns))
}

/// `path:12`, `path:12:5`, `path:12-30` or `path#L12-30` into the path and line 12.
fn split_line(word: &str) -> (&str, Option<usize>) {
    if let Some((path, lines)) = word.split_once("#L") {
        return (path, lines.split('-').next().and_then(|n| n.parse().ok()));
    }
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    // `12` or `12-30`.
    let lines = |s: &str| s.split_once('-').map_or(digits(s), |(first, last)| digits(first) && digits(last));
    // `path:12:5`: the column goes first.
    let without_column = match word.rsplit_once(':') {
        Some((rest, column)) if digits(column) && rest.rsplit_once(':').is_some_and(|(_, line)| digits(line)) => rest,
        _ => word,
    };
    // Only digits after the colon, so `C:\x` stays a drive.
    match without_column.rsplit_once(':') {
        Some((path, range)) if lines(range) && !path.is_empty() => (path, range.split('-').next().and_then(|n| n.parse().ok())),
        _ => (word, None),
    }
}

#[cfg(test)]
mod tests {
    use super::path_at;

    /// The path under the `^` in `marker`.
    fn at(row: &str, marker: &str) -> Option<(String, Option<usize>)> {
        path_at(row, marker.find('^').unwrap()).map(|(path, line, _)| (path, line))
    }

    #[test]
    fn finds_the_path_and_line_under_the_pointer() {
        let row = "● Fixed it in src/app/mod.rs:42, see the test.";
        assert_eq!(at(row, "                  ^"), Some(("src/app/mod.rs".into(), Some(42))));
        assert_eq!(at(row, "              ^"), Some(("src/app/mod.rs".into(), Some(42))), "the start of the word");
        assert_eq!(at("Update(README.md)", "        ^"), Some(("README.md".into(), None)));
        assert_eq!(at("  at lib.rs:12:5", "      ^"), Some(("lib.rs".into(), Some(12))), "the column is dropped");
        assert_eq!(at("is (src/lib/app-platforms.ts:2-10) here", "         ^"), Some(("src/lib/app-platforms.ts".into(), Some(2))), "a range of lines");
        assert_eq!(at("explain @src/a.rs#L12-30: why", "          ^"), Some(("src/a.rs".into(), Some(12))));
        assert_eq!(at(r"open C:\work\a.rs.", "      ^"), Some((r"C:\work\a.rs".into(), None)), "a drive, and a full stop after");
    }

    #[test]
    fn says_which_columns_the_path_covers() {
        let row = "see @src/a.rs:42, ok";
        assert_eq!(path_at(row, 8).map(|(_, _, columns)| columns), Some(5..16), "without the @ and the comma");
    }

    #[test]
    fn leaves_words_and_urls_alone() {
        assert_eq!(at("Done. All good", " ^"), None, "a word");
        assert_eq!(at("see https://example.com/a.rs", "        ^"), None);
        assert_eq!(at("two  spaces", "    ^"), None, "between words");
        assert_eq!(at("short", "         ^"), None, "past the end");
    }
}
