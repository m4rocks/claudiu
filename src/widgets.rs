//! Small stateless UI pieces shared by sidebar / main view / modals.

use std::time::Duration;

use gpui::{
    Animation, AnimationElement, AnimationExt, Div, ElementId, FontWeight, Hsla, InteractiveElement, ParentElement, SharedString,
    Stateful, Styled, div, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use crate::theme::{self, hsla};

/// The Clawd-style mark, drawn from a pixel pattern so it stays crisp at any size.
pub fn clawd(size: f32) -> Div {
    const PATTERN: [&str; 7] = [
        "..#######..", //
        "..#o###o#..",
        "###o###o###",
        "###########",
        "..#######..",
        "..#.#.#.#..",
        "..#.#.#.#..",
    ];
    let unit = size / 11.0;
    let body = hsla(theme::ACCENT);
    let eye = hsla(0x1a0f0b);
    let mut root = div().relative().w(px(size)).h(px(unit * 7.0)).flex_none();
    for (y, row) in PATTERN.iter().enumerate() {
        let mut x = 0;
        let cells: Vec<char> = row.chars().collect();
        while x < cells.len() {
            let c = cells[x];
            if c == '.' {
                x += 1;
                continue;
            }
            // Merge horizontal runs of the same color into one rect.
            let mut end = x + 1;
            while end < cells.len() && cells[end] == c {
                end += 1;
            }
            root = root.child(
                div()
                    .absolute()
                    .left(px(x as f32 * unit))
                    .top(px(y as f32 * unit))
                    .w(px((end - x) as f32 * unit))
                    .h(px(unit))
                    .bg(if c == 'o' { eye } else { body }),
            );
            x = end;
        }
    }
    root
}

pub fn section_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hsla(theme::TEXT_FAINT))
        .child(text.into())
}

/// Progress bar. `fraction == None` renders the neutral "unavailable" state.
pub fn meter(label: &str, fraction: Option<f32>, caption: String, tone: Option<Hsla>) -> Div {
    let track = hsla(0x1c1c1c);
    let color = tone.unwrap_or_else(|| match fraction {
        Some(f) if f >= 0.9 => hsla(theme::DANGER),
        Some(f) if f >= 0.7 => hsla(theme::WARN),
        _ => hsla(theme::ACCENT),
    });
    let pct = fraction
        .map(|f| format!("{:.0}%", (f * 100.0).round()))
        .unwrap_or_else(|| "—".into());
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .flex()
                .justify_between()
                .items_baseline()
                .child(section_label(label.to_string()))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(hsla(if fraction.is_some() {
                            theme::TEXT
                        } else {
                            theme::TEXT_FAINT
                        }))
                        .child(pct),
                ),
        )
        .child(
            div()
                .h(px(5.0))
                .w_full()
                .rounded(px(3.0))
                .bg(track)
                .child(match fraction {
                    Some(f) => div()
                        .h_full()
                        .w(gpui::relative(f.clamp(0.0, 1.0)))
                        .rounded(px(3.0))
                        .bg(color),
                    None => div().h_full(),
                }),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(hsla(theme::TEXT_FAINT))
                .child(caption),
        )
}

fn button_with(
    id: SharedString,
    label: SharedString,
    bg: Hsla,
    fg: Hsla,
    hover: Hsla,
    bold: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(10.0))
        .h(px(26.0))
        .flex()
        .items_center()
        .rounded(px(6.0))
        .bg(bg)
        .text_color(fg)
        .text_size(px(12.0))
        .font_weight(if bold {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .cursor_pointer()
        // GPUI panics if `hover` is set twice on one element, so every variant sets it exactly once, here.
        .hover(move |s| s.bg(hover))
        .child(label)
}

/// Compact text button. Caller attaches `.on_click`.
pub fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    primary: bool,
) -> Stateful<Div> {
    if primary {
        button_with(
            id.into(),
            label.into(),
            hsla(theme::ACCENT),
            hsla(0x17100d),
            hsla(0xe58a6b),
            true,
        )
    } else {
        button_with(
            id.into(),
            label.into(),
            hsla(0x1b1b1b),
            hsla(theme::TEXT),
            hsla(0x262626),
            false,
        )
    }
}

/// `button` with a Lucide icon before the label.
pub fn button_leading(id: impl Into<SharedString>, name: IconName, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    button(id, "", primary).child(icon(name, 13.0)).child(div().ml(px(5.0)).child(label.into()))
}

