//! Small stateless UI pieces shared by sidebar / main view / modals. Each returns an `El`; the caller keys it and
//! attaches its handler (`on_press_tx`).

use rune::tok::motion;
use rune::{Color, Cursor, El, Icon, TextStyle, Transition, col, el, row, text};

use crate::panels::icons;
use crate::panels::scale;
use crate::panels::theme::{self, rgb};

/// UI text at `size` points (scaled by the user's setting) and `weight`.
pub fn font(size: f32, weight: f32) -> TextStyle {
    TextStyle::new(scale::px(size), weight)
}

/// A line of UI text in `color`.
pub fn label(s: impl Into<String>, size: f32, color: u32) -> El {
    text(s).font(font(size, 400.0)).color(rgb(color))
}

/// The Clawd-style mark, drawn from a pixel pattern so it stays crisp at any size.
pub fn clawd(size: f32) -> El {
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
    let mut root = el().w(size).h(unit * 7.0);
    for (y, line) in PATTERN.iter().enumerate() {
        let cells: Vec<char> = line.chars().collect();
        let mut x = 0;
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
                el().absolute(x as f32 * unit, y as f32 * unit).size((end - x) as f32 * unit, unit).fill(rgb(if c == 'o' {
                    0x1a0f0b
                } else {
                    theme::ACCENT
                })),
            );
            x = end;
        }
    }
    root
}

/// The small capitals over a group of controls.
pub fn section_label(s: impl Into<String>) -> El {
    text(s).font(font(10.5, 600.0).tracking(0.3)).color(rgb(theme::TEXT_FAINT))
}

/// Progress bar. `fraction == None` renders the neutral "unavailable" state. The bar springs to a new value.
pub fn meter(key: &str, label: &str, fraction: Option<f32>, caption: String, tone: Option<Color>) -> El {
    let color = tone.unwrap_or_else(|| {
        rgb(match fraction {
            Some(f) if f >= 0.9 => theme::DANGER,
            Some(f) if f >= 0.7 => theme::WARN,
            _ => theme::ACCENT,
        })
    });
    let pct = fraction.map(|f| format!("{:.0}%", (f * 100.0).round())).unwrap_or_else(|| "—".into());
    let bar = el()
        .key(format!("{key}-bar"))
        .h(5.0)
        .w_frac(fraction.unwrap_or(0.0).clamp(0.0, 1.0))
        .radius(3.0)
        .fill(color)
        .motion(motion::REFLOW);
    col()
        .key(key.to_string())
        .gap(4.0)
        .child(
            row()
                .justify_between()
                .items_end()
                .child(section_label(label.to_string()))
                .child(text(pct).font(font(11.5, 500.0).features("'tnum' 1")).color(rgb(if fraction.is_some() {
                    theme::TEXT
                } else {
                    theme::TEXT_FAINT
                }))),
        )
        .child(el().h(5.0).w_full().radius(3.0).fill(rgb(0x1c1c1c)).child(bar))
        .child(text(caption).font(font(10.5, 400.0)).color(rgb(theme::TEXT_FAINT)))
}

struct Look {
    bg: u32,
    fg: u32,
    hover: Color,
    bold: bool,
}

fn button_with(label: String, look: Look, enabled: bool) -> El {
    let mut b = row()
        .h(26.0)
        .px(10.0)
        .center_y()
        .radius(6.0)
        .fill(rgb(look.bg))
        .hover(look.hover)
        .press(Color::BLACK.alpha(0.12))
        .motion(motion::SNAP)
        .child(text(label).font(font(12.0, if look.bold { 600.0 } else { 450.0 })).color(rgb(look.fg)));
    if enabled {
        b = b.cursor(Cursor::Pointer).press_scale(0.97);
    }
    b
}

/// Compact text button. The caller keys it and attaches `.on_press_tx`.
pub fn button(label: impl Into<String>, primary: bool) -> El {
    let look = if primary {
        Look { bg: theme::ACCENT, fg: 0x17100d, hover: Color::WHITE.alpha(0.12), bold: true }
    } else {
        Look { bg: 0x1b1b1b, fg: theme::TEXT, hover: Color::WHITE.alpha(0.05), bold: false }
    };
    button_with(label.into(), look, true).role("button")
}

/// `button` with a Lucide icon before the label.
pub fn button_leading(glyph: &'static Icon, label: impl Into<String>, primary: bool) -> El {
    let (bg, fg, hover) = if primary {
        (theme::ACCENT, 0x17100d, Color::WHITE.alpha(0.12))
    } else {
        (0x1b1b1b, theme::TEXT, Color::WHITE.alpha(0.05))
    };
    row()
        .h(26.0)
        .px(10.0)
        .gap(5.0)
        .center_y()
        .radius(6.0)
        .fill(rgb(bg))
        .hover(hover)
        .press(Color::BLACK.alpha(0.12))
        .cursor(Cursor::Pointer)
        .press_scale(0.97)
        .role("button")
        .child(icon(glyph, 13.0, fg))
        .child(text(label).font(font(12.0, if primary { 600.0 } else { 450.0 })).color(rgb(fg)))
}

