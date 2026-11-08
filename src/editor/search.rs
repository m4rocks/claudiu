//! Find in the open file: where a query occurs, and the matches the editor highlights while the search bar is up
//! (`app::find`). As in main's GPUI editor, matching ignores case for ASCII letters only, and the selection stays
//! where it is; the editor highlights every match, the current one brighter, and scrolls to it.

use super::Editor;
use super::buffer::Pos;

impl Editor {
    /// Every place `query` occurs, in order, without overlaps. A query may span lines.
    pub fn find_all(&self, query: &str, ignore_case: bool) -> Vec<(Pos, Pos)> {
        if query.is_empty() {
            return vec![];
        }
        let text = self.buffer.text();
        // ASCII lowering keeps every byte where it was, so offsets in the copies are offsets in the text.
        let (haystack, needle) = if ignore_case { (text.to_ascii_lowercase(), query.to_ascii_lowercase()) } else { (text.clone(), query.to_string()) };
        let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        let pos = |at: usize| {
            let line = starts.partition_point(|&s| s <= at) - 1;
            Pos::new(line, at - starts[line])
        };
        haystack.match_indices(needle.as_str()).map(|(at, m)| (pos(at), pos(at + m.len()))).collect()
    }

    /// The first line on screen.
    pub fn first_visible_line(&self) -> usize {
        self.row_line((self.scroll[1] / self.metrics().line_h).floor().max(0.0) as usize).0
    }

    /// Highlight `found`, `current` brighter, and with `reveal` bring the current one into view. Nothing is selected.
    pub fn show_found(&mut self, found: Vec<(Pos, Pos)>, current: Option<usize>, reveal: bool) {
        if let Some(&(a, _)) = current.and_then(|i| found.get(i)).filter(|_| reveal) {
            self.reveal_pos(a);
        }
        self.found = found;
        self.found_current = current;
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ignore_ascii_case_only_when_asked_and_may_span_lines() {
        let e = Editor::new("e", "Foo foo\nfoo bar\nÉé");
        assert_eq!(e.find_all("foo", false), [(Pos::new(0, 4), Pos::new(0, 7)), (Pos::new(1, 0), Pos::new(1, 3))]);
        assert_eq!(e.find_all("foo", true).len(), 3);
        assert_eq!(e.find_all("foo\nfoo", false), [(Pos::new(0, 4), Pos::new(1, 3))]);
        assert_eq!(e.find_all("é", true), [(Pos::new(2, 2), Pos::new(2, 4))], "only ASCII letters fold");
        assert!(e.find_all("", true).is_empty());
    }
}
