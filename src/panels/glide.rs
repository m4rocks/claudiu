//! Smooth scrolling of the terminal's scrollback for wheel notches: the wheel moves a target and the view eases toward
//! it a frame at a time. A trackpad already sends small steps, so those are applied as they come and never go through
//! here. Rune's own scroll views (lists, menus, settings) glide by themselves.

use std::time::Instant;

/// Time constant of each of the two easing stages, in seconds (see `Glide`).
const TAU: f32 = 0.03;

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

/// `logical` points moved to a whole device pixel, so a shifted row's background and its text move together.
pub fn snap(logical: f32, scale: f32) -> f32 {
    (logical * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glide_eases_to_its_target_and_stops_there() {
        let mut g = Glide::new(0.0, 10.0);
        let mut frames = 0;
        while g.step(0.01) {
            std::thread::sleep(std::time::Duration::from_millis(4));
            frames += 1;
            assert!(frames < 500, "the glide never settled");
        }
        assert_eq!(g.shown, 10.0);
    }

    #[test]
    fn notches_in_a_row_keep_one_glide_within_its_limits() {
        let mut slot = None;
        Glide::nudge(&mut slot, 0.0, 3.0, 0.0, 5.0);
        Glide::nudge(&mut slot, 0.0, 3.0, 0.0, 5.0);
        assert_eq!(slot.as_ref().map(|g| g.target), Some(5.0));
    }
}
