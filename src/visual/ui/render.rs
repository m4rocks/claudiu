//! Draws a parsed `ui` tree with Rune elements. A block is a column; `display:flex` or `grid` makes every child its
//! own item; otherwise runs of inline content (text, `strong`, `a`, `code`…) become one wrapped paragraph.
//! Rune text doesn't inherit from its box, so the text properties a box sets (size, line height, alignment, wrapping)
//! travel down in `Ctx` with the CSS ones. Inline spans cannot change size (font-size on a block does).

use rune::{Cursor, El, Link, Span, TextStyle, col, el, rich, row, text};

use super::parse::{Ax, Color, Css, Dir, Display, El as Tag, Len, Node, Svg, TextAlign, Tree, token};
use super::{CardUi, Scope};
use crate::app::Workspace;
use crate::panels::theme;

/// The longest message the mod sends for a button (`PROMPT_MAX` in `mod/hooks/register.js`).
const SEND_MAX: usize = 20_000;

fn color(c: Color) -> rune::Color {
    theme::rgb(c.0).alpha(c.1)
}

fn tok(name: &str) -> rune::Color {
    theme::rgb(token(name).unwrap_or(theme::TEXT))
}

/// Text properties that flow down to descendants.
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
    size: f32,
    line_height: Option<f32>,
    align: Option<TextAlign>,
    nowrap: bool,
}

impl Ctx {
    fn root() -> Self {
        Self {
            color: Color(theme::TEXT, 1.0),
            weight: 400.0,
            italic: false,
            mono: false,
            underline: false,
            strike: false,
            run_bg: None,
            pre: false,
            size: 13.0,
            line_height: None,
            align: None,
            nowrap: false,
        }
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
            size: if inline { self.size } else { css.size.unwrap_or(self.size) },
            line_height: if inline { self.line_height } else { css.line_height.or(self.line_height) },
            align: if inline { self.align } else { css.text_align.or(self.align) },
            nowrap: if inline { self.nowrap } else { css.nowrap.unwrap_or(self.nowrap) },
        }
    }

    fn style(&self) -> TextStyle {
        let size = crate::panels::scale::px(self.size);
        let style = TextStyle::new(size, 400.0);
        match self.line_height {
            Some(l) => style.lh((size * l).round()),
            None => style,
        }
    }
}

#[derive(Default)]
struct Inline {
    text: String,
    spans: Vec<Span>,
    strikes: Vec<(usize, usize)>,
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

struct R<'a> {
    uid: u64,
    texts: usize,
    screen_scale: f32,
    scope: &'a Scope,
    /// What the user has clicked so far, as of this frame.
    state: &'a CardUi,
}

pub fn render(tree: &Tree, scope: &Scope, state: &CardUi, screen_scale: f32) -> El {
    let mut r = R { uid: tree.uid, texts: 0, screen_scale, scope, state };
    let kids = r.children(&tree.nodes, false, &Ctx::root());
    col().w_full().gap(8.0).my(4.0).children(kids)
}

