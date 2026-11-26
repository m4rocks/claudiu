//! Helpers the kit (`panels::ui`) builds on and that are not components themselves: the icon, the brand mark, the
//! effort slider, time and path formatting, easing, and anchoring menus to their triggers.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Anchor as Corner, Bounds, Canvas, ClickEvent, Context, Div, FontWeight, Hsla, InteractiveElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, SharedString,
    StatefulInteractiveElement, Styled, Window, canvas, div, point, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use crate::panels::theme::theme;

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
        .bg(theme().background)
        .border_1()
        .border_color(theme().border)
        .child(crate::panels::logos::draw(&crate::panels::logos::M4CODE, size, theme().primary))
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
                .bg(if active { accent } else { theme().track_mark }),
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
                .bg(theme().track)
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
                .bg(theme().foreground)
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
                .text_color(if is_sel { accent } else { theme().subtle_foreground })
                .hover(|s| s.text_color(theme().foreground))
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
        Starting | Running => (IconName::CircleDotDashed, theme().busy),
        WaitingForUser => (IconName::CircleQuestionMark, theme().warning),
        Completed => (IconName::CircleCheck, theme().muted_foreground),
        Failed => (IconName::CircleX, theme().destructive),
        Stopped => (IconName::CircleSlash, theme().subtle_foreground),
    };
    icon(name, size).text_color(color)
}

/// A Ctrl shortcut (Cmd on macOS) as the keyboard shows it: `shortcut("Shift+N")` is `Ctrl+Shift+N`, or `⌘⇧N` on macOS.
pub fn shortcut(keys: &str) -> String {
    shortcut_for(cfg!(target_os = "macos"), keys)
}

fn shortcut_for(macos: bool, keys: &str) -> String {
    if macos { format!("⌘{}", keys.replace("Shift+", "⇧")) } else { format!("Ctrl+{keys}") }
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

/// Any element that opens a menu or picker anchored directly to itself on mouse-down or Enter/Space.
pub trait Trigger: InteractiveElement + ParentElement + Styled + Sized {
    /// Open directly below the trigger, left-aligned.
    fn dropdown<V: 'static>(self, cx: &Context<V>, open: impl Fn(&mut V, Dropdown, &mut Window, &mut Context<V>) + 'static) -> Self {
        let anchor = Anchor::default();
        let open = Rc::new(open);
        let key_anchor = anchor.clone();
        let key_open = open.clone();
        self.relative()
            .child(anchor.probe())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |v, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    open(v, anchor.below(), window, cx);
                }),
            )
            .on_key_down(cx.listener(move |v, ev: &KeyDownEvent, window, cx| {
                let plain = !ev.keystroke.modifiers.control && !ev.keystroke.modifiers.alt && !ev.keystroke.modifiers.platform && !ev.keystroke.modifiers.shift;
                if plain && matches!(ev.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    key_open(v, key_anchor.below(), window, cx);
                }
            }))
    }

    /// Open directly below the trigger, right-aligned (for controls near the right edge).
    fn dropdown_right<V: 'static>(self, cx: &Context<V>, open: impl Fn(&mut V, Dropdown, &mut Window, &mut Context<V>) + 'static) -> Self {
        let anchor = Anchor::default();
        let open = Rc::new(open);
        let key_anchor = anchor.clone();
        let key_open = open.clone();
        self.relative()
            .child(anchor.probe())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |v, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    open(v, anchor.below_right(), window, cx);
                }),
            )
            .on_key_down(cx.listener(move |v, ev: &KeyDownEvent, window, cx| {
                let plain = !ev.keystroke.modifiers.control && !ev.keystroke.modifiers.alt && !ev.keystroke.modifiers.platform && !ev.keystroke.modifiers.shift;
                if plain && matches!(ev.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    key_open(v, key_anchor.below_right(), window, cx);
                }
            }))
    }

    /// Open directly above the trigger, left-aligned (for bottom-docked triggers like the composer bar).
    fn dropdown_above<V: 'static>(self, cx: &Context<V>, open: impl Fn(&mut V, Dropdown, &mut Window, &mut Context<V>) + 'static) -> Self {
        let anchor = Anchor::default();
        let open = Rc::new(open);
        let key_anchor = anchor.clone();
        let key_open = open.clone();
        self.relative()
            .child(anchor.probe())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |v, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    open(v, anchor.above(), window, cx);
                }),
            )
            .on_key_down(cx.listener(move |v, ev: &KeyDownEvent, window, cx| {
                let plain = !ev.keystroke.modifiers.control && !ev.keystroke.modifiers.alt && !ev.keystroke.modifiers.platform && !ev.keystroke.modifiers.shift;
                if plain && matches!(ev.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    key_open(v, key_anchor.above(), window, cx);
                }
            }))
    }
}

impl<T: InteractiveElement + ParentElement + Styled> Trigger for T {}

