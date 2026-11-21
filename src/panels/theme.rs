//! Colors. Deep neutral-slate surfaces with per-harness accents.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, rgb};

use crate::harness::Harness;

/// The window background the panels float on, slightly deeper than the panels themselves.
pub const APP_BG: u32 = 0x0b0c0f;
/// Primary surface for the main chat pane and editor.
pub const PANEL_BG: u32 = 0x111216;
pub const TERMINAL_BG: u32 = PANEL_BG;
pub const SIDEBAR_BG: u32 = 0x0e0f13;
/// Elevated surfaces: composer dock, welcome launch card, popovers, pills.
pub const SURFACE_ELEVATED: u32 = 0x171920;
pub const SURFACE_HOVER: u32 = 0x1e212b;
pub const BORDER: u32 = 0x1d1f27;
/// The crisp hairline outline around panels and cards.
pub const PANEL_BORDER: u32 = 0x242731;
/// Corner radius of the window's panels.
pub const PANEL_RADIUS: f32 = 10.0;
/// Gutter between the panels and the window edges.
pub const PANEL_GAP: f32 = 6.0;
/// Height of the window's top bar. macOS traffic lights are centered in it (see `main.rs`).
pub const TOPBAR_HEIGHT: f32 = 36.0;
pub const ROW_HOVER: u32 = 0x191b22;
pub const ROW_ACTIVE: u32 = 0x20232d;
pub const TEXT: u32 = 0xe8eaef;
pub const TEXT_DIM: u32 = 0x9499a8;
pub const TEXT_FAINT: u32 = 0x5c6170;
pub const ACCENT: u32 = 0xd97757;
pub const OK: u32 = 0x7fb069;
pub const WARN: u32 = 0xe5c07b;
pub const DANGER: u32 = 0xe06c75;
pub const SELECTION: u32 = 0x3d4a5c;
/// Codex's accent and usage bars.
pub const CODEX: u32 = 0x61afef;
/// OpenCode's accent.
pub const OPENCODE: u32 = 0x5ec29b;
/// Antigravity's accent and usage bars.
pub const ANTIGRAVITY: u32 = 0xc678dd;

/// The signature color of a harness across badges, switches, and usage meters.
pub fn harness_color(harness: Harness) -> u32 {
    match harness {
        Harness::Claude => ACCENT,
        Harness::Codex => CODEX,
        Harness::OpenCode => OPENCODE,
        Harness::Antigravity => ANTIGRAVITY,
    }
}

const FG: Rgb = Rgb { r: 0xe8, g: 0xea, b: 0xef };
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
