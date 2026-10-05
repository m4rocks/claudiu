//! Colors. Black-first, with Claude's orange as the single accent.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, rgb};

/// One background for the whole app: sidebar, top bar, panes and the terminal's default background.
pub const APP_BG: u32 = 0x0c0c0d;
pub const TERMINAL_BG: u32 = APP_BG;
pub const SIDEBAR_BG: u32 = APP_BG;
pub const BORDER: u32 = 0x1f1f1f;
pub const ROW_HOVER: u32 = 0x181818;
pub const ROW_ACTIVE: u32 = 0x211f1e;
pub const TEXT: u32 = 0xe4e4e4;
pub const TEXT_DIM: u32 = 0x8a8a8a;
pub const TEXT_FAINT: u32 = 0x5a5a5a;
pub const ACCENT: u32 = 0xd97757;
pub const OK: u32 = 0x7fb069;
pub const WARN: u32 = 0xe5c07b;
pub const DANGER: u32 = 0xe06c75;
pub const SELECTION: u32 = 0x3d4a5c;

const FG: Rgb = Rgb { r: 0xe4, g: 0xe4, b: 0xe4 };
const BG: Rgb = Rgb { r: (APP_BG >> 16) as u8, g: (APP_BG >> 8) as u8, b: APP_BG as u8 };

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
