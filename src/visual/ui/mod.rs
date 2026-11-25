//! Generative UI in a card: Markdown that may contain fenced blocks with the info string `ui`, written as HTML with
//! inline CSS, drawn natively in m4code's theme instead of shown as code (`parse` reads it, `render` draws it). The
//! model receives this dialect through m4code's MCP instructions.

mod parse;
mod render;

use std::collections::{HashMap, HashSet};

use gpui::{Context, Entity, EventEmitter};
use gpui_kit::component::text::{MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextView, markdown_ast};

use parse::Tree;

const NODE: &str = "m4code-ui";

/// What the user has done in a session's cards: buttons sent, sections opened, tabs chosen, checkboxes toggled.
/// Kept for the life of the terminal, because scrolling a card off screen must not reset it; a restart does.
#[derive(Clone, Default)]
pub struct CardUi {
    sent: HashSet<String>,
    open: HashMap<String, bool>,
    tabs: HashMap<String, usize>,
    checks: HashMap<String, (String, bool)>,
}

/// A button was clicked: `text` should go out as the user's next message.
pub struct SendText {
    pub key: String,
    pub text: String,
}

/// A button with `open="path#L10-L20"` was clicked: open that workspace file/range in the editor.
pub struct OpenFile {
    pub path: String,
    pub line: Option<u32>,
    pub end_line: Option<u32>,
    pub row: Option<usize>,
}

impl EventEmitter<SendText> for CardUi {}
impl EventEmitter<OpenFile> for CardUi {}

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

    fn is_checked(&self, key: &str, default: bool) -> bool {
        self.checks.get(key).map_or(default, |(_, v)| *v)
    }

    fn set_checked(&mut self, key: &str, label: String, checked: bool, cx: &mut Context<Self>) {
        self.checks.insert(key.to_string(), (label, checked));
        cx.notify();
    }

    fn checked_labels(&self, block_prefix: &str, defaults: &[(String, String, bool)]) -> Vec<String> {
        let mut out = Vec::new();
        for (key, label, default) in defaults {
            if key.starts_with(block_prefix) && self.is_checked(key, *default) {
                out.push(label.clone());
            }
        }
        out
    }
}

/// The card a block belongs to, and where its clicks go.
#[derive(Clone)]
pub struct Scope {
    pub call: String,
    pub row: Option<usize>,
    pub ui: Entity<CardUi>,
}

/// The chat keeps one Markdown document so references and layout span interactive blocks.
pub(crate) fn transcript(view: TextView, scope: Scope) -> TextView {
    let extensions = MarkdownExtensions::default().parser_revision(1)
        .block_parser(transcript_block)
        .block_renderer(NODE, move |node, window, cx| {
            let Some((offset, tree)) = node.data::<(usize, Tree)>() else { return gpui::IntoElement::into_any_element(gpui::div()); };
            let block = Scope { call: format!("{}-{offset}", scope.call), row: scope.row, ui: scope.ui.clone() };
            render::render(tree, &block, window, cx)
        });
    view.markdown_extensions(extensions)
}

fn transcript_block(node: &markdown_ast::Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
    let markdown_ast::Node::Code(code) = node else { return None };
    if !code.lang.as_deref().is_some_and(|lang| lang.eq_ignore_ascii_case("ui"))
        || code.value.len() > 128 * 1024
    {
        return None;
    }
    let source = cx.node_source(node)?;
    if !closed_fence(source) {
        return None;
    }
    let start = code.position.as_ref()?.start.offset;
    let prefix = cx.source().get(..start)?.rsplit('\n').next()?;
    if prefix.len() > 3 || prefix.bytes().any(|ch| ch != b' ') {
        return None;
    }
    Some(MarkdownNode::new(NODE, (cx.offset() + start, parse::parse_tree(&code.value)))
        .text(code.value.clone()).markdown(source.to_string()))
}

fn closed_fence(source: &str) -> bool {
    let Some((opening, body)) = source.split_once('\n') else { return false };
    let Some(marker @ ('`' | '~')) = opening.chars().next() else { return false };
    let count = opening.chars().take_while(|c| *c == marker).count();
    let last_line = body.lines().last().unwrap_or_default().trim_end();
    let closing = last_line.trim_start_matches(' ');
    count >= 3 && last_line.len() - closing.len() <= 3 && closing.len() >= count
        && closing.chars().all(|c| c == marker)
}

#[cfg(test)]
mod tests {
    //! Draws a reply with a `ui` block headless, to catch panics in the renderer (gpui's test platform; no window).
    use std::sync::{Arc, Mutex};

    use gpui::{AppContext, Context, Entity, IntoElement, Render, TestAppContext, Window, div, prelude::*};

    const SAMPLE: &str = "Intro **bold**\n\n```ui\n<card><div style=\"display:flex;justify-content:space-between\"><h3>Build</h3><badge style=\"color:var(--ok)\">passing</badge></div><progress value=\"78\"></progress><ul><li>one<li>two with <a href=\"https://x.dev\">link</a></ul><grid cols=\"2\"><div style=\"grid-column:span 2\">wide</div><div>a</div><div>b</div></grid><table><tr><th>h<th>i<tr><td>1<td>2</table><svg viewBox=\"0 0 100 40\" width=\"200\"><rect width=\"60\" height=\"10\" fill=\"var(--accent)\"/><text x=\"2\" y=\"30\" fill=\"currentColor\">hi</text></svg><pre>code\n  here</pre></card>\n```\n\nAfter";

    #[test]
    fn streamed_fences_only_render_after_the_closing_marker() {
        assert!(!super::closed_fence("```ui\n<button>Running</button>"));
        assert!(!super::closed_fence("````ui\n<p>Example</p>\n```"));
        assert!(super::closed_fence("~~~ui\n<p>Done</p>\n~~~~"));
    }

    struct Sample(String, Entity<super::CardUi>);
    impl Render for Sample {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let scope = super::Scope { call: "c".into(), row: None, ui: self.1.clone() };
            div().w(gpui::px(600.0)).child(super::transcript(crate::github::ui::markdown("t".into(), &self.0), scope))
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
            ui.set_open("c-0/0/1", true, cx);
            ui.set_tab("c-0/0/2", 7, cx);
        });
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
    }

    /// How many charts, each with a drawing, a card's Markdown reaches the `ui` renderer with.
    fn charts_drawn(content: &'static str) -> usize {
        struct Recorder(&'static str, Arc<Mutex<usize>>);
        impl Render for Recorder {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let count = self.1.clone();
                let view = crate::github::ui::markdown("t".into(), self.0).markdown_block_parser(super::transcript_block).markdown_block_renderer(super::NODE, move |node, _, _| {
                    let (_, tree) = node.data::<(usize, super::Tree)>().expect("a UI block carries its tree");
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
    fn only_closed_top_level_ui_fences_render_interactive_content() {
        let closed = "```ui\n<chart values=\"1,2\"></chart>\n```";
        let unclosed = "```ui\n<chart values=\"1,2\"></chart>";
        let indented = "    ```ui\n    <chart values=\"1,2\"></chart>\n    ```";
        assert_eq!(charts_drawn(closed), 1);
        assert_eq!(charts_drawn(unclosed), 0);
        assert_eq!(charts_drawn(indented), 0);
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
