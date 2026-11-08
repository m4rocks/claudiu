//! Generative UI in a card: Markdown that may contain fenced blocks with the info string `ui`, written as HTML with
//! inline CSS, drawn natively in Claudiu's theme instead of shown as code (`parse` reads it, `render` draws it). The
//! model is taught the dialect by `PROMPT`, which is the description of the `show_visual` tool.

mod parse;
mod render;

use rune::El;

const UI_TAGS: [&str; 4] = ["button", "chart", "details", "tabs"];

/// The description of the `show_visual` tool. Keep it in step with `parse::default_css` and the properties
/// `Css::set` understands.
pub const PROMPT: &str = r#"Draws Markdown as a card inside the terminal, under this call. Use it when a reply needs a table, comparison, flow, architecture, before/after, status board, timeline, chart or mock-up, instead of typing that into the terminal. Each call adds a new card. The user sees the drawing and not the markup, so keep your normal reply short and say the key facts there without repeating the content.

`content` is ordinary Markdown (headings, lists, tables, code, links, emphasis). Where a drawing helps, put it in a fenced block with the info string `ui`; Markdown and `ui` blocks mix freely, with prose before and after and several blocks per call. The block holds HTML with inline CSS, and Claudiu draws it in the app's own dark style. There are no scripts; the interactive tags are listed below.

````
```ui
<card>
  <div style="display:flex; justify-content:space-between; align-items:center">
    <h3>Build</h3><badge style="color:var(--ok)">passing</badge>
  </div>
  <progress value="78"></progress>
</card>
```
````

Tags: any HTML you like (div, span, p, h1-h6, ul/ol/li, table/thead/tbody/tr/th/td, pre, code, a, strong, em, hr, br, svg). Unknown tags are plain blocks. Extra shortcuts with the app's look built in: `<card>`, `<badge>`, `<callout>`, `<progress value=".." max="100">`, `<row>` (flex row), `<col>` (flex column), `<grid cols="3">`. Their style can be overridden with `style`.

Interactive tags, which go inside a `ui` block like everything else on this list:
- `<button send="Run the tests">Run tests</button>`: a click sends its `send` text as the user's next message, once, and the button shows that text. It does nothing else, so use it to offer next steps or choices.
- `<details open><summary>Title</summary>...</details>`: a section that opens and closes; leave out `open` to start closed. Use it to keep long detail out of the way.
- `<tabs><tab title="One">...</tab><tab title="Two">...</tab></tabs>`: one tab shown at a time.

Charts from data, instead of plotting SVG by hand, also inside a `ui` block: `<chart type="bar" labels="a,b,c" values="3,5,2"></chart>`, or `type="line"`. For several series, replace `values` with `<series name="p95" values="3,5,2"/>` children (`color` is optional). `width` and, for line charts, `height` are optional.

Layout is flexbox: a block stacks its children vertically; `display:flex` lays them in a row (use `flex-direction:column` for a column) and `display:grid` with `grid-template-columns` (equal-width columns) or `<grid cols>` makes a grid. Supported CSS in `style="..."`: display, flex-direction, flex-wrap, flex, gap, align-items, justify-content, align-self, width, height, min/max-width, min/max-height (px, %, rem), padding, margin, background (a colour; gradients are ignored), color, border and border-left/right/top/bottom, border-radius, opacity, overflow:hidden, font-size, font-weight, font-style, font-family:monospace, text-align, line-height, text-decoration, white-space, grid-column:span N. There are no classes, `<style>`, scripts, hover states, shadows, or external images. Inline spans can change colour, weight and background but not size; put font-size on a block.

Colours: prefer the theme tokens so everything matches the app: var(--text), var(--dim), var(--faint), var(--accent) (orange), var(--ok), var(--warn), var(--danger), var(--info), var(--bg), var(--surface), var(--raised), var(--border). Hex and rgb()/rgba() work too (`#d9775733` is accent at 20%). The background is near black, so never use black text or white backgrounds.

Drawings: an inline `<svg>` with a viewBox (and width/height) renders shapes, paths, text, and gradients. `var(--accent)` and `currentColor` work inside it. Use SVG for diagrams, flows and icons. Give it explicit sizes and legible text (at least 11px).

Be as creative as the content deserves; this is a general canvas, not a fixed set of widgets. Keep one block focused on one thing, and keep it under about 60 elements.
"#;