impl R<'_> {
    fn children(&mut self, nodes: &[Node], flex: bool, ctx: &Ctx) -> Vec<El> {
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
                Node::Svg(s) => out.push(svg(s.as_ref(), self.screen_scale)),
            }
        }
        self.flush(&mut run, ctx, &mut out);
        out
    }

    fn flush(&mut self, run: &mut Vec<&Node>, ctx: &Ctx, out: &mut Vec<El>) {
        out.extend(self.paragraph(run, ctx));
        run.clear();
    }

    fn element(&mut self, e: &Tag, ctx: &Ctx) -> Option<El> {
        let css = &e.css;
        if css.display == Some(Display::None) {
            return None;
        }
        let ctx = ctx.with(css, false);
        let flex = matches!(css.display, Some(Display::Flex | Display::Grid));
        let kids = match e.tag.as_str() {
            "button" => return Some(self.button(e, &ctx)),
            "details" => return Some(self.details(e, &ctx)),
            "tabs" => return Some(self.tabs(e, &ctx)),
            "progress" => {
                let num = |k: &str| e.attr(k).and_then(|v| v.trim().parse::<f32>().ok());
                let share = (num("value").unwrap_or(0.0) / num("max").filter(|m| *m > 0.0).unwrap_or(100.0)).clamp(0.0, 1.0);
                vec![el().h_full().w_frac(share).fill(color(ctx.color)).motion(rune::tok::motion::REFLOW)]
            }
            "hr" => vec![],
            "ul" | "ol" => self.list(e, &ctx),
            _ if css.display == Some(Display::Grid) => return Some(self.grid(e, &ctx)),
            _ => self.children(&e.children, flex, &ctx),
        };
        Some(boxed(css).children(kids))
    }

    /// A grid: rows of `cols` equal columns, where `grid-column: span N` takes N of them.
    fn grid(&mut self, e: &Tag, ctx: &Ctx) -> El {
        let cols = e.css.cols.unwrap_or(1).max(1);
        let gap_x = e.css.gap_x.unwrap_or(0.0);
        let mut rows: Vec<El> = vec![];
        let mut line: Vec<El> = vec![];
        let mut used = 0;
        for n in &e.children {
            let span = match n {
                Node::El(c) => c.css.col_span.unwrap_or(1).clamp(1, cols),
                _ => 1,
            };
            let Some(cell) = self.children(std::slice::from_ref(n), true, ctx).into_iter().next() else { continue };
            if used + span > cols && !line.is_empty() {
                rows.push(grid_row(std::mem::take(&mut line), used.into(), cols.into(), gap_x));
                used = 0;
            }
            line.push(el().grow(span as f32).shrink().min_w(0.0).child(cell));
            used += span;
        }
        if !line.is_empty() {
            rows.push(grid_row(line, used.into(), cols.into(), gap_x));
        }
        let mut b = boxed(&e.css);
        // The rows stack; the box's own row gap separates them.
        b = b.gaps(e.css.gap_y.unwrap_or(0.0), 0.0);
        b.children(rows)
    }

    /// Identifies an interactive element across frames: card, block, and its place in the block.
    fn key(&self, e: &Tag) -> String {
        format!("{}/{}/{}", self.scope.call, self.uid, e.seq)
    }

    /// Sends its `send` text as the user's next message, once. That text is always on show, so a label can't hide
    /// what a click does. A button with nothing valid to send is drawn inert.
    fn button(&mut self, e: &Tag, ctx: &Ctx) -> El {
        let kids = self.children(&e.children, false, ctx);
        let send = e.attr("send").map(str::trim).filter(|s| !s.is_empty() && s.chars().count() <= SEND_MAX).map(str::to_string);
        let Some(message) = send else { return boxed(&e.css).opacity(0.5).children(kids) };
        let key = self.key(e);
        let sent = self.state.is_sent(&key);
        let hint = if sent { format!("Sent: {message}") } else { message.clone() };
        let hint = text(hint).font(TextStyle::new(crate::panels::scale::px(11.5), 400.0)).color(tok("dim")).wrap();
        let body = boxed(&e.css).key(format!("{}/{key}", self.scope.terminal)).children(kids).child(hint);
        if sent {
            return body.opacity(0.6);
        }
        let terminal = self.scope.terminal.clone();
        body.cursor(Cursor::Pointer)
            .hover(rune::Color::WHITE.alpha(0.04))
            .press_scale(0.98)
            .on_press_tx(move |ws: &mut Workspace, tx| {
                if let Some(t) = ws.terminal_mut(&terminal) {
                    t.card_send(&key, message.clone());
                }
                ws.after_input(tx);
            })
    }

    /// `<details>`: a `<summary>` line that opens and closes the rest.
    fn details(&mut self, e: &Tag, ctx: &Ctx) -> El {
        let key = self.key(e);
        let open = self.state.is_open(&key).unwrap_or(e.attr("open").is_some());
        let is_summary = |n: &&Node| matches!(n, Node::El(s) if s.tag == "summary");
        let summary = e.children.iter().find(is_summary).map_or(vec![], |n| match n {
            Node::El(s) => s.children.clone(),
            _ => vec![],
        });
        let title = self.children(&summary, false, ctx);
        let terminal = self.scope.terminal.clone();
        let toggle_key = key.clone();
        let chevron = rune::icon(&crate::panels::icons::CHEVRON_RIGHT, 12.0)
            .color(tok("dim"))
            .rotate(if open { std::f32::consts::FRAC_PI_2 } else { 0.0 })
            .rotate_motion(rune::tok::motion::SNAP);
        let head = row()
            .key(format!("{}/{key}", self.scope.terminal))
            .center_y()
            .gap(6.0)
            .cursor(Cursor::Pointer)
            .child(chevron)
            .child(col().grow(1.0).shrink().min_w(0.0).children(title))
            .on_press_tx(move |ws: &mut Workspace, tx| {
                if let Some(t) = ws.terminal_mut(&terminal) {
                    t.card_ui().set_open(&toggle_key, !open);
                }
                ws.after_input(tx);
            });
        let mut body = vec![];
        if open {
            let rest: Vec<Node> = e.children.iter().filter(|n| !is_summary(n)).cloned().collect();
            body = self.children(&rest, false, ctx).into_iter().map(|b| b.transition(rune::Transition::opacity())).collect();
        }
        boxed(&e.css).child(head).children(body)
    }

    /// `<tabs>`: one `<tab title="..">` shown at a time.
    fn tabs(&mut self, e: &Tag, ctx: &Ctx) -> El {
        let tabs: Vec<&Tag> = e
            .children
            .iter()
            .filter_map(|n| match n {
                Node::El(t) if t.tag == "tab" => Some(t.as_ref()),
                _ => None,
            })
            .collect();
        let key = self.key(e);
        let chosen = self.state.tab(&key).min(tabs.len().saturating_sub(1));
        let mut head = row().flex_wrap().gap(4.0);
        for (n, tab) in tabs.iter().enumerate() {
            let title = tab.attr("title").map_or_else(|| format!("Tab {}", n + 1), str::to_string);
            let (terminal, tab_key) = (self.scope.terminal.clone(), key.clone());
            let mut t = row()
                .key(format!("{}/{key}/{n}", self.scope.terminal))
                .px(10.0)
                .py(3.0)
                .radius(6.0)
                .cursor(Cursor::Pointer)
                .hover(rune::Color::WHITE.alpha(0.04))
                .motion(rune::tok::motion::SNAP)
                .child(text(title).font(ctx.style()).color(tok(if n == chosen { "text" } else { "dim" })))
                .on_press_tx(move |ws: &mut Workspace, tx| {
                    if let Some(t) = ws.terminal_mut(&terminal) {
                        t.card_ui().set_tab(&tab_key, n);
                    }
                    ws.after_input(tx);
                });
            if n == chosen {
                t = t.fill(tok("raised"));
            }
            head = head.child(t);
        }
        let body = tabs.get(chosen).map_or(vec![], |tab| self.children(&tab.children, false, ctx));
        boxed(&e.css).child(head).children(body)
    }

    fn list(&mut self, e: &Tag, ctx: &Ctx) -> Vec<El> {
        let (ordered, mut n, mut out) = (e.tag == "ol", 0, vec![]);
        for child in &e.children {
            match child {
                Node::El(li) if li.tag == "li" => {
                    n += 1;
                    let Some(body) = self.element(li, ctx) else { continue };
                    let marker = if ordered { format!("{n}.") } else { "•".to_string() };
                    let mark = text(marker).font(ctx.style()).color(tok("dim"));
                    let mark = row().min_w(14.0).child(mark);
                    out.push(
                        row()
                            .gap(6.0)
                            .child(if ordered { mark.justify_end() } else { mark })
                            .child(col().grow(1.0).shrink().min_w(0.0).child(body)),
                    );
                }
                other => out.extend(self.children(std::slice::from_ref(other), false, ctx)),
            }
        }
        out
    }

    /// One wrapped block of text from a run of inline nodes; `None` when it has nothing to show.
    fn paragraph(&mut self, nodes: &[&Node], ctx: &Ctx) -> Option<El> {
        let mut out = Inline::default();
        for n in nodes {
            self.flatten(n, ctx, &mut out);
        }
        if !ctx.pre {
            let trimmed = out.text.trim_end_matches([' ', '\n']).len();
            out.text.truncate(trimmed);
        }
        if out.text.is_empty() {
            return None;
        }
        let end = out.text.len() as u32;
        let spans: Vec<Span> = out
            .spans
            .into_iter()
            .filter_map(|mut s| {
                s.end = s.end.min(end);
                (s.start < s.end).then_some(s)
            })
            .collect();
        let strikes: Vec<_> = out.strikes.into_iter().filter(|(a, _)| *a < end as usize).map(|(a, b)| (a, b.min(end as usize), None)).collect();
        let mut t = rich(out.text, spans).font(ctx.style()).color(color(ctx.color)).strikes(strikes);
        t = if ctx.nowrap { t } else { t.wrap() };
        t = match ctx.align {
            Some(TextAlign::Center) => t.align(rune::Align::Center),
            Some(TextAlign::Right) => t.align(rune::Align::End),
            _ => t,
        };
        self.texts += 1;
        t = t.key(format!("{}/ui-text/{}/{}/{}", self.scope.terminal, self.scope.call, self.uid, self.texts));
        if out.links.is_empty() {
            return Some(t.selectable(true).selection_color(theme::rgb(theme::SELECTION)));
        }
        for (range, url) in out.links {
            t = t.link(Link::new(range, url));
        }
        Some(t.on_link(|ws: &mut Workspace, url: &str, tx: &mut rune::Tx| {
            let _ = tx.open_url(url);
            ws.after_input(tx);
        }))
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
        let (start, end) = (out.text.len(), out.text.len() + s.len());
        let c = color(ctx.color);
        let mut span = Span::new(start, end).weight(ctx.weight).color(c);
        if ctx.italic {
            span = span.italic();
        }
        if ctx.mono {
            span = span.mono();
        }
        if let Some(bg) = ctx.run_bg {
            span = span.bg(color(bg), 2.0);
        }
        if ctx.underline {
            span = span.underline(c);
        }
        if ctx.strike {
            out.strikes.push((start, end));
        }
        out.spans.push(span);
        out.text.push_str(s);
    }
}

