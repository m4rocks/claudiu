//! The `ui` dialect: forgiving HTML and a CSS subset, turned into a tree the renderer draws. No GPUI here (apart from
//! the SVG bitmap type), so everything below is testable. The input is whatever the model has streamed so far, so
//! nothing may be an error: an open tag cut short is dropped, open elements close at the end, unknown tags become
//! plain blocks, unknown CSS is ignored.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use gpui::{Image, ImageFormat};

use crate::theme;

/// An sRGB colour as `0xRRGGBB` and an alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub u32, pub f32);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Len {
    Px(f32),
    Pct(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Display {
    Block,
    Flex,
    Grid,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    Row,
    Col,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ax {
    Start,
    Center,
    End,
    Between,
    Around,
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// The CSS properties the renderer understands. Every field is optional: unset means "inherit or default".
/// Sides are ordered top, right, bottom, left.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Css {
    pub display: Option<Display>,
    pub dir: Option<Dir>,
    pub wrap: Option<bool>,
    pub gap_x: Option<f32>,
    pub gap_y: Option<f32>,
    pub align: Option<Ax>,
    pub justify: Option<Ax>,
    pub align_self: Option<Ax>,
    pub grow: Option<f32>,
    pub shrink: Option<f32>,
    pub w: Option<Len>,
    pub h: Option<Len>,
    pub min_w: Option<Len>,
    pub max_w: Option<Len>,
    pub min_h: Option<Len>,
    pub max_h: Option<Len>,
    pub pad: [Option<f32>; 4],
    pub margin: [Option<f32>; 4],
    pub bg: Option<Color>,
    pub color: Option<Color>,
    pub border: [Option<f32>; 4],
    /// shortcut: one colour for all four sides, upgrade if a model needs different side colours.
    pub border_color: Option<Color>,
    pub radius: Option<f32>,
    pub size: Option<f32>,
    pub weight: Option<f32>,
    pub italic: Option<bool>,
    pub mono: Option<bool>,
    pub text_align: Option<TextAlign>,
    /// A multiplier of the font size.
    pub line_height: Option<f32>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub opacity: Option<f32>,
    pub clip: Option<bool>,
    pub pre: Option<bool>,
    pub nowrap: Option<bool>,
    pub cols: Option<u16>,
    pub col_span: Option<u16>,
}

/// The sidebar-matching palette a model can name instead of a hex value (`var(--accent)` or just `accent`).
pub fn token(name: &str) -> Option<u32> {
    Some(match name {
        "bg" | "background" | "base" => theme::APP_BG,
        "surface" | "card" => theme::ROW_HOVER,
        "raised" | "surface2" => theme::ROW_ACTIVE,
        "border" => theme::BORDER,
        "text" | "fg" | "foreground" => theme::TEXT,
        "dim" | "muted" => theme::TEXT_DIM,
        "faint" => theme::TEXT_FAINT,
        "accent" | "primary" => theme::ACCENT,
        "ok" | "success" | "good" => theme::OK,
        "warn" | "warning" => theme::WARN,
        "danger" | "error" | "bad" => theme::DANGER,
        "info" => 0x61afef,
        "selection" => theme::SELECTION,
        _ => return None,
    })
}

/// CSS colour names, pulled onto the app's palette so a plain `red` still looks like Claudiu.
fn named(name: &str) -> Option<u32> {
    Some(match name {
        "red" => theme::DANGER,
        "green" | "lime" => theme::OK,
        "yellow" | "gold" => theme::WARN,
        "orange" => theme::ACCENT,
        "blue" => 0x61afef,
        "purple" | "violet" => 0xc678dd,
        "cyan" | "teal" | "aqua" => 0x56b6c2,
        "pink" | "magenta" => 0xff7b86,
        "white" => 0xffffff,
        "black" => 0x000000,
        "gray" | "grey" => theme::TEXT_DIM,
        "silver" => 0xc8ccd4,
        _ => return None,
    })
}

fn hex(s: &str) -> Option<Color> {
    let s = s.strip_prefix('#')?;
    if !s.is_ascii() || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = |i: usize, n: usize| u32::from_str_radix(&s[i..i + n], 16).ok();
    match s.len() {
        3 => Some(Color((v(0, 1)? * 0x110000) | (v(1, 1)? * 0x1100) | (v(2, 1)? * 0x11), 1.0)),
        4 => Some(Color((v(0, 1)? * 0x110000) | (v(1, 1)? * 0x1100) | (v(2, 1)? * 0x11), v(3, 1)? as f32 / 15.0)),
        6 => Some(Color(v(0, 6)?, 1.0)),
        8 => Some(Color(v(0, 6)?, v(6, 2)? as f32 / 255.0)),
        _ => None,
    }
}

/// `var(--name)` or `var(--name, fallback)`: the token, else the fallback colour.
fn var(inner: &str) -> Option<Color> {
    let (name, fallback) = inner.split_once(',').map_or((inner, None), |(n, f)| (n, Some(f)));
    let name = name.trim().trim_start_matches("--");
    token(name).map(|c| Color(c, 1.0)).or_else(|| fallback.and_then(color))
}

pub fn color(v: &str) -> Option<Color> {
    let v = v.trim().to_ascii_lowercase();
    if v == "transparent" {
        return Some(Color(0, 0.0));
    }
    if let Some(c) = hex(&v) {
        return Some(c);
    }
    let call = |name: &str| v.strip_prefix(name).and_then(|r| r.strip_prefix('(')).and_then(|r| r.strip_suffix(')'));
    if let Some(inner) = call("var") {
        return var(inner);
    }
    if let Some(inner) = call("rgb").or_else(|| call("rgba")) {
        let n: Vec<f32> = inner
            .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .map(|s| s.strip_suffix('%').map_or_else(|| s.parse().ok(), |p| p.parse::<f32>().ok().map(|x| x / 100.0)))
            .collect::<Option<_>>()?;
        if n.len() < 3 {
            return None;
        }
        let c = |x: f32| x.clamp(0.0, 255.0) as u32;
        return Some(Color(c(n[0]) << 16 | c(n[1]) << 8 | c(n[2]), n.get(3).copied().unwrap_or(1.0).clamp(0.0, 1.0)));
    }
    token(&v).or_else(|| named(&v)).map(|c| Color(c, 1.0))
}

/// Splits on whitespace outside parentheses, so `rgba(0, 0, 0, .5)` stays one word.
fn words(v: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (vec![], 0i32, None);
    for (i, c) in v.char_indices() {
        match c {
            '(' => {
                depth += 1;
                start.get_or_insert(i);
            }
            ')' => depth = (depth - 1).max(0),
            c if c.is_whitespace() && depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(&v[s..i]);
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        out.push(&v[s..]);
    }
    out
}

/// `em` and `rem` are taken as 14px: the chat's text size.
fn len(v: &str) -> Option<Len> {
    let v = v.trim().to_ascii_lowercase();
    if let Some(p) = v.strip_suffix('%') {
        return p.parse().ok().map(Len::Pct);
    }
    let (n, scale) = if let Some(n) = v.strip_suffix("rem") {
        (n, 14.0)
    } else if let Some(n) = v.strip_suffix("em") {
        (n, 14.0)
    } else if let Some(n) = v.strip_suffix("px") {
        (n, 1.0)
    } else {
        (v.as_str(), 1.0)
    };
    n.parse::<f32>().ok().map(|x| Len::Px(x * scale))
}

fn px_of(v: &str) -> Option<f32> {
    match len(v)? {
        Len::Px(x) => Some(x),
        Len::Pct(_) => None,
    }
}

fn sides(v: &str) -> Option<[Option<f32>; 4]> {
    let w: Vec<Option<f32>> = words(v).into_iter().map(px_of).collect();
    Some(match w.len() {
        1 => [w[0]; 4],
        2 => [w[0], w[1], w[0], w[1]],
        3 => [w[0], w[1], w[2], w[1]],
        4 => [w[0], w[1], w[2], w[3]],
        _ => return None,
    })
}

fn ax(v: &str) -> Option<Ax> {
    Some(match v.trim() {
        "flex-start" | "start" | "self-start" | "left" => Ax::Start,
        "center" => Ax::Center,
        "flex-end" | "end" | "self-end" | "right" => Ax::End,
        "space-between" => Ax::Between,
        "space-around" | "space-evenly" => Ax::Around,
        "stretch" => Ax::Stretch,
        _ => return None,
    })
}

fn flag(v: &str, yes: &[&str]) -> bool {
    words(v).iter().any(|w| yes.contains(w))
}

impl Css {
    /// Applies `name: value; …`, later declarations winning.
    pub fn apply(&mut self, decls: &str) {
        for d in decls.split(';') {
            if let Some((k, v)) = d.split_once(':') {
                self.set(&k.trim().to_ascii_lowercase(), v.trim().trim_end_matches("!important").trim());
            }
        }
    }

    fn set(&mut self, k: &str, v: &str) {
        let side = |name: &str| ["top", "right", "bottom", "left"].iter().position(|s| name.ends_with(s));
        match k {
            "display" => {
                self.display = Some(match v {
                    "flex" | "inline-flex" => Display::Flex,
                    "grid" | "inline-grid" => Display::Grid,
                    "none" => Display::None,
                    _ => Display::Block,
                })
            }
            "flex-direction" => self.dir = Some(if v.starts_with("column") { Dir::Col } else { Dir::Row }),
            "flex-wrap" => self.wrap = Some(v.starts_with("wrap")),
            "flex-flow" => {
                self.dir = Some(if flag(v, &["column", "column-reverse"]) { Dir::Col } else { Dir::Row });
                self.wrap = Some(flag(v, &["wrap"]));
            }
            "gap" | "grid-gap" => {
                let w: Vec<_> = words(v).into_iter().map(px_of).collect();
                self.gap_y = w.first().copied().flatten();
                self.gap_x = w.get(1).or(w.first()).copied().flatten();
            }
            "row-gap" => self.gap_y = px_of(v),
            "column-gap" => self.gap_x = px_of(v),
            "align-items" => self.align = ax(v),
            "justify-content" => self.justify = ax(v),
            "align-self" => self.align_self = ax(v),
            "flex" => {
                let w = words(v);
                match w.first().copied() {
                    Some("none") => (self.grow, self.shrink) = (Some(0.0), Some(0.0)),
                    Some("auto") => (self.grow, self.shrink) = (Some(1.0), Some(1.0)),
                    Some(g) => {
                        self.grow = g.parse().ok();
                        self.shrink = w.get(1).and_then(|s| s.parse().ok()).or(Some(1.0));
                    }
                    None => {}
                }
            }
            "flex-grow" => self.grow = v.parse().ok(),
            "flex-shrink" => self.shrink = v.parse().ok(),
            "width" => self.w = len(v),
            "height" => self.h = len(v),
            "min-width" => self.min_w = len(v),
            "max-width" => self.max_w = len(v),
            "min-height" => self.min_h = len(v),
            "max-height" => self.max_h = len(v),
            "padding" | "margin" => {
                if let Some(s) = sides(v) {
                    let dst = if k == "padding" { &mut self.pad } else { &mut self.margin };
                    for i in 0..4 {
                        dst[i] = s[i].or(dst[i]);
                    }
                }
            }
            _ if k.starts_with("padding-") || k.starts_with("margin-") => {
                if let (Some(i), Some(x)) = (side(k), px_of(v)) {
                    if k.starts_with("padding") { self.pad[i] = Some(x) } else { self.margin[i] = Some(x) }
                }
            }
            // Gradients and images have no counterpart; the first plain colour is the best stand-in.
            "background" | "background-color" => {
                if let Some(c) = words(v).into_iter().find_map(color) {
                    self.bg = Some(c)
                }
            }
            "color" => self.color = color(v).or(self.color),
            "opacity" => self.opacity = v.parse().ok(),
            "border" => self.border_shorthand(None, v),
            "border-color" => self.border_color = words(v).first().and_then(|w| color(w)).or(self.border_color),
            "border-width" => {
                if let Some(s) = sides(v) {
                    self.border = s;
                }
            }
            "border-radius" => {
                self.radius = words(v).first().and_then(|w| if w.ends_with('%') { Some(999.0) } else { px_of(w) }).map(|r| r.min(999.0))
            }
            _ if k.starts_with("border-") && side(k).is_some() && !k.ends_with("radius") => self.border_shorthand(side(k), v),
            "font-size" => self.size = len(v).map(|l| match l {
                Len::Px(x) => x,
                Len::Pct(p) => 14.0 * p / 100.0,
            }),
            "font-weight" => {
                self.weight = match v {
                    "bold" => Some(700.0),
                    "normal" => Some(400.0),
                    "bolder" => Some(800.0),
                    "lighter" => Some(300.0),
                    n => n.parse().ok(),
                }
            }
            "font-style" => self.italic = Some(v.starts_with("italic") || v.starts_with("oblique")),
            "font-family" => {
                let v = v.to_ascii_lowercase();
                self.mono = Some(["mono", "consolas", "courier", "menlo", "code", "lilex"].iter().any(|m| v.contains(m)));
            }
            "text-align" => {
                self.text_align = match v {
                    "center" => Some(TextAlign::Center),
                    "right" | "end" => Some(TextAlign::Right),
                    _ => Some(TextAlign::Left),
                }
            }
            "line-height" => {
                self.line_height = match len(v) {
                    Some(Len::Px(x)) if !v.ends_with("px") => Some(x / if v.ends_with("em") { 14.0 } else { 1.0 }),
                    Some(Len::Pct(p)) => Some(p / 100.0),
                    _ => None,
                }
            }
            "text-decoration" | "text-decoration-line" => {
                self.underline = Some(flag(v, &["underline"]));
                self.strike = Some(flag(v, &["line-through"]));
            }
            "overflow" | "overflow-x" | "overflow-y" => self.clip = Some(flag(v, &["hidden", "clip", "auto", "scroll"])),
            "white-space" => {
                self.pre = Some(v.starts_with("pre"));
                self.nowrap = Some(v == "nowrap");
            }
            "grid-template-columns" => self.cols = grid_cols(v),
            "grid-column" => self.col_span = words(v).iter().position(|w| *w == "span").and_then(|i| words(v).get(i + 1).and_then(|n| n.parse().ok())),
            _ => {}
        }
    }

    fn border_shorthand(&mut self, side: Option<usize>, v: &str) {
        let (mut width, mut none, mut col) = (None, false, None);
        for w in words(v) {
            match w {
                "none" | "hidden" => none = true,
                "solid" | "dashed" | "dotted" | "double" => {}
                _ => match (color(w), px_of(w)) {
                    (Some(c), _) => col = Some(c),
                    (None, Some(x)) => width = Some(x),
                    _ => {}
                },
            }
        }
        let width = if none { 0.0 } else { width.unwrap_or(1.0) };
        match side {
            Some(i) => self.border[i] = Some(width),
            None => self.border = [Some(width); 4],
        }
        self.border_color = col.or(self.border_color);
    }
}

/// shortcut: columns are equal-width; a track list only gives the count, and `auto-fit`/`auto-fill` is taken as 3.
fn grid_cols(v: &str) -> Option<u16> {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix("repeat(").and_then(|r| r.strip_suffix(')')) {
        let n = inner.split(',').next()?.trim();
        return Some(n.parse().unwrap_or(3));
    }
    let n = words(v).len();
    (n > 0).then_some(n as u16)
}

/// What a tag looks like before its own `style`. Unknown tags get nothing: they are plain blocks.
fn default_css(tag: &str) -> &'static str {
    match tag {
        "h1" => "font-size:20px;font-weight:600",
        "h2" => "font-size:17px;font-weight:600",
        "h3" => "font-size:15px;font-weight:600",
        "h4" | "h5" | "h6" => "font-size:14px;font-weight:600",
        "strong" | "b" => "font-weight:700",
        "em" | "i" | "cite" | "dfn" => "font-style:italic",
        "u" | "ins" => "text-decoration:underline",
        "s" | "del" | "strike" => "text-decoration:line-through",
        "a" => "color:accent;text-decoration:underline",
        "small" => "font-size:12px;color:dim",
        "mark" => "background:#e5c07b44",
        "code" | "samp" | "tt" => "font-family:monospace;background:raised;font-size:12.5px",
        "kbd" => "font-family:monospace;background:raised;font-size:12px",
        "pre" => "font-family:monospace;font-size:12.5px;white-space:pre;background:surface;border:1px solid border;border-radius:8px;padding:10px 12px",
        "blockquote" => "border-left:3px solid #d9775788;padding:2px 0 2px 12px;color:dim",
        "hr" => "height:1px;background:border;margin:6px 0",
        "ul" | "ol" => "gap:3px;padding-left:2px",
        "table" => "border:1px solid border;border-radius:8px;overflow:hidden",
        "tr" => "display:flex;border-bottom:1px solid border",
        "th" => "flex:1;min-width:0;padding:6px 10px;font-weight:600;color:dim;background:surface",
        "td" => "flex:1;min-width:0;padding:6px 10px",
        "card" => "background:surface;border:1px solid border;border-radius:10px;padding:12px;gap:6px",
        "badge" => "display:flex;align-self:flex-start;padding:1px 9px;border-radius:999px;background:raised;border:1px solid border;font-size:11.5px",
        "callout" => "background:surface;border-left:3px solid accent;border-radius:6px;padding:8px 12px;gap:4px",
        "progress" => "height:6px;border-radius:999px;background:raised;overflow:hidden;color:accent",
        "row" => "display:flex;gap:8px;align-items:center",
        "col" => "gap:6px",
        "grid" => "display:grid;gap:8px;grid-template-columns:repeat(3,1fr)",
        _ => "",
    }
}

#[derive(Clone)]
pub struct El {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub css: Css,
    pub children: Vec<Node>,
}

impl El {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn new(tag: String, attrs: Vec<(String, String)>) -> Self {
        let mut css = Css::default();
        css.apply(default_css(&tag));
        if let Some(s) = attrs.iter().find(|(n, _)| n == "style") {
            css.apply(&s.1);
        }
        if let Some(n) = attrs.iter().find(|(n, _)| n == "cols").and_then(|(_, v)| v.parse().ok()) {
            css.cols = Some(n);
        }
        Self { tag, attrs, css, children: vec![] }
    }
}

/// A drawing, rasterised once here (parsing runs off the UI thread) at twice its size so it stays sharp.
#[derive(Clone)]
pub struct Svg {
    pub image: Arc<Image>,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone)]
pub enum Node {
    Text(String),
    El(El),
    /// `None` while the closing `</svg>` has not streamed in yet, or when the drawing is unusable.
    Svg(Option<Svg>),
}

#[derive(Clone)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// Identifies this block, so interactive elements in two blocks never share an id.
    pub uid: u64,
}