/// What the user has done in a session's cards: buttons sent, sections opened, tabs chosen. Kept for the life of the
/// terminal, because scrolling a card off screen must not reset it; a restart does.
#[derive(Clone, Default, Hash)]
pub struct CardUi {
    sent: std::collections::BTreeSet<String>,
    open: std::collections::BTreeMap<String, bool>,
    tabs: std::collections::BTreeMap<String, usize>,
}

impl CardUi {
    fn is_sent(&self, key: &str) -> bool {
        self.sent.contains(key)
    }

    pub fn mark_sent(&mut self, key: &str) {
        self.sent.insert(key.to_string());
    }

    fn is_open(&self, key: &str) -> Option<bool> {
        self.open.get(key).copied()
    }

    pub fn set_open(&mut self, key: &str, open: bool) {
        self.open.insert(key.to_string(), open);
    }

    fn tab(&self, key: &str) -> usize {
        self.tabs.get(key).copied().unwrap_or(0)
    }

    pub fn set_tab(&mut self, key: &str, tab: usize) {
        self.tabs.insert(key.to_string(), tab);
    }
}

/// The card a block belongs to, and the terminal its clicks go to.
#[derive(Clone)]
pub struct Scope {
    pub call: String,
    pub terminal: String,
}

fn starts_with_ui_tag(source: &str) -> bool {
    let Some(rest) = source.trim_start().strip_prefix('<') else { return false };
    let name: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
    UI_TAGS.contains(&name.to_ascii_lowercase().as_str())
}

/// The tags that mean nothing outside a `ui` fence, which the model also writes straight into the Markdown (where a
/// one-line `<chart>` is a paragraph and one with children an HTML block), fenced so they draw like a `ui` block. A
/// blank line ends such a block, so a bare tag with blank lines inside it is only drawn up to the first of them. A
/// list item that is only such a tag keeps its marker, with the fence indented under it.
pub fn fence_bare_tags(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 32);
    let mut lines = src.split_inclusive('\n').peekable();
    let mut in_fence = false;
    let mut block_start = true;
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            out.push_str(line);
            block_start = false;
            continue;
        }
        if in_fence {
            out.push_str(line);
            continue;
        }
        if line.trim().is_empty() {
            out.push_str(line);
            block_start = true;
            continue;
        }
        // A list marker, then the tag.
        let marker_len = list_marker(trimmed);
        let indent = line.len() - trimmed.len();
        let body = &trimmed[marker_len..];
        if (block_start || marker_len > 0) && starts_with_ui_tag(body) {
            let pad = " ".repeat(indent + marker_len);
            out.push_str(&line[..indent + marker_len]);
            out.push('\n');
            out.push_str(&format!("{pad}```ui\n{pad}{}", body.trim_end_matches(['\n', '\r'])));
            out.push('\n');
            while let Some(next) = lines.peek() {
                if next.trim().is_empty() {
                    break;
                }
                out.push_str(&pad);
                out.push_str(next.trim_start());
                lines.next();
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("{pad}```\n"));
            block_start = false;
            continue;
        }
        out.push_str(line);
        block_start = false;
    }
    out
}

/// The length of a Markdown list marker (`- `, `* `, `1. `) at the start of `line`, or 0.
fn list_marker(line: &str) -> usize {
    if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
        return 2;
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && line[digits..].starts_with(". ") { digits + 2 } else { 0 }
}

/// A card's Markdown: the shared look (`panels::markdown`), plus `ui` blocks drawn natively. (GitHub text does not
/// get them.) `key` names it among the window's elements; `state` is what the user did in this terminal's cards.
pub fn markdown(key: &str, text: &str, scope: &Scope, state: &CardUi) -> El {
    let source = fence_bare_tags(text);
    let (scope, state) = (scope.clone(), state.clone());
    let scale = crate::panels::scale::screen();
    crate::panels::markdown::markdown(&source)
        .code_frame(move |card, frame| match frame.language {
            Some("ui") => render::render(&parse::parse_tree(frame.source), &scope, &state, scale),
            _ => card,
        })
        .render(&crate::panels::markdown::style())
        .key(key.to_string())
}

