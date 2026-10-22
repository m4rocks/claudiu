//! Smooth scrolling for wheel notches: the wheel moves a target and the view eases toward it a frame at a time.
//! A trackpad already sends small pixel steps, so those are applied as they come and never go through here.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui::{InteractiveElement, ScrollHandle, ScrollWheelEvent, StatefulInteractiveElement, Styled, Window, point, px};

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

struct State {
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
        Self(Rc::new(RefCell::new(State { handle: ScrollHandle::new(), glide: None, rest: 0.0 })))
    }
}

impl Smooth {
    pub fn handle(&self) -> ScrollHandle {
        self.0.borrow().handle.clone()
    }

    /// Stop a glide in progress (the view is being moved some other way).
    pub fn cancel(&self) {
        self.0.borrow_mut().glide = None;
    }

    /// Make `el` a vertically scrolling area whose wheel notches ease. Call it while building the view, every frame.
    pub fn wire<E: InteractiveElement + StatefulInteractiveElement + Styled>(&self, el: E, window: &mut Window) -> E {
        self.step(window);
        let (me, view) = (self.clone(), window.current_view());
        el.overflow_y_scroll().track_scroll(&self.handle()).on_scroll_wheel(move |ev, window, cx| {
            if me.wheel(ev, window) {
                cx.notify(view);
            }
        })
    }

    /// Ease the offset toward the wheel's target, one frame at a time.
    fn step(&self, window: &mut Window) {
        let mut st = self.0.borrow_mut();
        let Some(g) = &mut st.glide else {
            st.rest = f32::from(st.handle.offset().y);
            return;
        };
        let moving = g.step(0.5);
        let shown = g.shown;
        st.handle.set_offset(point(st.handle.offset().x, px(shown)));
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
        let max = f32::from(st.handle.max_offset().y);
        let by = f32::from(ev.delta.pixel_delta(window.line_height()).y) * NOTCH;
        let rest = st.rest;
        Glide::nudge(&mut st.glide, rest, by, -max, 0.0);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