const VOID: [&str; 8] = ["br", "hr", "img", "input", "meta", "link", "wbr", "col-group"];

pub fn parse_tree(src: &str) -> Tree {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    Tree { nodes: parse(src), uid: h.finish() }
}

pub fn parse(src: &str) -> Vec<Node> {
    let (mut stack, mut root): (Vec<El>, Vec<Node>) = (vec![], vec![]);
    let mut i = 0;
    while i < src.len() {
        let rest = &src[i..];
        if rest.starts_with('<') {
            if rest.starts_with("<!--") {
                i = rest.find("-->").map_or(src.len(), |p| i + p + 3);
                continue;
            }
            if rest.starts_with("</") {
                let Some(end) = rest.find('>') else { break };
                let name = rest[2..end].trim().to_ascii_lowercase();
                if stack.iter().any(|e| e.tag == name) {
                    while let Some(top) = stack.last() {
                        let done = top.tag == name;
                        close(&mut stack, &mut root);
                        if done {
                            break;
                        }
                    }
                }
                i += end + 1;
                continue;
            }
            if rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
                // A tag cut off by the end of the stream is dropped; the next chunk completes it.
                let Some((tag, attrs, self_closing, used)) = open_tag(rest) else { break };
                i += used;
                match tag.as_str() {
                    "script" | "style" => {
                        let end = find_ci(&src[i..], &format!("</{tag}")).map(|p| i + p);
                        i = end.and_then(|e| src[e..].find('>').map(|p| e + p + 1)).unwrap_or(src.len());
                    }
                    "svg" => match find_ci(&src[i..], "</svg") {
                        Some(p) => {
                            let node = Node::Svg(svg_node(&attrs, &src[i..i + p]));
                            push(&mut stack, &mut root, node);
                            i = src[i + p..].find('>').map_or(src.len(), |q| i + p + q + 1);
                        }
                        None => {
                            push(&mut stack, &mut root, Node::Svg(None));
                            i = src.len();
                        }
                    },
                    _ => {
                        implied_end(&tag, &mut stack, &mut root);
                        let el = El::new(tag, attrs);
                        if self_closing || VOID.contains(&el.tag.as_str()) {
                            push(&mut stack, &mut root, Node::El(el));
                        } else {
                            stack.push(el);
                        }
                    }
                }
                continue;
            }
        }
        let first = rest.chars().next().map_or(1, char::len_utf8);
        let end = rest[first..].find('<').map_or(src.len(), |p| i + first + p);
        push(&mut stack, &mut root, Node::Text(decode(&src[i..end])));
        i = end;
    }
    while !stack.is_empty() {
        close(&mut stack, &mut root);
    }
    mark_last_rows(&mut root);
    root
}

