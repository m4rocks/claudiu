//! `<chart>`: a bar or line chart from data, drawn as an SVG so the model needn't plot one by hand.
//!
//! `<chart type="bar" labels="a,b" values="3,5"></chart>`, or several `<series name=".." values=".." color=".."/>`
//! children in place of `values`. Bars run sideways, so long labels fit.

use super::{El, Node, Svg, svg_node};

const WIDTH: f32 = 640.0;
const LINE_HEIGHT: f32 = 220.0;
const COLORS: [&str; 4] = ["var(--accent)", "var(--info)", "var(--ok)", "var(--warn)"];
const LABEL_W: f32 = 120.0;
const VALUE_W: f32 = 64.0;
const BAR_H: f32 = 16.0;
const GROUP_GAP: f32 = 12.0;
const LEGEND_H: f32 = 24.0;

struct Series {
    name: String,
    values: Vec<f32>,
    color: String,
}

/// The drawing for a `<chart>`, or `None` while it has no data yet.
pub fn chart(el: &El) -> Option<Svg> {
    let labels: Vec<String> = el.attr("labels").map_or(vec![], |l| l.split(',').map(|s| s.trim().to_string()).collect());
    let series = series(el);
    if series.is_empty() {
        return None;
    }
    let width = el.attr("width").and_then(|w| w.trim().parse().ok()).filter(|w| *w > 0.0).unwrap_or(WIDTH);
    let (inner, height) = match el.attr("type") {
        Some("line") => line(&labels, &series, width, el.attr("height").and_then(|h| h.trim().parse().ok()).unwrap_or(LINE_HEIGHT)),
        _ => bars(&labels, &series, width),
    };
    let attrs = [("width".to_string(), width.to_string()), ("height".to_string(), height.to_string())];
    svg_node(&attrs, &inner)
}