fn grid_row(cells: Vec<El>, used: usize, cols: usize, gap: f32) -> El {
    let mut r = row().w_full().gap(gap).items_stretch();
    r = r.children(cells);
    // A short last row keeps its cells at the width of a full one.
    if used < cols {
        r = r.child(el().grow((cols - used) as f32));
    }
    r
}

fn svg(s: Option<&Svg>, screen_scale: f32) -> El {
    match s {
        Some(s) => {
            let image = s.image(screen_scale);
            el().w(s.w).h(s.h).pixels(image.pixels()).transition(rune::Transition::opacity())
        }
        None => row()
            .center_y()
            .h(48.0)
            .px(12.0)
            .radius(8.0)
            .fill(tok("surface"))
            .child(text("Drawing…").font(TextStyle::new(crate::panels::scale::px(12.0), 400.0)).color(tok("faint")))
            .looping(rune::LoopSpec::opacity(1.6, 0.0, 0.5, 1.0)),
    }
}

/// The element's own box: layout, size, spacing and colours.
fn boxed(c: &Css) -> El {
    let mut d = match c.display {
        Some(Display::Flex) if c.dir != Some(Dir::Col) => row(),
        _ => col(),
    };
    if c.wrap == Some(true) {
        d = d.flex_wrap();
    }
    match (c.gap_x, c.gap_y) {
        (None, None) => {}
        (x, y) => d = d.gaps(y.unwrap_or(0.0), x.unwrap_or(0.0)),
    }
    d = match c.align {
        Some(Ax::Start) => d.items_start(),
        Some(Ax::Center) => d.items_center(),
        Some(Ax::End) => d.items_end(),
        Some(Ax::Stretch) => d.items_stretch(),
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
        if grow > 0.0 {
            d = d.grow(grow);
        }
        if c.shrink.unwrap_or(1.0) > 0.0 {
            d = d.shrink();
        }
    }
    match c.w {
        Some(Len::Px(x)) => d = d.w(x),
        Some(Len::Pct(p)) => d = d.w_frac(p / 100.0),
        None => {}
    }
    if let Some(Len::Px(x)) = c.h {
        d = d.h(x);
    }
    if let Some(Len::Px(x)) = c.min_w {
        d = d.min_w(x);
    } else {
        d = d.min_w(0.0);
    }
    if let Some(Len::Px(x)) = c.max_w {
        d = d.max_w(x);
    }
    if let Some(Len::Px(x)) = c.min_h {
        d = d.min_h(x);
    }
    if let Some(Len::Px(x)) = c.max_h {
        d = d.max_h(x);
    }
    let [pt, pr, pb, pl] = c.pad;
    let [mt, mr, mb, ml] = c.margin;
    if let Some(v) = pt {
        d = d.pt(v);
    }
    if let Some(v) = pr {
        d = d.pr(v);
    }
    if let Some(v) = pb {
        d = d.pb(v);
    }
    if let Some(v) = pl {
        d = d.pl(v);
    }
    if let Some(v) = mt {
        d = d.mt(v);
    }
    if let Some(v) = mr {
        d = d.mr(v);
    }
    if let Some(v) = mb {
        d = d.mb(v);
    }
    if let Some(v) = ml {
        d = d.ml(v);
    }
    if c.border.iter().any(|b| b.is_some_and(|b| b > 0.0)) {
        let widths = c.border.map(|b| b.unwrap_or(0.0));
        let border = c.border_color.map_or_else(|| tok("border"), color);
        // Borders take no layout room in Rune; CSS boxes give them some, so the content keeps clear of them.
        let [bt, br, bb, bl] = widths;
        d = d.border_sides(widths, border);
        if pt.is_none() {
            d = d.pt(bt);
        }
        if pr.is_none() {
            d = d.pr(br);
        }
        if pb.is_none() {
            d = d.pb(bb);
        }
        if pl.is_none() {
            d = d.pl(bl);
        }
    }
    if let Some(bg) = c.bg {
        d = d.fill(color(bg));
    }
    if let Some(r) = c.radius {
        d = d.radius(r);
    }
    if let Some(o) = c.opacity {
        d = d.opacity(o.clamp(0.0, 1.0));
    }
    if c.clip == Some(true) {
        d = d.clip_rounded(c.radius.unwrap_or(0.0));
    }
    d
}
