//! The `ui` dialect: forgiving HTML and a CSS subset, turned into a tree the renderer draws. No UI here (apart from
//! the SVG bitmap), so everything below is testable. The input is whatever the model has streamed so far, so
//! nothing may be an error: an open tag cut short is dropped, open elements close at the end, unknown tags become
//! plain blocks, unknown CSS is ignored.
//! `css` holds the CSS subset (colours, lengths, `Css`); this file holds the tree, the tag parser and entities.

mod chart;
mod css;
pub use css::*;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};


use crate::panels::theme;

#[derive(Clone)]
pub struct El {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub css: Css,
    pub children: Vec<Node>,
    /// This element's place among the block's interactive ones, so clicking one doesn't renumber those after it.
    pub seq: usize,
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
        Self { tag, attrs, css, children: vec![], seq: 0 }
    }
}

/// A drawing: its size on screen, and the root `<svg>` from after its name, without a width or height.
#[derive(Clone)]
pub struct Svg {
    rest: String,
    pub w: f32,
    pub h: f32,
}

impl Svg {
    /// The drawing as a bitmap of exactly the pixels it covers on a screen of this scale, decoded off the UI thread.
    pub fn image(&self, screen_scale: f32) -> rune::Image {
        let svg = format!("<svg width=\"{}\" height=\"{}\"{}", self.w, self.h, self.rest);
        // A view is built on every frame, and Rune keeps an image only while a handle to it lives: the same drawing
        // must give the same handle, so it is decoded once.
        static IMAGES: LazyLock<rune::ImageCache> = LazyLock::new(|| rune::ImageCache::new(Arc::new(crate::app::rt::wake)));
        static CACHE: LazyLock<Mutex<HashMap<(String, u32), rune::Image>>> = LazyLock::new(Default::default);
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 64 {
            cache.clear();
        }
        let size = self.w.max(self.h);
        cache.entry((svg.clone(), screen_scale.to_bits())).or_insert_with(|| IMAGES.svg(svg.into_bytes(), size, screen_scale)).clone()
    }
}

#[derive(Clone)]
pub enum Node {
    Text(String),
    El(Box<El>),
    /// `None` while the closing `</svg>` has not streamed in yet, or when the drawing is unusable.
    Svg(Option<Svg>),
}

#[derive(Clone)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// Identifies this block, so interactive elements in two blocks never share an id.
    pub uid: u64,
}

const VOID: [&str; 9] = ["br", "hr", "img", "input", "meta", "link", "wbr", "col-group", "series"];

pub fn parse_tree(src: &str) -> Tree {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    let mut nodes = parse(src);
    finish(&mut nodes, &mut 0);
    Tree { nodes, uid: h.finish() }
}

/// Turns charts into drawings and numbers the elements a click can change.
fn finish(nodes: &mut [Node], next: &mut usize) {
    for n in nodes {
        let Node::El(e) = n else { continue };
        if e.tag == "chart" {
            *n = Node::Svg(chart::chart(e));
            continue;
        }
        if matches!(e.tag.as_str(), "button" | "details" | "tabs") {
            e.seq = *next;
            *next += 1;
        }
        finish(&mut e.children, next);
    }
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
            if rest.get(1..).is_some_and(|r| r.starts_with(|c: char| c.is_ascii_alphabetic())) {
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
                            push(&mut stack, &mut root, Node::El(Box::new(el)));
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
        push(stack, root, Node::El(Box::new(e)));
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
                    found = Some(&mut **e);
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
type OpenTag = (String, Vec<(String, String)>, bool, usize);

fn open_tag(s: &str) -> Option<OpenTag> {
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

/// The app's text font by name. The SVG renderer knows none of the app's, and on its own draws text in Times New Roman.
const SVG_FONT: &str = if cfg!(windows) {
    "Segoe UI"
} else if cfg!(target_os = "macos") {
    "Helvetica Neue"
} else {
    "sans-serif"
};

/// Sets every `font-family` attribute to the app's font, so drawings read like the rest of the app. Monospace stays.
fn svg_font(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut rest = inner;
    while let Some(at) = rest.find("font-family=") {
        let value = at + "font-family=".len();
        let quote = rest[value..].chars().next().filter(|c| matches!(c, '"' | '\''));
        let end = quote.and_then(|q| rest[value + 1..].find(q)).map(|e| value + 1 + e);
        let Some(end) = end else { break };
        out.push_str(&rest[..value + 1]);
        let family = &rest[value + 1..end];
        out.push_str(if family.to_ascii_lowercase().contains("mono") { family } else { SVG_FONT });
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
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
        .filter(|(n, _)| !matches!(n.as_str(), "width" | "height" | "viewbox" | "xmlns" | "xmlns:xlink" | "class" | "id" | "font-family"))
        .map(|(n, v)| format!(" {}=\"{}\"", if n == "preserveaspectratio" { "preserveAspectRatio" } else { n }, v.replace('"', "&quot;")))
        .collect();
    let rest = svg_tokens(&format!(
        " xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"{view}\" font-family=\"{SVG_FONT}\"{extra}>{}</svg>",
        svg_font(inner)
    ));
    Some(Svg { rest, w, h })
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
    fn interactive_elements_keep_their_numbers_whatever_is_open() {
        let tree = parse_tree("<details><button send=\"a\">A</button></details><tabs><tab></tab></tabs><p>x</p><button send=\"b\">B</button>");
        let seq = |n: &Node| match n {
            Node::El(e) => e.seq,
            _ => panic!("element expected"),
        };
        let Node::El(details) = &tree.nodes[0] else { panic!("element expected") };
        assert_eq!(seq(&tree.nodes[0]), 0);
        assert_eq!(seq(&details.children[0]), 1, "numbered even while its section is closed");
        assert_eq!(seq(&tree.nodes[1]), 2);
        assert_eq!(seq(&tree.nodes[3]), 3);
    }

    #[test]
    fn a_drawing_becomes_exactly_the_pixels_it_covers() {
        let nodes = parse("<svg viewBox=\"0 0 200 100\" width=\"100\"><rect width=\"10\" height=\"10\"/></svg>");
        let Node::Svg(Some(s)) = &nodes[0] else { panic!("svg expected") };
        for (scale, w, h) in [(1.0, 100, 50), (1.5, 150, 75), (2.0, 200, 100)] {
            let size = s.image(scale).wait().expect("the drawing decodes");
            assert_eq!((size[0], size[1]), (w, h), "at scale {scale}");
        }
    }

    #[test]
    fn svg_text_uses_the_app_font_and_keeps_monospace() {
        assert_eq!(svg_font("<text font-family=\"Georgia, serif\">a</text>"), format!("<text font-family=\"{SVG_FONT}\">a</text>"));
        assert_eq!(svg_font("<text font-family='system-ui'>a</text><text font-family=\"x\"/>"), format!("<text font-family='{SVG_FONT}'>a</text><text font-family=\"{SVG_FONT}\"/>"));
        assert_eq!(svg_font("<text font-family=\"monospace\">a</text>"), "<text font-family=\"monospace\">a</text>");
        assert_eq!(svg_font("<text font-family=\"cut off"), "<text font-family=\"cut off", "half a streamed drawing is left alone");
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
        // Parsed again (the next streamed chunk): the same drawing is the same image, so it is not decoded twice.
        let again = parse("<svg viewBox=\"0 0 200 100\" width=\"100\"><rect width=\"10\" height=\"10\"/></svg>");
        match (&nodes[0], &again[0]) {
            (Node::Svg(Some(a)), Node::Svg(Some(b))) => assert_eq!(a.image(1.0).id(), b.image(1.0).id()),
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