fn series(el: &El) -> Vec<Series> {
    // One bad value rejects the whole series: dropping it would shift the later values onto the wrong labels.
    let numbers = |s: &str| -> Option<Vec<f32>> {
        s.trim().trim_end_matches(',').split(',').map(|v| v.trim().parse::<f32>().ok().filter(|v| v.is_finite())).collect()
    };
    let color = |n: usize| COLORS[n % COLORS.len()].to_string();
    let mut out: Vec<Series> = vec![];
    if let Some(values) = el.attr("values").and_then(numbers) {
        out.push(Series { name: String::new(), values, color: el.attr("color").map_or(color(0), str::to_string) });
    }
    for child in &el.children {
        let Node::El(s) = child else { continue };
        if s.tag == "series" {
            let Some(values) = numbers(s.attr("values").unwrap_or("")) else { continue };
            out.push(Series { name: s.attr("name").unwrap_or("").to_string(), values, color: s.attr("color").map_or(color(out.len()), str::to_string) });
        }
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `12345.5` as `12345.5`, `3532` as `3532`: a whole number shows no decimals.
fn number(v: f32) -> String {
    if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v:.2}").trim_end_matches('0').to_string() }
}

fn text(x: f32, y: f32, anchor: &str, color: &str, s: &str) -> String {
    format!("<text x=\"{x}\" y=\"{y}\" text-anchor=\"{anchor}\" font-size=\"12\" fill=\"{color}\">{}</text>", escape(s))
}

/// A row of colour keys along the top, when series have names. Returns its markup and height.
fn legend(series: &[Series]) -> (String, f32) {
    if series.iter().all(|s| s.name.is_empty()) {
        return (String::new(), 0.0);
    }
    let mut x = 0.0;
    let mut out = String::new();
    for s in series {
        out += &format!("<rect x=\"{x}\" y=\"4\" width=\"10\" height=\"10\" rx=\"2\" fill=\"{}\"/>", escape(&s.color));
        out += &text(x + 15.0, 13.0, "start", "var(--dim)", &s.name);
        x += 15.0 + s.name.chars().count() as f32 * 7.0 + 18.0;
    }
    (out, LEGEND_H)
}

/// The range values are drawn over. Zero is always in it, so a bar's length means its value.
fn range(series: &[Series]) -> (f32, f32) {
    let all = || series.iter().flat_map(|s| s.values.iter().copied());
    let (low, high) = (all().fold(0.0, f32::min), all().fold(0.0, f32::max));
    if low == high { (0.0, 1.0) } else { (low, high) }
}

fn bars(labels: &[String], series: &[Series], width: f32) -> (String, f32) {
    let (mut out, top) = legend(series);
    let (low, high) = range(series);
    let groups = series.iter().map(|s| s.values.len()).max().unwrap_or(0);
    let plot = (width - LABEL_W - VALUE_W).max(1.0);
    let x_of = |v: f32| LABEL_W + (v - low) / (high - low) * plot;
    let group_h = series.len() as f32 * (BAR_H + 2.0);
    for g in 0..groups {
        let y = top + g as f32 * (group_h + GROUP_GAP);
        out += &text(LABEL_W - 10.0, y + group_h / 2.0 + 3.0, "end", "var(--text)", labels.get(g).map_or("", String::as_str));
        for (n, s) in series.iter().enumerate() {
            let Some(&v) = s.values.get(g) else { continue };
            let bar_y = y + n as f32 * (BAR_H + 2.0);
            let (from, to) = (x_of(v.min(0.0)), x_of(v.max(0.0)));
            out += &format!("<rect x=\"{from}\" y=\"{bar_y}\" width=\"{}\" height=\"{BAR_H}\" rx=\"3\" fill=\"{}\"/>", (to - from).max(1.0), escape(&s.color));
            out += &text(to + 6.0, bar_y + BAR_H - 4.0, "start", "var(--dim)", &number(v));
        }
    }
    let height = top + groups as f32 * (group_h + GROUP_GAP) - GROUP_GAP;
    (out, height.max(1.0))
}

fn line(labels: &[String], series: &[Series], width: f32, height: f32) -> (String, f32) {
    let (mut out, top) = legend(series);
    let (low, high) = range(series);
    let (left, bottom) = (48.0, height - 22.0);
    let plot_h = (bottom - top - 8.0).max(1.0);
    let y_of = |v: f32| bottom - (v - low) / (high - low) * plot_h;
    let points = series.iter().map(|s| s.values.len()).max().unwrap_or(0);
    let x_of = |i: usize| left + if points > 1 { i as f32 / (points - 1) as f32 * (width - left - 12.0) } else { 0.0 };
    for v in [low, (low + high) / 2.0, high] {
        let y = y_of(v);
        out += &format!("<line x1=\"{left}\" y1=\"{y}\" x2=\"{}\" y2=\"{y}\" stroke=\"var(--border)\"/>", width - 12.0);
        out += &text(left - 8.0, y + 4.0, "end", "var(--dim)", &number(v));
    }
    // Every label when they fit, or evenly spaced ones when they don't.
    let step = (labels.len() as f32 * 60.0 / width).ceil().max(1.0) as usize;
    for (i, label) in labels.iter().enumerate().take(points).step_by(step) {
        out += &text(x_of(i), height - 4.0, "middle", "var(--dim)", label);
    }
    for s in series {
        let path: Vec<String> = s.values.iter().enumerate().map(|(i, v)| format!("{},{}", x_of(i), y_of(*v))).collect();
        out += &format!("<polyline points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"2\" stroke-linejoin=\"round\"/>", path.join(" "), escape(&s.color));
        if let [only] = path.as_slice() {
            let (x, y) = only.split_once(',').unwrap_or(("0", "0"));
            out += &format!("<circle cx=\"{x}\" cy=\"{y}\" r=\"3\" fill=\"{}\"/>", escape(&s.color));
        }
    }
    (out, height)
}

#[cfg(test)]
mod tests {
    use super::super::parse_tree;
    use super::*;

    fn drawn(src: &str) -> Option<Svg> {
        match parse_tree(src).nodes.into_iter().next() {
            Some(Node::Svg(s)) => s,
            _ => panic!("a chart becomes a drawing"),
        }
    }

    fn el(src: &str) -> El {
        let mut nodes = super::super::parse(src);
        match nodes.remove(0) {
            Node::El(e) => *e,
            _ => panic!("element expected"),
        }
    }

    #[test]
    fn a_bar_is_as_long_as_its_share_of_the_largest_value() {
        let (out, _) = bars(&["a".into(), "b".into()], &series(&el("<chart values=\"100,50\"></chart>")), 284.0);
        // 284 wide leaves 100 for the bars, from x=120.
        assert!(out.contains("x=\"120\" y=\"0\" width=\"100\""), "{out}");
        assert!(out.contains("x=\"120\" y=\"30\" width=\"50\""), "{out}");
        assert!(out.contains(">100<") && out.contains(">50<"), "values are written beside the bars");
    }

    #[test]
    fn a_negative_bar_runs_left_of_zero() {
        let (out, _) = bars(&["a".into(), "b".into()], &series(&el("<chart values=\"50,-50\"></chart>")), 284.0);
        assert!(out.contains("x=\"170\" y=\"0\" width=\"50\""), "{out}");
        assert!(out.contains("x=\"120\" y=\"30\" width=\"50\""), "{out}");
    }

    #[test]
    fn line_points_span_the_plot_and_labels_are_escaped() {
        let (out, height) = line(&["<a>".into(), "b".into()], &series(&el("<chart values=\"0,10\"></chart>")), 260.0, 100.0);
        assert_eq!(height, 100.0);
        assert!(out.contains("points=\"48,78 248,8\""), "from bottom left to top right: {out}");
        assert!(out.contains("&lt;a&gt;") && !out.contains("<a>"));
    }

    #[test]
    fn series_get_their_own_colours_and_a_legend() {
        let e = el("<chart labels=\"a\"><series name=\"p95\" values=\"1\"/><series name=\"p50\" values=\"2\" color=\"#123456\"/></chart>");
        let s = series(&e);
        assert_eq!((s[0].color.as_str(), s[1].color.as_str()), ("var(--accent)", "#123456"));
        assert!(legend(&s).0.contains("p50"));
        assert_eq!(legend(&s).1, LEGEND_H);
    }

    #[test]
    fn a_series_with_a_bad_value_is_left_out_instead_of_shifting_its_values() {
        assert!(series(&el("<chart values=\"1,x,3\"></chart>")).is_empty());
        let e = el("<chart><series name=\"a\" values=\"1,2\"/><series name=\"b\" values=\"1,,3\"/></chart>");
        assert_eq!(series(&e).len(), 1, "only the whole series is kept");
        assert_eq!(series(&el("<chart values=\"1,2,\"></chart>"))[0].values, vec![1.0, 2.0], "a trailing comma is fine");
    }

    #[test]
    fn odd_data_draws_or_waits_without_failing() {
        assert!(drawn("<chart type=\"bar\"></chart>").is_none(), "no data yet");
        assert!(drawn("<chart values=\"x,y\"></chart>").is_none(), "nothing numeric");
        assert!(drawn("<chart values=\"0,0\"></chart>").is_some(), "all zero has no range but still draws");
        assert!(drawn("<chart type=\"line\" values=\"5\"></chart>").is_some(), "a single point");
        assert!(drawn("<chart labels=\"a,b,c\" values=\"1\"></chart>").is_some(), "more labels than values");
    }
}
