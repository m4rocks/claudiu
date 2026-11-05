//! Generative UI in a card: Markdown that may contain fenced blocks with the info string `ui`, written as HTML with
//! inline CSS, drawn natively in Claudiu's theme instead of shown as code (`parse` reads it, `render` draws it). The
//! model is taught the dialect by `PROMPT`, which is the description of the `show_visual` tool.

mod parse;
mod render;

use std::collections::{HashMap, HashSet};

use gpui::{Context, Entity, EventEmitter};
use gpui_kit::component::text::{MarkdownNode, MarkdownParseContext, TextView, markdown_ast};

use parse::Tree;

const NODE: &str = "claudiu-ui";

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
#[derive(Clone, Default)]
pub struct CardUi {
    sent: HashSet<String>,
    open: HashMap<String, bool>,
    tabs: HashMap<String, usize>,
}

/// A button was clicked: `text` should go out as the user's next message.
pub struct SendText {
    pub key: String,
    pub text: String,
}

impl EventEmitter<SendText> for CardUi {}

impl CardUi {
    fn is_sent(&self, key: &str) -> bool {
        self.sent.contains(key)
    }

    pub fn mark_sent(&mut self, key: &str, cx: &mut Context<Self>) {
        self.sent.insert(key.to_string());
        cx.notify();
    }

    fn is_open(&self, key: &str) -> Option<bool> {
        self.open.get(key).copied()
    }

    fn set_open(&mut self, key: &str, open: bool, cx: &mut Context<Self>) {
        self.open.insert(key.to_string(), open);
        cx.notify();
    }

    fn tab(&self, key: &str) -> usize {
        self.tabs.get(key).copied().unwrap_or(0)
    }

    fn set_tab(&mut self, key: &str, tab: usize, cx: &mut Context<Self>) {
        self.tabs.insert(key.to_string(), tab);
        cx.notify();
    }
}

/// The card a block belongs to, and where its clicks go.
#[derive(Clone)]
pub struct Scope {
    pub call: String,
    pub ui: Entity<CardUi>,
}

/// `ui` fences, and the tags that mean nothing outside one. The model writes those straight into the Markdown too,
/// where a one-line `<chart>` is a paragraph and one with children an HTML block. A blank line ends either, so a bare
/// tag with blank lines inside it is only drawn up to the first of them.
fn ui_block(node: &markdown_ast::Node, cx: &MarkdownParseContext) -> Option<MarkdownNode> {
    match node {
        markdown_ast::Node::Code(c) if c.lang.as_deref() == Some("ui") => Some(ui_node(&c.value)),
        markdown_ast::Node::Html(_) | markdown_ast::Node::Paragraph(_) => cx.node_source(node).filter(|s| starts_with_ui_tag(s)).map(ui_node),
        _ => None,
    }
}

fn starts_with_ui_tag(source: &str) -> bool {
    let Some(rest) = source.strip_prefix('<') else { return false };
    let name: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
    UI_TAGS.contains(&name.to_ascii_lowercase().as_str())
}