fn push(stack: &mut [El], root: &mut Vec<Node>, n: Node) {
    match stack.last_mut() {
        Some(e) => e.children.push(n),
        None => root.push(n),
    }
}

fn close(stack: &mut Vec<El>, root: &mut Vec<Node>) {
    if let Some(e) = stack.pop() {
        push(stack, root, Node::El(e));
    }
}

/// `<li>`, `<p>`, `<td>`… are often left open; a sibling of the same kind ends the previous one.
fn implied_end(tag: &str, stack: &mut Vec<El>, root: &mut Vec<Node>) {
    let top = |s: &[El]| s.last().map(|e| e.tag.clone()).unwrap_or_default();
    let ends: &[&str] = match tag {
        "li" => &["li"],
        "p" => &["p"],
        "td" | "th" => &["td", "th"],
        "tr" => &["td", "th", "tr"],
        _ => return,
    };
    while ends.contains(&top(stack).as_str()) {
        close(stack, root);
    }
}

/// The last row of a table loses its bottom rule, so it does not double the table's own border.
fn mark_last_rows(nodes: &mut [Node]) {
    fn last_row(nodes: &mut [Node]) -> Option<&mut El> {
        let mut found = None;
        for n in nodes {
            if let Node::El(e) = n {
                if e.tag == "tr" {
                    found = Some(e);
                } else if matches!(e.tag.as_str(), "thead" | "tbody" | "tfoot")
                    && let Some(r) = last_row(&mut e.children) {
                        found = Some(r);
                    }
            }
        }
        found
    }
    for n in nodes {
        if let Node::El(e) = n {
            if e.tag == "table"
                && let Some(r) = last_row(&mut e.children) {
                    r.css.border[2] = Some(0.0);
                }
            mark_last_rows(&mut e.children);
        }
    }
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.to_ascii_lowercase().find(&needle.to_ascii_lowercase())
}

