//! Draws a parsed `ui` tree with native GPUI elements. A block is a column; `display:flex` or `grid` makes every child
//! its own item; otherwise runs of inline content (text, `strong`, `a`, `code`…) become one wrapped paragraph.
//! shortcut: text inside a paragraph is not selectable, and inline spans cannot change size (font-size on a block does).

use gpui::{
    AnyElement, App, Div, Font, FontStyle, FontWeight, Hsla, InteractiveText, IntoElement, Length, ParentElement, StrikethroughStyle, Styled,
    StyledText, TextRun, UnderlineStyle, Window, div, img, px, relative,
};

use super::parse::{Ax, Color, Css, Dir, Display, El, Len, Node, Svg, TextAlign, Tree, token};
use crate::theme;

fn hsla(c: Color) -> Hsla {
    let mut h = theme::hsla(c.0);
    h.a = c.1;
    h
}

fn tok(name: &str) -> Hsla {
    theme::hsla(token(name).unwrap_or(theme::TEXT))
}

fn length(l: Len) -> Length {
    match l {
        Len::Px(x) => px(x).into(),
        Len::Pct(p) => relative(p / 100.0).into(),
    }
}

/// Text properties that flow down to descendants, so a run can be built without asking GPUI what it inherited.
#[derive(Clone)]
struct Ctx {
    color: Color,
    weight: f32,
    italic: bool,
    mono: bool,
    underline: bool,
    strike: bool,
    /// Behind the text of an inline element only: a block's background belongs to its box.
    run_bg: Option<Color>,
    pre: bool,
}

impl Ctx {
    fn root() -> Self {
        Self { color: Color(theme::TEXT, 1.0), weight: 400.0, italic: false, mono: false, underline: false, strike: false, run_bg: None, pre: false }
    }

    fn with(&self, css: &Css, inline: bool) -> Self {
        Self {
            color: css.color.unwrap_or(self.color),
            weight: css.weight.unwrap_or(self.weight),
            italic: css.italic.unwrap_or(self.italic),
            mono: css.mono.unwrap_or(self.mono),
            underline: css.underline.unwrap_or(self.underline),
            strike: css.strike.unwrap_or(self.strike),
            run_bg: if inline { css.bg.or(self.run_bg) } else { None },
            pre: css.pre.unwrap_or(self.pre),
        }
    }
}

#[derive(Default)]
struct Inline {
    text: String,
    runs: Vec<TextRun>,
    links: Vec<(std::ops::Range<usize>, String)>,
}

const INLINE: [&str; 21] = [
    "span", "a", "b", "strong", "i", "em", "u", "ins", "s", "del", "strike", "mark", "small", "code", "kbd", "samp", "sub", "sup", "abbr", "label", "br",
];

fn is_inline(n: &Node) -> bool {
    match n {
        Node::Text(_) => true,
        Node::El(e) => INLINE.contains(&e.tag.as_str()) && e.css.display.is_none(),
        Node::Svg(_) => false,
    }
}

struct R {
    font: Font,
    uid: u64,
    links: usize,
}

pub fn render(tree: &Tree, window: &mut Window, _cx: &mut App) -> AnyElement {
    let mut r = R { font: window.text_style().font(), uid: tree.uid, links: 0 };
    let kids = r.children(&tree.nodes, false, &Ctx::root());
    div().flex().flex_col().gap(px(8.0)).my(px(4.0)).children(kids).into_any_element()
}

impl R {
    fn children(&mut self, nodes: &[Node], flex: bool, ctx: &Ctx) -> Vec<AnyElement> {
        let mut out = vec![];
        let mut run: Vec<&Node> = vec![];
        for n in nodes {
            if !flex && is_inline(n) {
                run.push(n);
                continue;
            }
            self.flush(&mut run, ctx, &mut out);
            match n {
                Node::Text(_) => out.extend(self.paragraph(&[n], ctx)),
                Node::El(e) => out.extend(self.element(e, ctx)),
                Node::Svg(s) => out.push(svg(s.as_ref())),
            }
        }
        self.flush(&mut run, ctx, &mut out);
        out
    }

    fn flush(&mut self, run: &mut Vec<&Node>, ctx: &Ctx, out: &mut Vec<AnyElement>) {
        out.extend(self.paragraph(run, ctx));
        run.clear();
    }

