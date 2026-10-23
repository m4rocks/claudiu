//! The CSS subset of the `ui` dialect: colours, lengths and the declarations that fill a `Css`.

use super::*;

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
        "bg" | "background" | "base" => theme::PANEL_BG,
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
pub(super) fn var(inner: &str) -> Option<Color> {
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

/// `em` and `rem` are taken as 14px: the panel's text size.
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
pub(super) fn grid_cols(v: &str) -> Option<u16> {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix("repeat(").and_then(|r| r.strip_suffix(')')) {
        let n = inner.split(',').next()?.trim();
        return Some(n.parse().unwrap_or(3));
    }
    let n = words(v).len();
    (n > 0).then_some(n as u16)
}

/// What a tag looks like before its own `style`. Unknown tags get nothing: they are plain blocks.
pub(super) fn default_css(tag: &str) -> &'static str {
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