/// `<tag a="b" c>` at the start of `s`: (name, attributes, self-closing, bytes used). `None` when the `>` is missing.
fn open_tag(s: &str) -> Option<(String, Vec<(String, String)>, bool, usize)> {
    let b = s.as_bytes();
    let mut i = 1;
    let word = |i: &mut usize, stop: &dyn Fn(u8) -> bool| {
        let start = *i;
        while *i < b.len() && !stop(b[*i]) {
            *i += 1;
        }
        s[start..*i].to_string()
    };
    let name = word(&mut i, &|c| c.is_ascii_whitespace() || c == b'>' || c == b'/').to_ascii_lowercase();
    let mut attrs = vec![];
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || (b[i] == b'/' && b.get(i + 1) != Some(&b'>'))) {
            i += 1;
        }
        match b.get(i)? {
            b'>' => return Some((name, attrs, false, i + 1)),
            b'/' => return Some((name, attrs, true, i + 2)),
            _ => {}
        }
        let key = word(&mut i, &|c| c.is_ascii_whitespace() || c == b'=' || c == b'>' || c == b'/').to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if b.get(i)? == &b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            match *b.get(i)? {
                q @ (b'"' | b'\'') => {
                    i += 1;
                    value = word(&mut i, &|c| c == q);
                    b.get(i)?;
                    i += 1;
                }
                _ => value = word(&mut i, &|c| c.is_ascii_whitespace() || c == b'>'),
            }
        }
        if !key.is_empty() {
            attrs.push((key, decode(&value)));
        } else if i < b.len() && !matches!(b[i], b'>' | b'/') {
            i += 1; // a stray byte such as a lone '='
        }
    }
}