fn ui_node(source: &str) -> MarkdownNode {
    MarkdownNode::new(NODE, parse::parse_tree(source)).text(source.to_string()).markdown(format!("```ui
{source}
```"))
}

/// A card's markdown view: the shared one, plus `ui` blocks. (GitHub text does not get them.)
pub fn markdown(id: String, text: &str, scope: Scope) -> TextView {
    crate::github::ui::markdown(id, text)
        .markdown_block_parser(ui_block)
        .markdown_block_renderer(NODE, move |node, window, cx| match node.data::<Tree>() {
            Some(tree) => render::render(tree, &scope, window, cx),
            None => gpui::IntoElement::into_any_element(gpui::div()),
        })
}

/// What the model reads about `show_visual`.
pub fn tool_description() -> String {
    PROMPT.trim().to_string()
}

#[cfg(test)]
mod tests {
    //! Draws a reply with a `ui` block headless, to catch panics in the renderer (gpui's test platform; no window).
    use std::sync::{Arc, Mutex};

    use gpui::{AppContext, Context, Entity, IntoElement, Render, TestAppContext, Window, div, prelude::*};

    const SAMPLE: &str = "Intro **bold**\n\n```ui\n<card><div style=\"display:flex;justify-content:space-between\"><h3>Build</h3><badge style=\"color:var(--ok)\">passing</badge></div><progress value=\"78\"></progress><ul><li>one<li>two with <a href=\"https://x.dev\">link</a></ul><grid cols=\"2\"><div style=\"grid-column:span 2\">wide</div><div>a</div><div>b</div></grid><table><tr><th>h<th>i<tr><td>1<td>2</table><svg viewBox=\"0 0 100 40\" width=\"200\"><rect width=\"60\" height=\"10\" fill=\"var(--accent)\"/><text x=\"2\" y=\"30\" fill=\"currentColor\">hi</text></svg><pre>code\n  here</pre></card>\n```\n\nAfter";

    struct Sample(String, Entity<super::CardUi>);
    impl Render for Sample {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let scope = super::Scope { call: "c".into(), ui: self.1.clone() };
            div().w(gpui::px(600.0)).child(super::markdown("t".into(), &self.0, scope))
        }
    }

    #[test]
    fn ui_block_draws() {
        let mut cx = TestAppContext::single();
        cx.update(|app| {
            gpui_kit::init(app);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
        });
        let (window, _) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |_, app| app.new(|cx| Sample(SAMPLE.into(), cx.new(|_| super::CardUi::default()))))).unwrap();
        for _ in 0..3 {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
    }

    /// Every interactive element, in both of its states, through the real renderer.
    #[test]
    fn interactive_elements_draw_after_being_used() {
        const BLOCK: &str = "```ui\n<button send=\"go\">Go</button><button>inert</button>\
            <details><summary>More</summary><p>hidden</p></details>\
            <tabs><tab title=\"A\"><p>a</p></tab><tab><p>b</p></tab></tabs>\
            <tabs></tabs>\
            <chart type=\"line\" labels=\"a,b\" values=\"1,2\"></chart>\n```";
        let mut cx = TestAppContext::single();
        cx.update(|app| {
            gpui_kit::init(app);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
        });
        let ui = cx.update(|app| app.new(|_| super::CardUi::default()));
        let sample = ui.clone();
        let (window, _) = cx.update(|app| gpui_kit::open_window(Default::default(), app, move |_, app| app.new(|_| Sample(BLOCK.into(), sample)))).unwrap();
        let mut draw = || {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        };
        draw();
        ui.update(&mut cx, |ui, cx| {
            ui.mark_sent("anything", cx);
            ui.set_open("c/0/1", true, cx);
            ui.set_tab("c/0/2", 7, cx);
        });
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
    }

    /// How many charts, each with a drawing, a card's Markdown reaches the `ui` renderer with.
    fn charts_drawn(content: &'static str) -> usize {
        struct Recorder(&'static str, Arc<Mutex<usize>>);
        impl Render for Recorder {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let count = self.1.clone();
                let view = crate::github::ui::markdown("t".into(), self.0).markdown_block_parser(super::ui_block).markdown_block_renderer(super::NODE, move |node, _, _| {
                    let tree = node.data::<super::Tree>().expect("a ui block carries its tree");
                    *count.lock().unwrap() += tree.nodes.iter().filter(|n| matches!(n, super::parse::Node::Svg(Some(_)))).count();
                    div().into_any_element()
                });
                div().w(gpui::px(600.0)).child(view)
            }
        }

        let mut cx = TestAppContext::single();
        cx.update(|app| {
            gpui_kit::init(app);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
        });
        let count = Arc::new(Mutex::new(0));
        let recorded = count.clone();
        let (window, _) = cx.update(|app| gpui_kit::open_window(Default::default(), app, move |_, app| app.new(|_| Recorder(content, recorded)))).unwrap();
        for _ in 0..3 {
            *count.lock().unwrap() = 0;
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
        *count.lock().unwrap()
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
        assert_eq!(charts_drawn(content), 2);
        assert_eq!(charts_drawn("```ui
<chart values=\"1,2\"></chart>
```"), 1, "and in a fence, as taught");
        assert_eq!(charts_drawn("- <chart values=\"1,2\"></chart>"), 1, "and in a list");
        assert_eq!(charts_drawn("a <chart values=\"1,2\"></chart>

<chartist values=\"1\"></chartist>"), 0, "only a whole block counts");
    }

    #[test]
    fn card_state_is_remembered_by_key() {
        let mut ui = super::CardUi::default();
        assert_eq!((ui.is_sent("k"), ui.is_open("k"), ui.tab("k")), (false, None, 0));
        ui.sent.insert("k".into());
        ui.open.insert("k".into(), true);
        ui.tabs.insert("k".into(), 2);
        assert_eq!((ui.is_sent("k"), ui.is_open("k"), ui.tab("k")), (true, Some(true), 2));
        assert_eq!((ui.is_sent("other"), ui.is_open("other"), ui.tab("other")), (false, None, 0));
    }
}
