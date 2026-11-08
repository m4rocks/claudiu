//! Syntax colours: tree-sitter parses a file's text and its highlight query names each piece (`keyword`, `string`,
//! ...), which becomes a colour per stretch of each line. Runs off the UI thread over the whole text; the editor shows
//! the result once it arrives (see `app::editor_mode`).

use std::collections::HashMap;
use std::sync::OnceLock;

use rune::Color;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use super::{Span, Syntax};
use crate::panels::theme;

/// The names the highlight queries use that get a colour, and the colour. A query's longer name takes the colour of
/// the longest one here that starts it (`function.method.call` is `function.method`).
const COLOURS: &[(&str, u32)] = &[
    ("attribute", 0xd19a66),
    ("boolean", 0xd19a66),
    ("comment", 0x7f848e),
    ("constant", 0xd19a66),
    ("constant.builtin", 0xd19a66),
    ("constructor", 0xe5c07b),
    ("escape", 0x56b6c2),
    ("function", 0x61afef),
    ("function.builtin", 0x56b6c2),
    ("function.macro", 0x56b6c2),
    ("function.method", 0x61afef),
    ("keyword", 0xc678dd),
    ("label", 0xe06c75),
    ("module", 0xe5c07b),
    ("number", 0xd19a66),
    ("operator", 0x56b6c2),
    ("property", 0xe06c75),
    ("punctuation", 0x8b929e),
    ("string", 0x98c379),
    ("string.escape", 0x56b6c2),
    ("string.special", 0x56b6c2),
    ("tag", 0xe06c75),
    ("text.emphasis", 0xc678dd),
    ("text.literal", 0x98c379),
    ("text.reference", 0x61afef),
    ("text.strong", 0xd19a66),
    ("text.title", 0xe06c75),
    ("text.uri", 0x56b6c2),
    ("type", 0xe5c07b),
    ("type.builtin", 0xe5c07b),
    ("variable.builtin", 0xe06c75),
    ("variable.parameter", 0xd7dae0),
];

/// tree-sitter-javascript's JSX query, which neither JavaScript's nor TypeScript's own query holds.
const JSX: &str = tree_sitter_javascript::JSX_HIGHLIGHT_QUERY;

fn config(language: tree_sitter::Language, name: &str, queries: &[&str], injections: &str, locals: &str) -> Option<HighlightConfiguration> {
    let names: Vec<&str> = COLOURS.iter().map(|(n, _)| *n).collect();
    let mut c = HighlightConfiguration::new(language, name, &queries.join("\n"), injections, locals).ok()?;
    c.configure(&names);
    Some(c)
}

/// Every language's configuration, made the first time any file is highlighted. Names are `files::language`'s.
fn configs() -> &'static HashMap<&'static str, HighlightConfiguration> {
    static CONFIGS: OnceLock<HashMap<&'static str, HighlightConfiguration>> = OnceLock::new();
    CONFIGS.get_or_init(|| {
        use tree_sitter_javascript as js;
        use tree_sitter_typescript as ts;
        let all = [
            ("rust", config(tree_sitter_rust::LANGUAGE.into(), "rust", &[tree_sitter_rust::HIGHLIGHTS_QUERY], tree_sitter_rust::INJECTIONS_QUERY, "")),
            ("javascript", config(js::LANGUAGE.into(), "javascript", &[js::HIGHLIGHT_QUERY, JSX], js::INJECTIONS_QUERY, js::LOCALS_QUERY)),
            // TypeScript's query only adds what TypeScript has beyond JavaScript, so it goes over JavaScript's.
            ("typescript", config(ts::LANGUAGE_TYPESCRIPT.into(), "typescript", &[ts::HIGHLIGHTS_QUERY, js::HIGHLIGHT_QUERY], js::INJECTIONS_QUERY, ts::LOCALS_QUERY)),
            ("tsx", config(ts::LANGUAGE_TSX.into(), "tsx", &[ts::HIGHLIGHTS_QUERY, js::HIGHLIGHT_QUERY, JSX], js::INJECTIONS_QUERY, ts::LOCALS_QUERY)),
            ("json", config(tree_sitter_json::LANGUAGE.into(), "json", &[tree_sitter_json::HIGHLIGHTS_QUERY], "", "")),
            ("toml", config(tree_sitter_toml_ng::LANGUAGE.into(), "toml", &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY], "", "")),
            ("yaml", config(tree_sitter_yaml::LANGUAGE.into(), "yaml", &[tree_sitter_yaml::HIGHLIGHTS_QUERY], "", "")),
            ("markdown", config(tree_sitter_md::LANGUAGE.into(), "markdown", &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK], tree_sitter_md::INJECTION_QUERY_BLOCK, "")),
            ("markdown_inline", config(tree_sitter_md::INLINE_LANGUAGE.into(), "markdown_inline", &[tree_sitter_md::HIGHLIGHT_QUERY_INLINE], tree_sitter_md::INJECTION_QUERY_INLINE, "")),
            ("html", config(tree_sitter_html::LANGUAGE.into(), "html", &[tree_sitter_html::HIGHLIGHTS_QUERY], tree_sitter_html::INJECTIONS_QUERY, "")),
            ("css", config(tree_sitter_css::LANGUAGE.into(), "css", &[tree_sitter_css::HIGHLIGHTS_QUERY], "", "")),
            ("bash", config(tree_sitter_bash::LANGUAGE.into(), "bash", &[tree_sitter_bash::HIGHLIGHT_QUERY], "", "")),
            ("gleam", config(tree_sitter_gleam::LANGUAGE.into(), "gleam", &[tree_sitter_gleam::HIGHLIGHT_QUERY], "", tree_sitter_gleam::LOCALS_QUERY)),
        ];
        // A grammar whose query fails to load leaves its files uncoloured rather than the app down.
        all.into_iter().filter_map(|(n, c)| Some((n, c?))).collect()
    })
}

