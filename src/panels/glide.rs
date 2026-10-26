//! Smooth scrolling for wheel notches: the wheel moves a target and the view eases toward it a frame at a time.
//! A trackpad already sends small pixel steps, so those are applied as they come and never go through here.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui::{InteractiveElement, Pixels, ScrollHandle, ScrollWheelEvent, StatefulInteractiveElement, Styled, Window, point, px};

/// Time constant of each of the two easing stages, in seconds (see `Glide`).
const TAU: f32 = 0.03;

/// A mouse wheel notch scrolls this many times what the system reports for it.
pub const NOTCH: f32 = 1.6;

/// The view follows `mid`, which follows `target`: two chained easings, so a move starts gently, speeds up and
/// settles softly instead of leaping at full speed on the first frame.
pub struct Glide {
    pub target: f32,
    pub shown: f32,
    mid: f32,
    at: Instant,
}

impl Glide {
    pub fn new(shown: f32, target: f32) -> Self {
        Self { target, shown, mid: shown, at: Instant::now() }
    }

    /// Move the target of the glide in `slot` by `by` within `lo..=hi`, starting one from `here` if none is running
    /// (a running one keeps its speed, so a burst of notches stays one smooth motion).
    pub fn nudge(slot: &mut Option<Glide>, here: f32, by: f32, lo: f32, hi: f32) {
        let g = slot.get_or_insert_with(|| Glide::new(here, here));
        g.target = (g.target + by).clamp(lo, hi);
    }

    /// Move `shown` toward `target` for the time since the last call; false once it is within `eps` (and has snapped).
    pub fn step(&mut self, eps: f32) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.at).as_secs_f32().min(0.1);
        self.at = now;
        let a = 1.0 - (-dt / TAU).exp();
        self.mid += (self.target - self.mid) * a;
        self.shown += (self.mid - self.shown) * a;
        if (self.target - self.shown).abs() <= eps {
            self.shown = self.target;
            return false;
        }
        true
    }
}

/// The direction an area scrolls in.
#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Vertical,
    Horizontal,
}

/// `logical` pixels moved to a whole device pixel. gpui draws text at whole device pixels in y, so an offset in
/// between moves the rows' backgrounds and their text by different amounts, and the list shimmers.
pub fn snap(logical: f32, scale: f32) -> f32 {
    (logical * scale).round() / scale
}

/// Glides an offset that another component owns and clamps by itself, so the limits are unknown here: the glide
/// ends when the offset read back is not the one last sent.
#[derive(Default)]
pub struct Follow {
    glide: Option<Glide>,
    sent: f32,
}

impl Follow {
    /// A wheel notch of `by` pixels with the view at `here`.
    pub fn wheel(&mut self, here: f32, by: f32) {
        if self.glide.is_none() {
            self.sent = here;
        }
        Glide::nudge(&mut self.glide, here, by, f32::MIN, f32::MAX);
    }

    /// The offset to show for this frame with the view at `here`, `None` when nothing is gliding. `more` is whether
    /// another frame is needed.
    pub fn frame(&mut self, here: f32, scale: f32) -> Option<(f32, bool)> {
        let g = self.glide.as_mut()?;
        if (here - self.sent).abs() > 1.0 / scale {
            // Moved by someone else, or clamped at an end: stop where it is.
            self.glide = None;
            return None;
        }
        let more = g.step(0.5);
        self.sent = snap(g.shown, scale);
        if !more {
            self.glide = None;
        }
        Some((self.sent, more))
    }

    /// A pixel-precise device took over (a trackpad): drop what was gliding.
    pub fn cancel(&mut self) {
        self.glide = None;
    }
}

struct State {
    axis: Axis,
    handle: ScrollHandle,
    glide: Option<Glide>,
    /// Where the view stood before the last wheel event: the scroll element jumps by the whole notch on its own,
    /// so the offset it shows by then is not the starting point.
    rest: f32,
}

/// One vertical scroll area with smooth wheel scrolling. Keep it in the view and wire the scrolling element through it.
#[derive(Clone)]
pub struct Smooth(Rc<RefCell<State>>);

