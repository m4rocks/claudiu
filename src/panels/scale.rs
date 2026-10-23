//! The user's UI scale (Settings). GPUI has no global zoom, so text sizes and the terminal font go through here.
use std::sync::atomic::{AtomicU32, Ordering};

pub const MIN: u32 = 80;
pub const MAX: u32 = 160;
pub const STEP: u32 = 10;

static PERCENT: AtomicU32 = AtomicU32::new(100);

pub fn set(percent: u32) {
    PERCENT.store(percent.clamp(MIN, MAX), Ordering::Relaxed);
}

pub fn percent() -> u32 {
    PERCENT.load(Ordering::Relaxed)
}

/// A font size scaled by the setting.
pub fn px(v: f32) -> gpui::Pixels {
    gpui::px(v * percent() as f32 / 100.0)
}