/// The language an injection names (a code block's info string, HTML's `<script>`) as `configs` names it.
fn injected(name: &str) -> Option<&'static HighlightConfiguration> {
    let name = match name {
        "js" | "jsx" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "sh" | "shell" | "zsh" | "bash" => "bash",
        "md" => "markdown",
        "yml" => "yaml",
        n => n,
    };
    configs().get(name)
}

/// The colours of `text` in `language`, as spans per line (byte offsets within each line). `None` when the language
/// has no grammar here or the parse gave up.
pub fn highlight(language: &str, text: &str) -> Option<Vec<Vec<Span>>> {
    let config = configs().get(language)?;
    let mut highlighter = Highlighter::new();
    let events = highlighter.highlight(config, text.as_bytes(), None, |name| injected(name)).ok()?;
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let mut lines: Vec<Vec<Span>> = vec![vec![]; starts.len()];
    let mut stack: Vec<usize> = vec![];
    for event in events {
        match event.ok()? {
            HighlightEvent::HighlightStart(h) => stack.push(h.0),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let Some(&h) = stack.last() else { continue };
                let (name, colour) = COLOURS[h];
                let color = theme::rgb(colour);
                let syntax = if name.starts_with("string") { Syntax::String } else if name == "comment" { Syntax::Comment } else { Syntax::Code };
                // A piece across line breaks (a block comment) is cut at each.
                let mut line = starts.partition_point(|&s| s <= start) - 1;
                let mut from = start;
                while from < end {
                    let line_end = starts.get(line + 1).map_or(text.len(), |s| s - 1);
                    let to = end.min(line_end);
                    if to > from {
                        push(&mut lines[line], from - starts[line]..to - starts[line], color, syntax);
                    }
                    line += 1;
                    match starts.get(line) {
                        Some(&s) => from = s.max(from),
                        None => break,
                    }
                }
            }
        }
    }
    Some(lines)
}

/// Add a coloured stretch to a line, joined to the one before when it continues it in the same colour.
fn push(line: &mut Vec<Span>, bytes: std::ops::Range<usize>, color: Color, syntax: Syntax) {
    match line.last_mut() {
        Some(last) if last.bytes.end == bytes.start && last.color == color && last.syntax == syntax => last.bytes.end = bytes.end,
        _ => line.push(Span { bytes, color, syntax }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The colour of exactly `part` (its first occurrence), if some span covers it.
    fn colour_of(lang: &str, code: &str, part: &str) -> Option<Color> {
        let lines = highlight(lang, code).unwrap_or_else(|| panic!("{lang} has no grammar"));
        let (line, text) = code.lines().enumerate().find(|(_, l)| l.contains(part))?;
        let at = text.find(part)?;
        lines[line].iter().find(|s| s.bytes.start <= at && at + part.len() <= s.bytes.end).map(|s| s.color)
    }

    /// A grammar or query that fails to load leaves its files plain, so check that every language `files::language`
    /// names really colours code.
    #[test]
    fn every_named_language_colours_its_keywords() {
        let samples = [
            ("rust", "fn main() { let x = 1; }", "fn"),
            ("typescript", "const x: number = 1;", "const"),
            ("tsx", "const a = <div>{x}</div>;", "const"),
            ("javascript", "function f() { return 1; }", "function"),
            ("json", "{\"a\": 1}", "1"),
            ("toml", "[package]\nname = \"x\"", "\"x\""),
            ("yaml", "a: 1", "1"),
            ("markdown", "# Title\n\nSome *text*.", "Title"),
            ("html", "<p class=\"x\">hi</p>", "p"),
            ("css", "a { color: red; }", "color"),
            ("bash", "echo \"$HOME\"", "echo"),
            ("gleam", "pub fn main() { let x = 1 }", "fn"),
        ];
        for (lang, code, part) in samples {
            assert!(colour_of(lang, code, part).is_some(), "{lang}: {part:?} has no colour");
        }
    }

    #[test]
    fn tsx_and_jsx_colour_code_and_tags() {
        let code = "function App() { const s = \"hi\"; return <div className={s}>{s}</div>; }";
        for lang in ["tsx", "javascript"] {
            for part in ["function", "\"hi\"", "div", "className"] {
                assert!(colour_of(lang, code, part).is_some(), "{lang}: {part} has no colour");
            }
        }
    }

    #[test]
    fn a_comment_across_lines_is_coloured_on_each_line_and_nothing_else_is() {
        let code = "/* one\ntwo */ let x = 1;";
        let lines = highlight("rust", code).unwrap();
        let comment = theme::rgb(0x7f848e);
        assert_eq!(lines[0], [Span { bytes: 0..6, color: comment, syntax: Syntax::Comment }]);
        assert_eq!(lines[1][0], Span { bytes: 0..6, color: comment, syntax: Syntax::Comment });
        assert!(lines[1].iter().all(|s| s.bytes.end <= "two */ let x = 1;".len()));
        assert!(highlight("cobol", code).is_none());
    }
}
