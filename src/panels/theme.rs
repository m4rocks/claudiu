//! Colors. Minimal monochrome dark theme with white accent.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, rgb};

/// The window background the panels float on, a shade lighter than the panels themselves.
pub const APP_BG: u32 = 0x181818;
/// One background for the panels and the terminal's default background.
pub const PANEL_BG: u32 = 0x121212;
pub const TERMINAL_BG: u32 = PANEL_BG;
pub const SIDEBAR_BG: u32 = PANEL_BG;
/// Elevated dark surfaces (composer box, cards, popovers).
pub const SURFACE_ELEVATED: u32 = 0x1a1a1a;
pub const SURFACE_HOVER: u32 = 0x222222;
pub const BORDER: u32 = 0x242424;
/// The faint outline that separates a panel from the window background.
pub const PANEL_BORDER: u32 = 0x282828;
/// The outline of what has the keyboard or is open, such as the message box or a chip's dropdown. Neutral, so the
/// accent stays for the few things that need the eye.
pub const FOCUS_BORDER: u32 = 0x444444;
/// Corner radius of the window's panels.
pub const PANEL_RADIUS: f32 = 8.0;
/// Gutter between the panels and the window edges.
pub const PANEL_GAP: f32 = 6.0;
/// Height of the window's top bar. macOS traffic lights are centered in it (see `main.rs`).
pub const TOPBAR_HEIGHT: f32 = 34.0;
pub const ROW_HOVER: u32 = 0x1c1c1c;
pub const ROW_ACTIVE: u32 = 0x242424;
pub const TEXT: u32 = 0xf0f0f0;
pub const TEXT_DIM: u32 = 0x8e8e8e;
pub const TEXT_FAINT: u32 = 0x5e5e5e;
pub const ACCENT: u32 = 0xffffff;
pub const OK: u32 = 0x7fb069;
pub const WARN: u32 = 0xe5c07b;
pub const DANGER: u32 = 0xe06c75;
pub const SELECTION: u32 = 0x383838;
/// A plain button, and under the pointer.
pub const CONTROL: u32 = 0x1b1b1b;
pub const CONTROL_HOVER: u32 = 0x262626;
/// The dimmed window behind a dialog, as RGBA.
pub const BACKDROP: u32 = 0x000000b0;
/// The grip between two panels, and under the pointer.
pub const RESIZE_HANDLE: u32 = 0x2b2b30;
pub const RESIZE_HANDLE_HOVER: u32 = 0x4a4a52;
/// A warm band that asks for attention, such as a file changed on disk.
pub const NOTICE_BG: u32 = 0x1d1512;
pub const NOTICE_BORDER: u32 = 0x3a2a24;
/// The warm tint of the current or own item in a list, such as your own review.
pub const CURRENT_TINT: u32 = 0x2a211d;
/// A placeholder bar while content loads.
pub const SKELETON: u32 = 0x1f1f21;
/// The icon of something still working.
pub const BUSY: u32 = 0xc8c8c8;
/// Backgrounds of added and removed lines in a diff.
pub const DIFF_ADDED_BG: u32 = 0x16301e;
pub const DIFF_REMOVED_BG: u32 = 0x3a1a1d;
/// Codex's usage bars, so they read apart from Claude's at a glance. The terminal palette's blue.
pub const CODEX: u32 = 0x61afef;
/// Antigravity's usage bars, apart from Claude's and Codex's. The terminal palette's magenta.
pub const ANTIGRAVITY: u32 = 0xc678dd;

const FG: Rgb = Rgb { r: (TEXT >> 16) as u8, g: (TEXT >> 8) as u8, b: TEXT as u8 };
const BG: Rgb = Rgb { r: (TERMINAL_BG >> 16) as u8, g: (TERMINAL_BG >> 8) as u8, b: TERMINAL_BG as u8 };

const ANSI: [u32; 16] = [
    0x3b3b3b, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xc8ccd4, //
    0x5c6370, 0xff7b86, 0xa9d98a, 0xf0d08c, 0x74bdff, 0xd68aee, 0x67c7d4, 0xffffff,
];

pub fn hsla(hex: u32) -> Hsla {
    rgb(hex).into()
}

pub fn rgb_to_hsla(c: Rgb) -> Hsla {
    hsla(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32)
}

fn ansi(i: usize) -> Rgb {
    let v = ANSI[i];
    Rgb { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
}

fn dim(c: Rgb) -> Rgb {
    let f = |v: u8| (v as f32 * 0.66) as u8;
    Rgb { r: f(c.r), g: f(c.g), b: f(c.b) }
}

fn indexed(i: u8) -> Rgb {
    match i {
        0..=15 => ansi(i as usize),
        16..=231 => {
            let i = i - 16;
            let level = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
            Rgb { r: level(i / 36), g: level((i / 6) % 6), b: level(i % 6) }
        }
        232..=255 => {
            let v = 8 + 10 * (i - 232);
            Rgb { r: v, g: v, b: v }
        }
    }
}

/// Palette lookup by alacritty color index (0-255, then the named specials 256+). Used for OSC 10/11 queries too.
pub fn palette(index: usize, colors: &Colors) -> Rgb {
    if let Some(c) = colors[index] {
        return c;
    }
    match index {
        0..=255 => indexed(index as u8),
        256 | 267 => FG,
        257 => BG,
        258 => FG,
        259..=266 => dim(ansi(index - 259)),
        268 => dim(FG),
        _ => FG,
    }
}

pub fn resolve(color: Color, colors: &Colors) -> Rgb {
    match color {
        Color::Spec(c) => c,
        Color::Indexed(i) => palette(i as usize, colors),
        Color::Named(n) => palette(n as usize, colors),
    }
}

pub fn is_default_bg(color: &Color) -> bool {
    matches!(color, Color::Named(NamedColor::Background))
}

pub fn dimmed(c: Rgb) -> Rgb {
    dim(c)
}

pub fn default_bg() -> Rgb {
    BG
}