/// What the model reads about `show_visual`.
pub fn tool_description() -> String {
    PROMPT.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Intro **bold**\n\n```ui\n<card><div style=\"display:flex;justify-content:space-between\"><h3>Build</h3><badge style=\"color:var(--ok)\">passing</badge></div><progress value=\"78\"></progress><ul><li>one<li>two with <a href=\"https://x.dev\">link</a></ul><grid cols=\"2\"><div style=\"grid-column:span 2\">wide</div><div>a</div><div>b</div></grid><table><tr><th>h<th>i<tr><td>1<td>2</table><svg viewBox=\"0 0 100 40\" width=\"200\"><rect width=\"60\" height=\"10\" fill=\"var(--accent)\"/><text x=\"2\" y=\"30\" fill=\"currentColor\">hi</text></svg><pre>code\n  here</pre></card>\n```\n\nAfter";

    /// Draws `content` as a card in a headless window, a few frames, so a panic in the renderer shows.
    fn draw(content: &str, state: CardUi) -> rune::Runtime<(String, CardUi)> {
        fn view(s: &(String, CardUi), _: &mut rune::Cx<(String, CardUi)>) -> El {
            let scope = Scope { call: "c".into(), terminal: "t".into() };
            rune::col().w(600.0).child(markdown("card", &s.0, &scope, &s.1))
        }
        let mut rt = rune::Runtime::new((content.to_string(), state), view, (640.0, 900.0), rune::Env { system_dark: true, scale: 2.0 });
        for _ in 0..3 {
            rt.frame(1.0 / 60.0);
        }
        rt
    }

    fn shows<S>(rt: &rune::Runtime<S>, text: &str) -> bool {
        rt.elements().iter().any(|e| e.text.as_deref().is_some_and(|t| t.contains(text)))
    }

    #[test]
    fn ui_block_draws() {
        let rt = draw(SAMPLE, CardUi::default());
        assert!(shows(&rt, "passing"), "the badge inside the ui block is drawn");
        assert!(shows(&rt, "After"), "Markdown after the block is drawn");
    }

    /// Every interactive element, in both of its states, through the real renderer.
    #[test]
    fn interactive_elements_draw_after_being_used() {
        const BLOCK: &str = "```ui\n<button send=\"go\">Go</button><button>inert</button>\
            <details><summary>More</summary><p>hidden</p></details>\
            <tabs><tab title=\"A\"><p>a</p></tab><tab><p>b</p></tab></tabs>\
            <tabs></tabs>\
            <chart type=\"line\" labels=\"a,b\" values=\"1,2\"></chart>\n```";
        draw(BLOCK, CardUi::default());
        let mut ui = CardUi::default();
        ui.mark_sent("anything");
        ui.set_open("c/0/1", true);
        ui.set_tab("c/0/2", 7);
        draw(BLOCK, ui);
    }

    #[test]
    fn charts_draw_when_written_straight_into_the_markdown() {
        // What the model wrote in the card that showed no charts: a one-line chart is a paragraph to Markdown, and one
        // with children is an HTML block.
        let content = "**Trucks**

<chart type=\"bar\" labels=\"S1,S2\" values=\"3,5\"></chart>

**Loans**

<chart type=\"line\" labels=\"S1,S2\" height=\"160\">
<series name=\"accepted\" values=\"2,4\"/>
<series name=\"rejected\" values=\"1,1\"/>
</chart>
";
        let fenced = fence_bare_tags(content);
        assert_eq!(fenced.matches("```ui").count(), 2, "{fenced}");
        assert!(fenced.contains("<series name=\"rejected\""), "a block with children stays whole");
        assert_eq!(fence_bare_tags("```ui\n<chart values=\"1,2\"></chart>\n```").matches("```ui").count(), 1, "and in a fence, as taught");
        assert_eq!(fence_bare_tags("- <chart values=\"1,2\"></chart>").matches("```ui").count(), 1, "and in a list");
        assert_eq!(
            fence_bare_tags("a <chart values=\"1,2\"></chart>\n\n<chartist values=\"1\"></chartist>").matches("```ui").count(),
            0,
            "only a whole block counts"
        );
        // And they draw.
        draw(content, CardUi::default());
    }

    #[test]
    fn card_state_is_remembered_by_key() {
        let mut ui = CardUi::default();
        assert_eq!((ui.is_sent("k"), ui.is_open("k"), ui.tab("k")), (false, None, 0));
        ui.mark_sent("k");
        ui.set_open("k", true);
        ui.set_tab("k", 2);
        assert_eq!((ui.is_sent("k"), ui.is_open("k"), ui.tab("k")), (true, Some(true), 2));
        assert_eq!((ui.is_sent("other"), ui.is_open("other"), ui.tab("other")), (false, None, 0));
    }
}