/// A button that can't be pressed right now: dimmed, no hover, no handler attached by the caller.
pub fn button_disabled(label: impl Into<String>) -> El {
    button_with(label.into(), Look { bg: 0x1b1b1b, fg: theme::TEXT_FAINT, hover: Color::TRANSPARENT, bold: false }, false)
        .role("button")
        .disabled(true)
}

/// Destructive action (end session, quit).
pub fn danger_button(label: impl Into<String>) -> El {
    button_with(label.into(), Look { bg: 0x5a2327, fg: 0xffd9dc, hover: Color::WHITE.alpha(0.08), bold: true }, true).role("button")
}

/// A Lucide icon, `size` points square, in `color`.
pub fn icon(glyph: &'static Icon, size: f32, color: u32) -> El {
    rune::icon(glyph, size).color(rgb(color))
}

/// The "working" icon: Lucide's loader, turning once a second.
pub fn spinner(size: f32, color: u32) -> El {
    icon(&icons::LOADER_CIRCLE, size, color).looping(rune::LoopSpec::rotation(1.0))
}

/// A square icon button: dim, brightening under the pointer. The caller keys it and attaches `.on_press_tx`.
pub fn icon_button(glyph: &'static Icon) -> El {
    icon_button_base(glyph, theme::TEXT_DIM)
        .hover_group()
        .hover(Color::WHITE.alpha(0.07))
        .press(Color::BLACK.alpha(0.15))
        .cursor(Cursor::Pointer)
        .role("button")
}

/// A dimmed icon button with no hover or click handler.
pub fn icon_button_disabled(glyph: &'static Icon) -> El {
    icon_button_base(glyph, theme::TEXT_FAINT).role("button").disabled(true)
}

fn icon_button_base(glyph: &'static Icon, color: u32) -> El {
    row()
        .size(22.0, 22.0)
        .center()
        .radius(5.0)
        .motion(motion::SNAP)
        .child(icon(glyph, 14.0, color).group_color(rgb(theme::TEXT)))
}

/// A Ctrl shortcut (Cmd on macOS) as the keyboard shows it: `shortcut("Shift+N")` is `Ctrl+Shift+N`, or `⌘⇧N` on macOS.
pub fn shortcut(keys: &str) -> String {
    shortcut_for(cfg!(target_os = "macos"), keys)
}

fn shortcut_for(macos: bool, keys: &str) -> String {
    if macos { format!("⌘{}", keys.replace("Shift+", "⇧")) } else { format!("Ctrl+{keys}") }
}

/// The small key label drawn beside a control while Ctrl (Cmd) is held. It pops in and out.
pub fn key_hint(keys: &str) -> El {
    row()
        
        .px(4.0)
        .radius(4.0)
        .fill(rgb(0x26262a))
        .transition(Transition::scale(0.85).combined(Transition::opacity()))
        .child(text(shortcut(keys)).font(font(10.5, 500.0)).color(rgb(theme::TEXT_DIM)))
}

/// Monogram avatar: the first letter of `seed` on a color picked from it, so each account keeps its color.
pub fn avatar(seed: &str, size: f32) -> El {
    const COLORS: [u32; 6] = [theme::ACCENT, 0x61afef, 0x98c379, 0xc678dd, 0xe5c07b, 0x56b6c2];
    let hash = seed.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let letter = seed.chars().find(|c| c.is_alphanumeric()).map_or('?', |c| c.to_ascii_uppercase());
    row()
        .size(size, size)
        
        .center()
        .radius(size / 2.0)
        .fill(rgb(COLORS[hash as usize % COLORS.len()]))
        .child(text(letter.to_string()).font(font(size * 0.55, 700.0)).color(rgb(0x17100d)))
}

/// The marker in front of menu entries, so the current one shows.
pub fn tick(current: bool) -> &'static str {
    if current { "✓  " } else { "    " }
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
    fn shortcut_names_the_platform_modifier() {
        assert_eq!(shortcut_for(false, "Shift+N"), "Ctrl+Shift+N");
        assert_eq!(shortcut_for(false, ","), "Ctrl+,");
        assert_eq!(shortcut_for(true, "Shift+N"), "⌘⇧N");
        assert_eq!(shortcut_for(true, "3"), "⌘3");
    }

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
        let p = std::path::Path::new("/a/very/long/path/that/keeps/going/and/going/forever/and/ever/project");
        assert!(short_path(p).chars().count() <= 46);
    }
}
