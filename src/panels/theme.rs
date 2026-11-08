//! Colors. Black-first, with Claude's orange as the single accent.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};
use rune::Color;

/// The window background the panels float on, a shade lighter than the panels themselves.
pub const APP_BG: u32 = 0x131316;
/// One background for the panels and the terminal's default background.
pub const PANEL_BG: u32 = 0x0c0c0d;
pub const TERMINAL_BG: u32 = PANEL_BG;
pub const SIDEBAR_BG: u32 = PANEL_BG;
pub const BORDER: u32 = 0x1f1f1f;
/// The faint outline that separates a panel from the window background.
pub const PANEL_BORDER: u32 = 0x242428;
/// Corner radius of the window's panels.
pub const PANEL_RADIUS: f32 = 8.0;
/// Gutter between the panels and the window edges.
pub const PANEL_GAP: f32 = 6.0;
/// Height of the window's top bar. macOS traffic lights are centered in it (see `main.rs`).
pub const TOPBAR_HEIGHT: f32 = 34.0;
pub const ROW_ACTIVE: u32 = 0x211f1e;
pub const TEXT: u32 = 0xe4e4e4;
pub const TEXT_DIM: u32 = 0x8a8a8a;
pub const TEXT_FAINT: u32 = 0x5a5a5a;
pub const ACCENT: u32 = 0xd97757;
pub const OK: u32 = 0x7fb069;
pub const WARN: u32 = 0xe5c07b;
pub const DANGER: u32 = 0xe06c75;
pub const SELECTION: u32 = 0x3d4a5c;
/// Search matches in the editor, faint, and the current one stronger.
pub const FOUND: u32 = 0xe5c07b;

const FG: Rgb = Rgb { r: 0xe4, g: 0xe4, b: 0xe4 };
const BG: Rgb = Rgb { r: (TERMINAL_BG >> 16) as u8, g: (TERMINAL_BG >> 8) as u8, b: TERMINAL_BG as u8 };

const ANSI: [u32; 16] = [
    0x3b3b3b, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xc8ccd4, //
    0x5c6370, 0xff7b86, 0xa9d98a, 0xf0d08c, 0x74bdff, 0xd68aee, 0x67c7d4, 0xffffff,
];

/// An sRGB colour written as `0xrrggbb`, in Rune's linear colour.
pub fn rgb(hex: u32) -> Color {
    let channel = |shift: u32| rune::color::srgb_decode(((hex >> shift) & 0xff) as f32 / 255.0);
    Color { r: channel(16), g: channel(8), b: channel(0), a: 1.0 }
}


pub fn term_color(c: Rgb) -> Color {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32)
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

pub fn resolve(color: AnsiColor, colors: &Colors) -> Rgb {
    match color {
        AnsiColor::Spec(c) => c,
        AnsiColor::Indexed(i) => palette(i as usize, colors),
        AnsiColor::Named(n) => palette(n as usize, colors),
    }
}

pub fn is_default_bg(color: &AnsiColor) -> bool {
    matches!(color, AnsiColor::Named(NamedColor::Background))
}

pub fn dimmed(c: Rgb) -> Rgb {
    dim(c)
}

