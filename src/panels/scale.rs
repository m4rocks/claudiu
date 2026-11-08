//! The user's UI scale (Settings). Rune has no window zoom, so text sizes and the terminal font go through here.
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
pub fn px(v: f32) -> f32 {
    v * percent() as f32 / 100.0
}

/// Device pixels per point of the window, as of its last frame: images rasterize at it.
static SCREEN: AtomicU32 = AtomicU32::new(0x4000_0000); // 2.0

pub fn set_screen(scale: f32) {
    SCREEN.store(scale.to_bits(), Ordering::Relaxed);
}

pub fn screen() -> f32 {
    f32::from_bits(SCREEN.load(Ordering::Relaxed))
}