    fn element(&mut self, e: &El, ctx: &Ctx) -> Option<AnyElement> {
        let css = &e.css;
        if css.display == Some(Display::None) {
            return None;
        }
        let ctx = ctx.with(css, false);
        let flex = matches!(css.display, Some(Display::Flex | Display::Grid));
        let kids = match e.tag.as_str() {
            "progress" => {
                let num = |k: &str| e.attr(k).and_then(|v| v.trim().parse::<f32>().ok());
                let share = (num("value").unwrap_or(0.0) / num("max").filter(|m| *m > 0.0).unwrap_or(100.0)).clamp(0.0, 1.0);
                vec![div().h_full().w(relative(share)).bg(hsla(ctx.color)).into_any_element()]
            }
            "hr" => vec![],
            "ul" | "ol" => self.list(e, &ctx),
            _ => self.children(&e.children, flex, &ctx),
        };
        Some(boxed(css).children(kids).into_any_element())
    }

    fn list(&mut self, e: &El, ctx: &Ctx) -> Vec<AnyElement> {
        let (ordered, mut n, mut out) = (e.tag == "ol", 0, vec![]);
        for child in &e.children {
            match child {
                Node::El(li) if li.tag == "li" => {
                    n += 1;
                    let Some(body) = self.element(li, ctx) else { continue };
                    let marker = if ordered { format!("{n}.") } else { "•".to_string() };
                    let mark = div().flex_none().min_w(px(14.0)).text_color(tok("dim")).child(marker);
                    out.push(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(6.0))
                            .child(if ordered { mark.text_right() } else { mark })
                            .child(div().flex_1().min_w(px(0.0)).child(body))
                            .into_any_element(),
                    );
                }
                other => out.extend(self.children(std::slice::from_ref(other), false, ctx)),
            }
        }
        out
    }

    /// One wrapped block of text from a run of inline nodes; `None` when it has nothing to show.
    fn paragraph(&mut self, nodes: &[&Node], ctx: &Ctx) -> Option<AnyElement> {
        let mut out = Inline::default();
        for n in nodes {
            self.flatten(n, ctx, &mut out);
        }
        if !ctx.pre {
            while out.text.ends_with([' ', '\n']) {
                out.text.pop();
                if let Some(last) = out.runs.last_mut() {
                    last.len -= 1;
                }
            }
        }
        out.runs.retain(|r| r.len > 0);
        if out.text.is_empty() {
            return None;
        }
        let styled = StyledText::new(out.text).with_runs(out.runs);
        if out.links.is_empty() {
            return Some(div().child(styled).into_any_element());
        }
        self.links += 1;
        let (ranges, urls): (Vec<_>, Vec<_>) = out.links.into_iter().unzip();
        let id = ("ui-link", (self.uid as usize).wrapping_add(self.links));
        Some(div().child(InteractiveText::new(id, styled).on_click(ranges, move |i, _, cx| cx.open_url(&urls[i]))).into_any_element())
    }

    fn flatten(&self, n: &Node, ctx: &Ctx, out: &mut Inline) {
        match n {
            Node::Text(t) if ctx.pre => self.push(out, t, ctx),
            Node::Text(t) => {
                let mut s = String::with_capacity(t.len());
                for c in t.chars() {
                    let prev = s.chars().last().or(out.text.chars().last());
                    if c.is_whitespace() && c != '\u{a0}' {
                        if !matches!(prev, None | Some(' ' | '\n')) {
                            s.push(' ');
                        }
                    } else {
                        s.push(c);
                    }
                }
                self.push(out, &s, ctx);
            }
            Node::El(e) if e.tag == "br" => self.push(out, "\n", ctx),
            Node::El(e) if e.css.display != Some(Display::None) => {
                let (inner, start) = (ctx.with(&e.css, true), out.text.len());
                for c in &e.children {
                    self.flatten(c, &inner, out);
                }
                let href = e.attr("href").filter(|h| h.starts_with("https://") || h.starts_with("http://"));
                if let (true, Some(h)) = (e.tag == "a" && out.text.len() > start, href) {
                    out.links.push((start..out.text.len(), h.to_string()));
                }
            }
            _ => {}
        }
    }

    fn push(&self, out: &mut Inline, s: &str, ctx: &Ctx) {
        if s.is_empty() {
            return;
        }
        let mut font = self.font.clone();
        font.weight = FontWeight(ctx.weight);
        if ctx.italic {
            font.style = FontStyle::Italic;
        }
        if ctx.mono {
            font.family = crate::terminal_view::FONT.into();
        }
        let color = hsla(ctx.color);
        out.runs.push(TextRun {
            len: s.len(),
            font,
            color,
            background_color: ctx.run_bg.map(hsla),
            underline: ctx.underline.then_some(UnderlineStyle { thickness: px(1.0), color: Some(color), wavy: false }),
            strikethrough: ctx.strike.then_some(StrikethroughStyle { thickness: px(1.0), color: Some(color) }),
        });
        out.text.push_str(s);
    }
}