impl Default for Smooth {
    fn default() -> Self {
        Self::new(Axis::Vertical)
    }
}

impl Smooth {
    fn new(axis: Axis) -> Self {
        Self(Rc::new(RefCell::new(State { axis, handle: ScrollHandle::new(), glide: None, rest: 0.0 })))
    }

    /// Glide an area some component scrolls itself: it owns the handle, and this only moves it.
    pub fn around(handle: ScrollHandle) -> Self {
        let me = Self::new(Axis::Vertical);
        me.0.borrow_mut().handle = handle;
        me
    }

    /// Ease the wheel for an element that already scrolls through the handle given to `around`.
    pub fn watch<E: InteractiveElement>(&self, el: E, window: &mut Window) -> E {
        self.step(window);
        let (me, view) = (self.clone(), window.current_view());
        let mut el = el;
        el.interactivity().on_scroll_wheel(move |ev, window, cx| {
            if me.wheel(ev, window) {
                cx.notify(view);
            }
        });
        el
    }

    /// A row that scrolls sideways; a vertical wheel notch moves it, as in gpui's own sideways areas.
    pub fn horizontal() -> Self {
        Self::new(Axis::Horizontal)
    }

    fn offset(&self) -> f32 {
        let st = self.0.borrow();
        let at = st.handle.offset();
        f32::from(if st.axis == Axis::Vertical { at.y } else { at.x })
    }

    pub fn handle(&self) -> ScrollHandle {
        self.0.borrow().handle.clone()
    }

    /// Make `el` a vertically scrolling area whose wheel notches ease. Call it while building the view, every frame.
    pub fn wire<E: InteractiveElement + StatefulInteractiveElement + Styled>(&self, el: E, window: &mut Window) -> E {
        self.step(window);
        let (me, view) = (self.clone(), window.current_view());
        let el = if self.0.borrow().axis == Axis::Vertical { el.overflow_y_scroll() } else { el.overflow_x_scroll() };
        el.track_scroll(&self.handle()).on_scroll_wheel(move |ev, window, cx| {
            if me.wheel(ev, window) {
                cx.notify(view);
            }
        })
    }

    /// Ease the offset toward the wheel's target, one frame at a time.
    fn step(&self, window: &mut Window) {
        let mut st = self.0.borrow_mut();
        let Some(g) = &mut st.glide else {
            drop(st);
            self.0.borrow_mut().rest = self.offset();
            return;
        };
        let moving = g.step(0.5);
        let shown = g.shown;
        let at = px(snap(shown, window.scale_factor()));
        let now = st.handle.offset();
        st.handle.set_offset(if st.axis == Axis::Vertical { point(now.x, at) } else { point(at, now.y) });
        st.rest = shown;
        if moving {
            window.request_animation_frame();
        } else {
            st.glide = None;
        }
    }

    /// A mouse wheel notch moves the target by its distance; a trackpad's small steps are left to the element.
    fn wheel(&self, ev: &ScrollWheelEvent, window: &Window) -> bool {
        let mut st = self.0.borrow_mut();
        if ev.delta.precise() {
            st.glide = None;
            return false;
        }
        let (max, notch) = (st.handle.max_offset(), ev.delta.pixel_delta(window.line_height()));
        let (max, by) = match st.axis {
            Axis::Vertical => (max.y, notch.y),
            Axis::Horizontal => (max.x, if notch.x == Pixels::ZERO { notch.y } else { notch.x }),
        };
        let (max, by) = (f32::from(max), f32::from(by) * NOTCH);
        let rest = st.rest;
        Glide::nudge(&mut st.glide, rest, by, -max, 0.0);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext;

    #[test]
    fn it_approaches_the_target_without_overshooting_and_then_stops() {
        let mut g = Glide::new(0.0, -100.0);
        let mut last = 0.0;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let moving = g.step(0.5);
            assert!(g.shown <= last && g.shown >= -100.0, "{} after {last}", g.shown);
            last = g.shown;
            if !moving {
                assert_eq!(g.shown, -100.0);
                return;
            }
        }
        panic!("never arrived");
    }

    struct Page(Smooth);

