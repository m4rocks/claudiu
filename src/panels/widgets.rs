//! Small stateless UI pieces shared by sidebar / main view / modals.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Anchor as Corner, Animation, AnimationElement, AnimationExt, Bounds, Canvas, ClickEvent, Context, Div, ElementId, FontWeight, Hsla, InteractiveElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, SharedString,
    Stateful, StatefulInteractiveElement, Styled, Window, canvas, div, point, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use crate::panels::theme::{self, hsla};

/// The `<m4/>` brand mark on a dark rounded tile, matching the app icon.
pub fn brand_mark(size: f32) -> Div {
    let s = crate::panels::scale::px(size);
    div()
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .size(s)
        .rounded(crate::panels::scale::px(size * 0.22))
        .bg(hsla(theme::PANEL_BG))
        .border_1()
        .border_color(hsla(theme::BORDER))
        .child(crate::panels::logos::draw(&crate::panels::logos::M4CODE, size, hsla(theme::ACCENT)))
}

pub fn section_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(crate::panels::scale::px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hsla(theme::TEXT_FAINT))
        .child(text.into())
}

/// From here up, a meter's bar turns to the warning color.
pub const METER_WARN_AT: f32 = 0.7;

/// Compact usage track, with the used fraction behind the percentage and reset countdown.
pub fn limit_bar(id: impl Into<ElementId>, fraction: Option<f32>, left: String, right: String, tone: Option<Hsla>) -> Stateful<Div> {
    let color = match fraction {
        Some(f) if f >= 0.9 => hsla(theme::DANGER),
        Some(f) if f >= METER_WARN_AT => hsla(theme::WARN),
        _ => tone.unwrap_or_else(|| hsla(theme::ACCENT)),
    };
    let height = crate::panels::scale::px(16.0);
    let radius = crate::panels::scale::px(3.0);
    // Faint enough that the text drawn over it stays readable.
    let fill = div().absolute().top_0().left_0().h(height).w(gpui::relative(fraction.unwrap_or(0.0).clamp(0.0, 1.0))).rounded(radius).bg(color.opacity(0.32));
    let percent_color = if fraction.is_some() { theme::TEXT } else { theme::TEXT_FAINT };
    let texts = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .px(crate::panels::scale::px(4.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(crate::panels::scale::px(3.0))
        .text_size(crate::panels::scale::px(10.5))
        .child(div().flex_none().text_color(hsla(percent_color)).child(left))
        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(hsla(theme::TEXT_DIM)).child(right));
    div().id(id.into()).relative().h(height).w_full().rounded(radius).overflow_hidden().bg(hsla(0x1c1c1c)).child(fill).child(texts)
}

/// Progress bar. `fraction == None` renders the neutral "unavailable" state.
pub fn meter(label: &str, fraction: Option<f32>, caption: String, tone: Option<Hsla>) -> Div {
    let track = hsla(0x1c1c1c);
    let color = tone.unwrap_or_else(|| match fraction {
        Some(f) if f >= 0.9 => hsla(theme::DANGER),
        Some(f) if f >= METER_WARN_AT => hsla(theme::WARN),
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
                        .text_size(crate::panels::scale::px(11.5))
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
                .text_size(crate::panels::scale::px(10.5))
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
        .text_size(crate::panels::scale::px(12.0))
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

pub fn dialog_card(id: impl Into<gpui::ElementId>) -> Stateful<Div> {
    div().id(id).occlude().rounded(px(12.0)).bg(hsla(theme::PANEL_BG)).border_1().border_color(hsla(theme::PANEL_BORDER)).shadow_lg()
}

pub fn dropdown_card(id: impl Into<gpui::ElementId>) -> Stateful<Div> {
    div().id(id).occlude().cursor_default().rounded(px(10.0)).bg(hsla(theme::SURFACE_ELEVATED)).border_1().border_color(hsla(theme::PANEL_BORDER)).shadow_lg()
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
            hsla(theme::PANEL_BG),
            hsla(0xdcdcdc),
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

/// Map a pointer's x position within `[left, left + width]` to the nearest discrete stop in `0..count`.
pub fn step_at_x(x: f32, left: f32, width: f32, count: usize) -> usize {
    if count <= 1 || width <= 0.0 {
        return 0;
    }
    let t = ((x - left) / width).clamp(0.0, 1.0);
    (t * (count - 1) as f32).round() as usize
}

/// Callback invoked when the user clicks or drags the effort slider to a stop index.
pub type EffortPick<V> = Rc<dyn Fn(&mut V, usize, &mut Window, &mut Context<V>)>;

/// Compact draggable effort bar: click or drag horizontally along the track to pick among `labels`, or click any label.
#[allow(clippy::too_many_arguments)]
pub fn effort_slider<V: 'static>(
    id: impl Into<SharedString>,
    labels: &[String],
    selected: Option<usize>,
    accent: Hsla,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    dragging: Rc<Cell<bool>>,
    on_pick: EffortPick<V>,
    cx: &mut Context<V>,
) -> Div {
    let id = id.into();
    let count = labels.len();
    let frac = match selected {
        Some(ix) if count > 1 => (ix as f32 / (count - 1) as f32).clamp(0.0, 1.0),
        Some(_) => 1.0,
        None => 0.0,
    };

    let measure_bounds = bounds.clone();
    let down_bounds = bounds.clone();
    let move_bounds = bounds;
    let down_drag = dragging.clone();
    let move_drag = dragging.clone();
    let up_drag = dragging;
    let down_pick = on_pick.clone();
    let move_pick = on_pick.clone();

    let mut stops = div().absolute().inset_0().flex().items_center().justify_between();
    for i in 0..count {
        let active = selected.is_some_and(|s| i <= s);
        stops = stops.child(
            div()
                .size(px(6.0))
                .rounded_full()
                .bg(if active { accent } else { hsla(0x323644) }),
        );
    }

    let track = div()
        .id(SharedString::from(format!("{id}-track")))
        .relative()
        .w_full()
        .h(px(22.0))
        .flex()
        .items_center()
        .cursor_pointer()
        .child(
            canvas(
                move |b, _, _| measure_bounds.set(b),
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .child(
            div()
                .relative()
                .w_full()
                .h(px(4.0))
                .rounded_full()
                .bg(hsla(0x252834))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(gpui::relative(frac))
                        .rounded_full()
                        .bg(accent),
                ),
        )
        .child(stops)
        .children(selected.map(|_| {
            div()
                .absolute()
                .left(gpui::relative(frac))
                .ml(px(-6.0))
                .size(px(12.0))
                .rounded_full()
                .bg(hsla(theme::TEXT))
                .border_2()
                .border_color(accent)
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |v, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                down_drag.set(true);
                let b = down_bounds.get();
                let ix = step_at_x(f32::from(ev.position.x), f32::from(b.origin.x), f32::from(b.size.width), count);
                down_pick(v, ix, window, cx);
            }),
        )
        .on_mouse_move(cx.listener(move |v, ev: &MouseMoveEvent, window, cx| {
            if !move_drag.get() {
                return;
            }
            if ev.pressed_button != Some(MouseButton::Left) {
                move_drag.set(false);
                return;
            }
            let b = move_bounds.get();
            let ix = step_at_x(f32::from(ev.position.x), f32::from(b.origin.x), f32::from(b.size.width), count);
            move_pick(v, ix, window, cx);
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |_, _: &MouseUpEvent, _, _| up_drag.set(false)),
        );

    let mut label_row = div().flex().items_center().justify_between().w_full();
    for (i, label) in labels.iter().enumerate() {
        let is_sel = selected == Some(i);
        let pick = on_pick.clone();
        label_row = label_row.child(
            div()
                .id(SharedString::from(format!("{id}-lbl-{i}")))
                .cursor_pointer()
                .text_size(crate::panels::scale::px(10.5))
                .font_weight(if is_sel { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .text_color(if is_sel { accent } else { hsla(theme::TEXT_FAINT) })
                .hover(|s| s.text_color(hsla(theme::TEXT)))
                .child(label.clone())
                .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| pick(v, i, window, cx))),
        );
    }

    div().flex().flex_col().gap(px(2.0)).w_full().child(track).child(label_row)
}

/// Where a subagent stands, as one Lucide circle. It does not spin while the subagent works, since an animation would
/// redraw the whole window for as long as the sidebar shows it.
pub fn subagent_state_icon(state: crate::app::SubagentState, size: f32) -> Icon {
    use crate::app::SubagentState::*;
    let (name, color) = match state {
        Starting | Running => (IconName::CircleDotDashed, 0xc8c8c8),
        WaitingForUser => (IconName::CircleQuestionMark, theme::WARN),
        Completed => (IconName::CircleCheck, theme::TEXT_DIM),
        Failed => (IconName::CircleX, theme::DANGER),
        Stopped => (IconName::CircleSlash, theme::TEXT_FAINT),
    };
    icon(name, size).text_color(hsla(color))
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
    icon_button_base(id, glyph)
        .text_color(hsla(theme::TEXT_DIM))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(0x262626)).text_color(hsla(theme::TEXT)))
}

/// A dimmed icon button with no hover or click handler.
pub fn icon_button_disabled(id: impl Into<SharedString>, glyph: IconName) -> Stateful<Div> {
    icon_button_base(id, glyph).text_color(hsla(theme::TEXT_FAINT)).cursor_default()
}

fn icon_button_base(id: impl Into<SharedString>, glyph: IconName) -> Stateful<Div> {
    div()
        .id(id.into())
        .size(px(22.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .child(icon(glyph, 14.0))
}

/// A Ctrl shortcut (Cmd on macOS) as the keyboard shows it: `shortcut("Shift+N")` is `Ctrl+Shift+N`, or `⌘⇧N` on macOS.
pub fn shortcut(keys: &str) -> String {
    shortcut_for(cfg!(target_os = "macos"), keys)
}

fn shortcut_for(macos: bool, keys: &str) -> String {
    if macos { format!("⌘{}", keys.replace("Shift+", "⇧")) } else { format!("Ctrl+{keys}") }
}

/// A small keycap pill drawn as-is (e.g. "Esc", "1-9").
pub fn key_badge(label: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px(px(4.0))
        .rounded(px(4.0))
        .bg(hsla(0x26262a))
        .text_size(crate::panels::scale::px(10.5))
        .text_color(hsla(theme::TEXT_DIM))
        .whitespace_nowrap()
        .child(label.into())
}

/// The small key label drawn beside a control while Ctrl (Cmd) is held, automatically prepending ⌘ / Ctrl+.
pub fn key_hint(keys: &str) -> Div {
    key_badge(shortcut(keys))
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
        .text_size(crate::panels::scale::px(size * 0.55))
        .font_weight(FontWeight::BOLD)
        .text_color(hsla(0x17100d))
        .child(letter.to_string())
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

/// Eases a value (a panel's open fraction, 0 hidden to 1 shown, or its width) toward its target over `Reveal::MS`.
pub struct Reveal {
    from: f32,
    to: f32,
    start: std::time::Instant,
}

impl Reveal {
    pub const MS: f32 = 180.0;

    pub fn new(shown: bool) -> Self {
        Self::at_value(shown as u8 as f32)
    }

    pub fn at_value(v: f32) -> Self {
        Self { from: v, to: v, start: std::time::Instant::now() - std::time::Duration::from_secs(1) }
    }

    fn at(&self) -> f32 {
        let t = (self.start.elapsed().as_secs_f32() * 1000.0 / Self::MS).min(1.0);
        self.from + (self.to - self.from) * (1.0 - (1.0 - t).powi(3))
    }

    /// The open fraction now, for a panel that should be `shown`; keeps frames coming while it moves.
    pub fn step(&mut self, shown: bool, window: &mut gpui::Window) -> f32 {
        self.to(shown as u8 as f32, window)
    }

    /// Jump to `v` with no easing (while the user drags the edge).
    pub fn snap(&mut self, v: f32) {
        *self = Self { from: v, to: v, start: self.start };
    }

    /// The value now, easing toward `target`.
    pub fn to(&mut self, target: f32, window: &mut gpui::Window) -> f32 {
        if target != self.to {
            self.from = self.at();
            self.to = target;
            self.start = std::time::Instant::now();
        }
        if self.start.elapsed().as_secs_f32() * 1000.0 < Self::MS {
            window.request_animation_frame();
        }
        self.at()
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
        let p = std::path::Path::new(
            "/a/very/long/path/that/keeps/going/and/going/forever/and/ever/project",
        );
        assert!(short_path(p).chars().count() <= 46);
    }

    #[test]
    fn effort_slider_maps_pointer_x_to_nearest_stop() {
        assert_eq!(step_at_x(0.0, 100.0, 200.0, 5), 0);
        assert_eq!(step_at_x(100.0, 100.0, 200.0, 5), 0);
        assert_eq!(step_at_x(150.0, 100.0, 200.0, 5), 1);
        assert_eq!(step_at_x(200.0, 100.0, 200.0, 5), 2);
        assert_eq!(step_at_x(300.0, 100.0, 200.0, 5), 4);
        assert_eq!(step_at_x(999.0, 100.0, 200.0, 5), 4);
    }
}

/// Where a dropdown opens relative to its trigger, at least as wide as it.
#[derive(Clone, Copy, Debug)]
pub struct Dropdown {
    pub pos: Point<Pixels>,
    pub min_w: Pixels,
    pub anchor: Corner,
    /// Trigger button's window bounds (empty when opened programmatically without a painted trigger).
    pub trigger: Bounds<Pixels>,
    /// Interactive bounds that toggle this dropdown closed when clicked again (defaults to `trigger`; rows with a
    /// smaller anchored pill can expand this to the full clickable row).
    pub hit: Bounds<Pixels>,
}

impl Dropdown {
    /// Fallback placement for programmatic / keyboard opens when no trigger bounds are available.
    pub fn at(pos: Point<Pixels>) -> Self {
        Self { pos, min_w: px(0.0), anchor: Corner::TopLeft, trigger: Bounds::default(), hit: Bounds::default() }
    }

    pub fn with_hit(mut self, hit: Bounds<Pixels>) -> Self {
        self.hit = hit;
        self
    }
}

/// Remembers a trigger's bounds as it paints, so its menu or picker can stem directly from it.
#[derive(Clone, Default)]
pub struct Anchor(Rc<Cell<Bounds<Pixels>>>);

impl Anchor {
    pub fn probe(&self) -> Canvas<()> {
        let cell = self.0.clone();
        canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {}).absolute().top_0().left_0().size_full()
    }

    pub fn bounds(&self) -> Bounds<Pixels> {
        self.0.get()
    }

    pub fn below(&self) -> Dropdown {
        let b = self.0.get();
        Dropdown { pos: b.bottom_left() + point(px(0.0), px(4.0)), min_w: b.size.width, anchor: Corner::TopLeft, trigger: b, hit: b }
    }

    pub fn below_right(&self) -> Dropdown {
        let b = self.0.get();
        Dropdown { pos: b.bottom_right() + point(px(0.0), px(4.0)), min_w: b.size.width, anchor: Corner::TopRight, trigger: b, hit: b }
    }

    pub fn above(&self) -> Dropdown {
        let b = self.0.get();
        Dropdown { pos: b.origin - point(px(0.0), px(4.0)), min_w: b.size.width, anchor: Corner::BottomLeft, trigger: b, hit: b }
    }
}

/// Any element that opens a menu or picker anchored directly to itself on mouse-down.
pub trait Trigger: InteractiveElement + ParentElement + Styled + Sized {
    /// Open directly below the trigger, left-aligned.
    fn dropdown<V: 'static>(self, cx: &Context<V>, open: impl Fn(&mut V, Dropdown, &mut Window, &mut Context<V>) + 'static) -> Self {
        let anchor = Anchor::default();
        self.relative().child(anchor.probe()).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |v, _: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                open(v, anchor.below(), window, cx);
            }),
        )
    }

    /// Open directly below the trigger, right-aligned (for controls near the right edge).
    fn dropdown_right<V: 'static>(self, cx: &Context<V>, open: impl Fn(&mut V, Dropdown, &mut Window, &mut Context<V>) + 'static) -> Self {
        let anchor = Anchor::default();
        self.relative().child(anchor.probe()).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |v, _: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                open(v, anchor.below_right(), window, cx);
            }),
        )
    }

    /// Open directly above the trigger, left-aligned (for bottom-docked triggers like the composer bar).
    fn dropdown_above<V: 'static>(self, cx: &Context<V>, open: impl Fn(&mut V, Dropdown, &mut Window, &mut Context<V>) + 'static) -> Self {
        let anchor = Anchor::default();
        self.relative().child(anchor.probe()).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |v, _: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                open(v, anchor.above(), window, cx);
            }),
        )
    }
}

impl<T: InteractiveElement + ParentElement + Styled> Trigger for T {}
