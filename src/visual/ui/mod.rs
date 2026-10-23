//! Generative UI in the Visual panel: Markdown that may contain fenced blocks with the info string `ui`, written as
//! HTML with inline CSS, drawn natively in Claudiu's theme instead of shown as code (`parse` reads it, `render` draws
//! it). The model is taught the dialect by `PROMPT`, which is the description of the `show_visual` tool.

mod parse;
mod render;

use gpui_kit::component::text::{MarkdownNode, TextView, markdown_ast};

use parse::Tree;

const NODE: &str = "claudiu-ui";

/// The description of the `show_visual` tool. Keep it in step with `parse::default_css` and the properties
/// `Css::set` understands.
pub const PROMPT: &str = r#"Draws Markdown in Claudiu's Visual panel, beside the terminal. Use it when something is easier to see than to read: a dashboard, a comparison, a layout, a timeline, a diagram, a chart, a status board, a mock-up, or a structured walkthrough. The panel fills while you write the call, so put the most important part first. Each call adds a new card to the panel. The user sees the drawing and not the markup, so keep your normal reply short and say the key facts there without repeating the content.

`content` is ordinary Markdown (headings, lists, tables, code, links, emphasis). Where a drawing helps, put it in a fenced block with the info string `ui`; Markdown and `ui` blocks mix freely, with prose before and after and several blocks per call. The block holds HTML with inline CSS, and Claudiu draws it in the app's own dark style. The result is static (no clicking, no scripts).

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

Layout is flexbox: a block stacks its children vertically; `display:flex` lays them in a row (use `flex-direction:column` for a column) and `display:grid` with `grid-template-columns` (equal-width columns) or `<grid cols>` makes a grid. Supported CSS in `style="..."`: display, flex-direction, flex-wrap, flex, gap, align-items, justify-content, align-self, width, height, min/max-width, min/max-height (px, %, rem), padding, margin, background (a colour; gradients are ignored), color, border and border-left/right/top/bottom, border-radius, opacity, overflow:hidden, font-size, font-weight, font-style, font-family:monospace, text-align, line-height, text-decoration, white-space, grid-column:span N. There are no classes, `<style>`, scripts, hover states, shadows, or external images. Inline spans can change colour, weight and background but not size; put font-size on a block.

Colours: prefer the theme tokens so everything matches the app: var(--text), var(--dim), var(--faint), var(--accent) (orange), var(--ok), var(--warn), var(--danger), var(--info), var(--bg), var(--surface), var(--raised), var(--border). Hex and rgb()/rgba() work too (`#d9775733` is accent at 20%). The background is near black, so never use black text or white backgrounds.

Drawings: an inline `<svg>` with a viewBox (and width/height) renders shapes, paths, text, and gradients. `var(--accent)` and `currentColor` work inside it. Use SVG for charts, diagrams, flows and icons. Give it explicit sizes and legible text (at least 11px).

Be as creative as the content deserves; this is a general canvas, not a fixed set of widgets. Keep one block focused on one thing, and keep it under about 60 elements.
"#;

/// The Visual panel's markdown view: the shared one, plus `ui` fences. (GitHub text does not get them.)
pub fn markdown(id: String, text: &str) -> TextView {
    crate::github::ui::markdown(id, text)
        .markdown_block_parser(|node, _| match node {
            markdown_ast::Node::Code(c) if c.lang.as_deref() == Some("ui") => {
                Some(MarkdownNode::new(NODE, parse::parse_tree(&c.value)).text(c.value.clone()).markdown(format!("```ui\n{}\n```", c.value)))
            }
            _ => None,
        })
        .markdown_block_renderer(NODE, |node, window, cx| match node.data::<Tree>() {
            Some(tree) => render::render(tree, window, cx),
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
    use gpui::{AppContext, Context, IntoElement, Render, TestAppContext, Window, div, prelude::*};

    const SAMPLE: &str = "Intro **bold**\n\n```ui\n<card><div style=\"display:flex;justify-content:space-between\"><h3>Build</h3><badge style=\"color:var(--ok)\">passing</badge></div><progress value=\"78\"></progress><ul><li>one<li>two with <a href=\"https://x.dev\">link</a></ul><grid cols=\"2\"><div style=\"grid-column:span 2\">wide</div><div>a</div><div>b</div></grid><table><tr><th>h<th>i<tr><td>1<td>2</table><svg viewBox=\"0 0 100 40\" width=\"200\"><rect width=\"60\" height=\"10\" fill=\"var(--accent)\"/><text x=\"2\" y=\"30\" fill=\"currentColor\">hi</text></svg><pre>code\n  here</pre></card>\n```\n\nAfter";

    struct Sample(String);
    impl Render for Sample {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(gpui::px(600.0)).child(super::markdown("t".into(), &self.0))
        }
    }

    #[test]
    fn ui_block_draws() {
        let mut cx = TestAppContext::single();
        cx.update(|app| {
            gpui_kit::init(app);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
        });
        let (window, _) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |_, app| app.new(|_| Sample(SAMPLE.into())))).unwrap();
        for _ in 0..3 {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
    }
}