pub fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let decoded = rest.find(';').filter(|&e| e <= 10).and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => '\u{a0}',
                "mdash" => '—',
                "ndash" => '–',
                "hellip" => '…',
                "bull" => '•',
                "middot" => '·',
                "times" => '×',
                "rarr" => '→',
                "larr" => '←',
                "uarr" => '↑',
                "darr" => '↓',
                "deg" => '°',
                "plusmn" => '±',
                "laquo" => '«',
                "raquo" => '»',
                "copy" => '©',
                "reg" => '®',
                "trade" => '™',
                "euro" => '€',
                n => {
                    let num = n.strip_prefix('#')?;
                    let code = num.strip_prefix(['x', 'X']).map_or_else(|| num.parse().ok(), |h| u32::from_str_radix(h, 16).ok());
                    char::from_u32(code?)?
                }
            };
            Some((c, e + 1))
        });
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Replaces `var(--token)` and `currentColor` with hex values: the SVG renderer knows neither.
pub fn svg_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find("var(") {
        out.push_str(&rest[..p]);
        let after = &rest[p + 4..];
        match after.find(')').and_then(|e| var(&after[..e]).map(|c| (c, e))) {
            Some((Color(c, _), e)) => {
                out.push_str(&format!("#{c:06x}"));
                rest = &after[e + 1..];
            }
            None => {
                out.push_str("var(");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let text = format!("#{:06x}", theme::TEXT);
    out.replace("currentColor", &text).replace("currentcolor", &text)
}

/// Rebuilds the root `<svg>` with a namespace, a viewBox and a doubled pixel size, keeping the inner markup as written.
fn svg_node(attrs: &[(String, String)], inner: &str) -> Option<Svg> {
    let get = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let num = |s: &str| s.trim().trim_end_matches("px").parse::<f32>().ok().filter(|x| *x > 0.0);
    let vb: Option<[f32; 4]> = get("viewbox").and_then(|v| {
        let n: Vec<f32> = v.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect();
        n.try_into().ok().filter(|n: &[f32; 4]| n[2] > 0.0 && n[3] > 0.0)
    });
    let ratio = vb.map(|n| n[2] / n[3]);
    let (w, h) = match (get("width").and_then(num), get("height").and_then(num)) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w / ratio?),
        (None, Some(h)) => (h * ratio?, h),
        (None, None) => {
            let n = vb?;
            (n[2], n[3])
        }
    };
    if w > 2000.0 || h > 2000.0 {
        return None;
    }
    let view = vb.map_or_else(|| format!("0 0 {w} {h}"), |n| format!("{} {} {} {}", n[0], n[1], n[2], n[3]));
    let extra: String = attrs
        .iter()
        .filter(|(n, _)| !matches!(n.as_str(), "width" | "height" | "viewbox" | "xmlns" | "xmlns:xlink" | "class" | "id"))
        .map(|(n, v)| format!(" {}=\"{}\"", if n == "preserveaspectratio" { "preserveAspectRatio" } else { n }, v.replace('"', "&quot;")))
        .collect();
    // Twice the size keeps it sharp on dense screens, but the bitmap stays under about 1600px a side.
    let scale = (1600.0 / w.max(h)).clamp(1.0, 2.0);
    let svg = svg_tokens(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"{view}\" width=\"{}\" height=\"{}\"{extra}>{inner}</svg>",
        w * scale,
        h * scale
    ));
    // The message is parsed again for every streamed chunk. A new `Image` each time would be decoded and kept by gpui
    // again, so the same drawing must give the same `Image`.
    static CACHE: Mutex<Option<HashMap<String, Arc<Image>>>> = Mutex::new(None);
    let mut cache = CACHE.lock().ok()?;
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() > 64 {
        cache.clear();
    }
    let image = cache.entry(svg.clone()).or_insert_with(|| Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))).clone();
    Some(Svg { image, w, h })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(n: &Node) -> &El {
        match n {
            Node::El(e) => e,
            _ => panic!("not an element"),
        }
    }

    fn text(n: &Node) -> &str {
        match n {
            Node::Text(t) => t,
            _ => panic!("not text"),
        }
    }

    #[test]
    fn self_closing_tags_do_not_swallow_their_siblings() {
        let nodes = parse("<row><badge/><badge>b</badge></row>");
        let row = el(&nodes[0]);
        assert_eq!(row.children.len(), 2);
        assert!(el(&row.children[0]).children.is_empty());
    }

    #[test]
    fn partial_input_never_fails() {
        // A tag cut mid-way is dropped, open elements close at the end.
        let nodes = parse("<div style=\"gap:4px\"><p>hello <str");
        let p = el(&el(&nodes[0]).children[0]);
        assert_eq!(p.tag, "p");
        assert_eq!(text(&p.children[0]), "hello ");
        let cut = parse("<div style=\"co");
        assert!(cut.is_empty());
        assert_eq!(parse("a < b & c").iter().map(text).collect::<String>(), "a < b & c");
        // Multi-byte characters at the start of text, in text, and as an attribute value or cut-off tag.
        assert_eq!(parse("● ok <b>é</b> ✓").len(), 3);
        let svg = parse("<svg viewBox=\"0 0 9 9\" width=\"9\"><text>● 日本</text></svg>");
        assert!(matches!(svg[0], Node::Svg(Some(_))));
        assert!(parse("<p title=\"●").is_empty());
    }

    #[test]
    fn unclosed_list_items_and_cells_are_siblings() {
        let nodes = parse("<ul><li>a<li>b</ul><table><tr><td>1<td>2</table>");
        assert_eq!(el(&nodes[0]).children.len(), 2);
        assert_eq!(el(&el(&el(&nodes[1]).children[0]).children[0]).tag, "td");
        assert_eq!(el(&el(&nodes[1]).children[0]).children.len(), 2);
    }

    #[test]
    fn unknown_tags_are_blocks_and_scripts_vanish() {
        let nodes = parse("<script>alert(1)</script><flux-capacitor style=\"color:red\">x</flux-capacitor>");
        assert_eq!(nodes.len(), 1);
        assert_eq!(el(&nodes[0]).tag, "flux-capacitor");
        assert_eq!(el(&nodes[0]).css.display, None);
    }

    #[test]
    fn css_tokens_and_literals_resolve() {
        let mut c = Css::default();
        c.apply("color: var(--accent); background:#fff8; border:1px solid var(--missing, rgb(10,20,30)); padding:4px 8px; gap:6px 12px; flex:1; width:50%");
        assert_eq!(c.color, Some(Color(theme::ACCENT, 1.0)));
        assert_eq!(c.bg.map(|b| b.0), Some(0xffffff));
        assert_eq!(c.border_color, Some(Color(0x0a141e, 1.0)));
        assert_eq!(c.pad, [Some(4.0), Some(8.0), Some(4.0), Some(8.0)]);
        assert_eq!((c.gap_y, c.gap_x), (Some(6.0), Some(12.0)));
        assert_eq!((c.grow, c.shrink), (Some(1.0), Some(1.0)));
        assert_eq!(c.w, Some(Len::Pct(50.0)));
        assert_eq!(color("red"), Some(Color(theme::DANGER, 1.0)));
        assert_eq!(color("#d9775780").map(|c| (c.0, (c.1 * 255.0).round() as u32)), Some((0xd97757, 128)));
        assert_eq!(color("nonsense"), None);
    }

    #[test]
    fn style_attribute_overrides_the_tag_default() {
        let nodes = parse("<h1 style=\"font-size:30px\">x</h1><card style=\"padding:0\">y</card>");
        assert_eq!(el(&nodes[0]).css.size, Some(30.0));
        assert_eq!(el(&nodes[0]).css.weight, Some(600.0));
        assert_eq!(el(&nodes[1]).css.pad, [Some(0.0); 4]);
        assert!(el(&nodes[1]).css.bg.is_some());
    }

    #[test]
    fn grid_columns() {
        assert_eq!(grid_cols("repeat(4, 1fr)"), Some(4));
        assert_eq!(grid_cols("1fr 2fr 1fr"), Some(3));
        assert_eq!(grid_cols("repeat(auto-fit, minmax(120px, 1fr))"), Some(3));
        assert_eq!(el(&parse("<grid cols=\"2\"></grid>")[0]).css.cols, Some(2));
    }

    #[test]
    fn entities_decode() {
        assert_eq!(decode("a &amp; b &lt;c&gt; &#65;&#x42; &bogus; &"), "a & b <c> AB &bogus; &");
    }

    #[test]
    fn svg_tokens_and_size() {
        assert_eq!(svg_tokens("fill=\"var(--accent)\" stroke=\"var(--nope)\""), "fill=\"#d97757\" stroke=\"var(--nope)\"");
        assert!(svg_tokens("<g color=\"currentColor\">").contains("#e4e4e4"));
        let nodes = parse("<svg viewBox=\"0 0 200 100\" width=\"100\"><rect width=\"10\" height=\"10\"/></svg><p>after</p>");
        match &nodes[0] {
            Node::Svg(Some(s)) => assert_eq!((s.w, s.h), (100.0, 50.0)),
            _ => panic!("svg expected"),
        }
        assert_eq!(el(&nodes[1]).tag, "p");
        // Parsed again (the next streamed chunk): the same drawing is the same image, so gpui does not decode it twice.
        let again = parse("<svg viewBox=\"0 0 200 100\" width=\"100\"><rect width=\"10\" height=\"10\"/></svg>");
        match (&nodes[0], &again[0]) {
            (Node::Svg(Some(a)), Node::Svg(Some(b))) => assert!(Arc::ptr_eq(&a.image, &b.image)),
            _ => panic!("svg expected"),
        }
        // Still streaming: a placeholder, and nothing after it is misread as markup.
        assert!(matches!(parse("<svg viewBox=\"0 0 9 9\"><rect")[0], Node::Svg(None)));
        // No size at all cannot be drawn.
        assert!(matches!(parse("<svg><rect/></svg>")[0], Node::Svg(None)));
    }

    #[test]
    fn last_table_row_has_no_bottom_rule() {
        let nodes = parse("<table><thead><tr><th>h</th></tr></thead><tbody><tr><td>a</td></tr><tr><td>b</td></tr></tbody></table>");
        let t = el(&nodes[0]);
        let body = el(&t.children[1]);
        assert_eq!(el(&body.children[0]).css.border[2], Some(1.0));
        assert_eq!(el(&body.children[1]).css.border[2], Some(0.0));
        assert_eq!(el(&el(&t.children[0]).children[0]).css.border[2], Some(1.0));
    }
}