fn svg(s: Option<&Svg>) -> AnyElement {
    match s {
        Some(s) => div().child(img(s.image.clone()).w(px(s.w)).h(px(s.h)).max_w(relative(1.0))).into_any_element(),
        None => div()
            .flex()
            .items_center()
            .h(px(48.0))
            .px(px(12.0))
            .rounded(px(8.0))
            .bg(tok("surface"))
            .text_size(px(12.0))
            .text_color(tok("faint"))
            .child("Drawing…")
            .into_any_element(),
    }
}

/// The element's own box: layout, size, spacing, colours and text size/alignment (which its descendants inherit).
fn boxed(c: &Css) -> Div {
    let mut d = div();
    d = match c.display {
        Some(Display::Flex) if c.dir == Some(Dir::Col) => d.flex().flex_col(),
        Some(Display::Flex) => d.flex().flex_row(),
        Some(Display::Grid) => d.grid().grid_cols(c.cols.unwrap_or(1)),
        _ => d.flex().flex_col(),
    };
    if c.wrap == Some(true) {
        d = d.flex_wrap();
    }
    if let Some(g) = c.gap_x {
        d = d.gap_x(px(g));
    }
    if let Some(g) = c.gap_y {
        d = d.gap_y(px(g));
    }
    d = match c.align {
        Some(Ax::Start) => d.items_start(),
        Some(Ax::Center) => d.items_center(),
        Some(Ax::End) => d.items_end(),
        _ => d,
    };
    d = match c.justify {
        Some(Ax::Start) => d.justify_start(),
        Some(Ax::Center) => d.justify_center(),
        Some(Ax::End) => d.justify_end(),
        Some(Ax::Between) => d.justify_between(),
        Some(Ax::Around) => d.justify_around(),
        _ => d,
    };
    d = match c.align_self {
        Some(Ax::Start) => d.self_start(),
        Some(Ax::Center) => d.self_center(),
        Some(Ax::End) => d.self_end(),
        _ => d,
    };
    if c.grow.is_some() || c.shrink.is_some() {
        let grow = c.grow.unwrap_or(0.0);
        d.style().flex_grow = Some(grow);
        d.style().flex_shrink = Some(c.shrink.unwrap_or(1.0));
        if grow > 0.0 {
            d = d.flex_basis(relative(0.0));
        }
    }
    for (set, w) in [(0, c.w), (1, c.h), (2, c.min_w), (3, c.max_w), (4, c.min_h), (5, c.max_h)] {
        if let Some(l) = w.map(length) {
            d = match set {
                0 => d.w(l),
                1 => d.h(l),
                2 => d.min_w(l),
                3 => d.max_w(l),
                4 => d.min_h(l),
                _ => d.max_h(l),
            };
        }
    }
    if c.min_w.is_none() {
        d = d.min_w(px(0.0));
    }
    let [pt, pr, pb, pl] = c.pad.map(|x| x.map(px));
    let [mt, mr, mb, ml] = c.margin.map(|x| x.map(px));
    let [bt, br, bb, bl] = c.border.map(|x| x.map(px));
    macro_rules! side {
        ($d:ident, $($m:ident = $v:expr),*) => { $(if let Some(v) = $v { $d = $d.$m(v); })* };
    }
    side!(d, pt = pt, pr = pr, pb = pb, pl = pl, mt = mt, mr = mr, mb = mb, ml = ml, border_t = bt, border_r = br, border_b = bb, border_l = bl);
    if c.border.iter().any(|b| b.is_some_and(|b| b > 0.0)) {
        d = d.border_color(c.border_color.map_or_else(|| tok("border"), hsla));
    }
    if let Some(bg) = c.bg {
        d = d.bg(hsla(bg));
    }
    if let Some(r) = c.radius {
        d = d.rounded(px(r));
    }
    if let Some(o) = c.opacity {
        d = d.opacity(o.clamp(0.0, 1.0));
    }
    if c.clip == Some(true) {
        d = d.overflow_hidden();
    }
    if let Some(s) = c.size {
        d = d.text_size(px(s));
    }
    if let Some(l) = c.line_height {
        d = d.line_height(relative(l));
    }
    d = match c.text_align {
        Some(TextAlign::Center) => d.text_center(),
        Some(TextAlign::Right) => d.text_right(),
        _ => d,
    };
    if c.nowrap == Some(true) {
        d = d.whitespace_nowrap();
    }
    if let Some(n) = c.col_span {
        d = d.col_span(n);
    }
    d
}