/// A button that can't be pressed right now: dimmed, no hover, no click handler attached by the caller.
pub fn button_disabled(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Stateful<Div> {
    button_with(id.into(), label.into(), hsla(0x1b1b1b), hsla(theme::TEXT_FAINT), hsla(0x1b1b1b), false).cursor_default()
}

/// One half of a split button (`( label | v )`): sits inside a rounded group and only shows a hover fill.
pub fn segment(id: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id.into())
        .h_full()
        .px(px(9.0))
        .flex()
        .items_center()
        .text_size(px(12.0))
        .text_color(hsla(theme::TEXT))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(0x262626)))
}

/// Destructive action (end session, quit).
pub fn danger_button(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Stateful<Div> {
    button_with(
        id.into(),
        label.into(),
        hsla(0x5a2327),
        hsla(0xffd9dc),
        hsla(0x74292e),
        true,
    )
}

/// A Lucide icon, `size` px square, in the text color it is drawn under.
pub fn icon(name: IconName, size: f32) -> Icon {
    Icon::new(name).size(px(size))
}

/// The "working" icon: Lucide's loader, turning.
pub fn spinner(id: impl Into<ElementId>, size: f32) -> AnimationElement<Icon> {
    icon(IconName::LoaderCircle, size).with_animation(
        id,
        Animation::new(Duration::from_millis(1000)).repeat(),
        |i, t| i.rotate(gpui::radians(t * std::f32::consts::TAU)),
    )
}

pub fn icon_button(id: impl Into<SharedString>, glyph: IconName) -> Stateful<Div> {
    div()
        .id(id.into())
        .size(px(22.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .text_color(hsla(theme::TEXT_DIM))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(0x262626)).text_color(hsla(theme::TEXT)))
        .child(icon(glyph, 14.0))
}

/// Monogram avatar: the first letter of `seed` on a color picked from it, so each account keeps its color.
pub fn avatar(seed: &str, size: f32) -> Div {
    const COLORS: [u32; 6] = [theme::ACCENT, 0x61afef, 0x98c379, 0xc678dd, 0xe5c07b, 0x56b6c2];
    let hash = seed.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let letter = seed.chars().find(|c| c.is_alphanumeric()).map_or('?', |c| c.to_ascii_uppercase());
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(hsla(COLORS[hash as usize % COLORS.len()]))
        .text_size(px(size * 0.55))
        .font_weight(FontWeight::BOLD)
        .text_color(hsla(0x17100d))
        .child(letter.to_string())
}

pub fn divider() -> Div {
    div().h(px(1.0)).w_full().bg(hsla(theme::BORDER))
}

/// Relative time like "now", "5m", "3h", "2d", "6w".
pub fn ago(ts: i64, now: i64) -> String {
    let d = (now - ts).max(0);
    match d {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", d / 60),
        3600..=86399 => format!("{}h", d / 3600),
        86400..=1_209_599 => format!("{}d", d / 86400),
        _ => format!("{}w", d / 604_800),
    }
}

/// "2h 14m" / "4d 8h" / "35m" for reset countdowns.
pub fn countdown(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Shorten a path for display: `~` for home, middle-ellipsis for long paths.
pub fn short_path(path: &std::path::Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    if let Some(home) = crate::platform::home_dir() {
        let h = home.to_string_lossy().into_owned();
        if let Some(rest) = s.strip_prefix(&h) {
            s = format!("~{rest}");
        }
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > 46 {
        let head: String = chars[..14].iter().collect();
        let tail: String = chars[chars.len() - 29..].iter().collect();
        s = format!("{head}…{tail}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_formats() {
        assert_eq!(ago(100, 100), "now");
        assert_eq!(ago(0, 90), "1m");
        assert_eq!(ago(0, 7300), "2h");
        assert_eq!(ago(0, 3 * 86400), "3d");
        assert_eq!(ago(0, 5 * 604_800), "5w");
        assert_eq!(countdown(2 * 3600 + 14 * 60 + 5), "2h 14m");
        assert_eq!(countdown(4 * 86400 + 8 * 3600), "4d 8h");
        assert_eq!(countdown(-5), "0m");
    }

    #[test]
    fn long_paths_are_shortened() {
        let p = std::path::Path::new(
            "/a/very/long/path/that/keeps/going/and/going/forever/and/ever/project",
        );
        assert!(short_path(p).chars().count() <= 46);
    }
}

/// Fades `el` in the first time it is drawn; a new `key` replays it (e.g. a different item or session).
/// It starts at `from` opacity so terminals never flash fully empty.
pub fn fade_in<E: gpui::IntoElement + Styled + 'static>(el: E, key: impl Into<SharedString>, ms: u64, from: f32) -> gpui::AnimationElement<E> {
    use gpui::{Animation, AnimationExt};
    el.with_animation(
        key.into(),
        Animation::new(std::time::Duration::from_millis(ms)).with_easing(gpui::ease_out_quint()),
        move |el, t| el.opacity(from + (1.0 - from) * t),
    )
}