    impl gpui::Render for Page {
        fn render(&mut self, window: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            use gpui::{ParentElement, div};
            let list = div().flex().flex_col().children((0..100).map(|i| div().h(px(20.0)).child(format!("row {i}"))));
            div().size_full().child(self.0.wire(div().id("page").size_full(), window).child(list))
        }
    }

    #[test]
    fn a_wheel_notch_scrolls_the_wired_area() {
        let mut cx = gpui::TestAppContext::single();
        let smooth = Smooth::default();
        let window = cx.update(|app| app.open_window(Default::default(), |_, app| app.new(|_| Page(smooth.clone()))).unwrap());
        let draw = |cx: &mut gpui::TestAppContext| {
            cx.update_window(window.into(), |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        };
        draw(&mut cx);
        cx.update_window(window.into(), |_, window, app| {
            let at = point(px(10.0), px(10.0));
            window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position: at, pressed_button: None, modifiers: Default::default() }), app);
            let wheel = ScrollWheelEvent { position: at, delta: gpui::ScrollDelta::Lines(point(0.0, -3.0)), modifiers: Default::default(), touch_phase: gpui::TouchPhase::Moved };
            window.dispatch_event(gpui::PlatformInput::ScrollWheel(wheel), app);
        })
        .unwrap();
        let mut seen = vec![f32::from(smooth.handle().offset().y)];
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            draw(&mut cx);
            seen.push(f32::from(smooth.handle().offset().y));
        }
        let scale = cx.update_window(window.into(), |_, window, _| window.scale_factor()).unwrap();
        assert!(seen.iter().all(|o| (o * scale - (o * scale).round()).abs() < 1e-3), "an offset fell between device pixels: {seen:?} at {scale}x");
        let end = seen[seen.len() - 1];
        // Not the whole way at once; how far the first frame gets depends on how long it took, so no tighter bound.
        assert!(seen[1].abs() < end.abs(), "the first frame jumped to {} of {end}", seen[1]);
        let offset = f32::from(smooth.handle().offset().y);
        assert!(offset < -20.0, "the area never scrolled: offset {offset}, max {:?}", smooth.handle().max_offset());
    }

    #[test]
    fn a_sideways_area_follows_a_vertical_notch() {
        struct Row(Smooth);
        impl gpui::Render for Row {
            fn render(&mut self, window: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
                use gpui::{ParentElement, div};
                let tabs = (0..60).map(|i| div().flex_none().w(px(100.0)).child(format!("tab {i}")));
                div().size_full().child(self.0.wire(div().id("row").flex().size_full(), window).children(tabs))
            }
        }
        let mut cx = gpui::TestAppContext::single();
        let smooth = Smooth::horizontal();
        let window = cx.update(|app| app.open_window(Default::default(), |_, app| app.new(|_| Row(smooth.clone()))).unwrap());
        let draw = |cx: &mut gpui::TestAppContext| {
            cx.update_window(window.into(), |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        };
        draw(&mut cx);
        cx.update_window(window.into(), |_, window, app| {
            let at = point(px(10.0), px(10.0));
            window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position: at, pressed_button: None, modifiers: Default::default() }), app);
            let wheel = ScrollWheelEvent { position: at, delta: gpui::ScrollDelta::Lines(point(0.0, -3.0)), modifiers: Default::default(), touch_phase: gpui::TouchPhase::Moved };
            window.dispatch_event(gpui::PlatformInput::ScrollWheel(wheel), app);
        })
        .unwrap();
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            draw(&mut cx);
        }
        let at = smooth.handle().offset();
        assert!(at.x < px(-20.0) && at.y == px(0.0), "the row did not move sideways: {at:?}");
    }

    #[test]
    fn a_followed_offset_glides_and_stops_where_the_owner_clamped_it() {
        let mut f = Follow::default();
        f.wheel(0.0, -100.0);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let (first, more) = f.frame(0.0, 2.0).unwrap();
        assert!(more && first < 0.0 && first > -100.0, "{first}");
        // The owner could only scroll to -5: it reads back as that, so the glide ends there.
        assert_eq!(f.frame(-5.0, 2.0), None);
        assert_eq!(f.frame(-5.0, 2.0), None, "nothing glides any more");
    }
}
