//! Colors. One [`Theme`] object holds every color m4code draws with, named after shadcn/ui's semantic tokens
//! (`background`, `card`, `popover`, `primary`, `secondary`, `muted`, `accent`, `destructive`, `border`, `input`, `ring`)
//! plus the roles the app adds (panel outlines, diff lines, GitHub states, harness colors, the terminal palette).
//!
//! Read colors through [`theme()`]: `.bg(theme().card)`, `.text_color(theme().muted_foreground)`. Nothing outside this
//! module builds a color from a literal (`source_tests::no_color_literals_outside_the_theme` enforces it). A light or
//! user theme is another `Theme` value returned from [`theme()`]; only the dark one exists today.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, Rgba};

/// Every color the app draws with. Field docs say where each one is used.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    // Surfaces.
    /// The window background the panels float on, a shade lighter than the panels themselves.
    pub window: Hsla,
    /// The panels and the terminal: the surface most content sits on.
    pub background: Hsla,
    /// The sidebar and the controls that sit flat on it. The same as `background` today, kept apart so it can change.
    pub sidebar: Hsla,
    /// A surface sunk below `background`: code blocks, settings' side nav, section bands, card bodies in the GitHub pane.
    pub inset: Hsla,
    /// Elevated surfaces: the composer box, cards, chips.
    pub card: Hsla,
    /// The header strip of a card, such as a file's name above its diff, or a list item that is not current.
    pub card_header: Hsla,
    /// Floating surfaces: menus, pickers, dropdowns, tooltips.
    pub popover: Hsla,
    /// The dimmed window behind a dialog, the review overlay or the command palette.
    pub backdrop: Hsla,

    // Text.
    /// Body text.
    pub foreground: Hsla,
    /// Secondary text: labels, descriptions, metadata.
    pub muted_foreground: Hsla,
    /// Tertiary text: hints, placeholders, section titles, idle icons.
    pub subtle_foreground: Hsla,

    // Controls.
    /// The main action and the accent of the monochrome theme (white).
    pub primary: Hsla,
    /// Text and icons on `primary`.
    pub primary_foreground: Hsla,
    /// `primary` under the pointer.
    pub primary_hover: Hsla,
    /// A plain button.
    pub secondary: Hsla,
    /// A plain button under the pointer; also key caps.
    pub secondary_hover: Hsla,
    /// A quiet fill: badges, placeholder bars while content loads.
    pub muted: Hsla,
    /// A surface under the pointer (shadcn's `accent`).
    pub accent: Hsla,
    /// A list row under the pointer.
    pub row_hover: Hsla,
    /// The chosen or open list row.
    pub row_active: Hsla,
    /// The warm tint of the current or own item in a list, such as your own review.
    pub current: Hsla,
    /// The track of a switch that is off.
    pub input: Hsla,
    /// The knob of a switch that is off.
    pub knob: Hsla,
    /// The rail of a slider.
    pub track: Hsla,
    /// A slider's stops not yet reached.
    pub track_mark: Hsla,

    // Lines.
    /// Separators and control outlines inside a panel.
    pub border: Hsla,
    /// The faint outline that separates a panel from the window background.
    pub panel_border: Hsla,
    /// The outline of what has the keyboard or is open, such as the message box or a chip's dropdown. Neutral, so
    /// `primary` stays for the few things that need the eye.
    pub ring: Hsla,
    /// Selected text.
    pub selection: Hsla,
    /// The grip between two panels.
    pub resize_handle: Hsla,
    /// The grip between two panels under the pointer.
    pub resize_handle_hover: Hsla,

    // Status.
    /// Errors and destructive actions, as text or an icon.
    pub destructive: Hsla,
    /// A solid destructive button (end session, quit, interrupt).
    pub destructive_solid: Hsla,
    /// Text on `destructive_solid`.
    pub destructive_solid_foreground: Hsla,
    /// `destructive_solid` under the pointer.
    pub destructive_solid_hover: Hsla,
    /// A tinted error card or badge.
    pub destructive_subtle: Hsla,
    /// The outline of `destructive_subtle`.
    pub destructive_subtle_border: Hsla,
    /// Success, as text or an icon; also the Resume button's fill.
    pub success: Hsla,
    /// Text on `success`.
    pub success_foreground: Hsla,
    /// `success` under the pointer.
    pub success_hover: Hsla,
    /// A tinted success control, such as Review changes.
    pub success_subtle: Hsla,
    /// Text on `success_subtle`.
    pub success_subtle_foreground: Hsla,
    /// `success_subtle` under the pointer.
    pub success_subtle_hover: Hsla,
    /// Warnings, as text or an icon; also the Steer button's fill.
    pub warning: Hsla,
    /// Text on `warning`.
    pub warning_foreground: Hsla,
    /// `warning` under the pointer.
    pub warning_hover: Hsla,
    /// Information, links and modified files; also the Queue button's fill.
    pub info: Hsla,
    /// Text on `info`.
    pub info_foreground: Hsla,
    /// `info` under the pointer.
    pub info_hover: Hsla,
    /// A warm band that asks for attention, such as a file changed on disk or the Full Access warning.
    pub notice: Hsla,
    /// The outline of `notice`.
    pub notice_border: Hsla,
    /// The icon of something still working.
    pub busy: Hsla,

    // Diffs and GitHub.
    /// The background of an added line in a diff.
    pub diff_added: Hsla,
    /// The background of a removed line in a diff.
    pub diff_removed: Hsla,
    /// GitHub: open, approved, passing, added lines.
    pub state_open: Hsla,
    /// GitHub: merged, auto-merge.
    pub state_merged: Hsla,
    /// GitHub: closed, failing, changes requested, removed lines.
    pub state_closed: Hsla,
    /// GitHub: draft, skipped, neutral.
    pub state_neutral: Hsla,
    /// GitHub: pending, review required, blocked.
    pub state_pending: Hsla,

    // Harnesses.
    /// Claude Code's usage bars, its signature terracotta orange.
    pub claude: Hsla,
    /// Codex's usage bars, so they read apart from Claude's at a glance. The terminal palette's blue.
    pub codex: Hsla,
    /// Antigravity's usage bars, apart from Claude's and Codex's. The terminal palette's magenta.
    pub antigravity: Hsla,

    /// The terminal's 16 ANSI colors; also avatars and the named colors of inline UI.
    pub ansi: [Hsla; 16],
}

/// The dark theme, the only one today.
pub const DARK: Theme = Theme {
    window: hex(APP_BG),
    background: hex(PANEL_BG),
    sidebar: hex(SIDEBAR_BG),
    inset: hex(0x101010),
    card: hex(SURFACE_ELEVATED),
    card_header: hex(0x161616),
    popover: hex(SURFACE_ELEVATED),
    backdrop: hexa(BACKDROP),

    foreground: hex(TEXT),
    muted_foreground: hex(TEXT_DIM),
    subtle_foreground: hex(TEXT_FAINT),

    primary: hex(ACCENT),
    primary_foreground: hex(PANEL_BG),
    primary_hover: hex(0xdcdcdc),
    secondary: hex(CONTROL),
    secondary_hover: hex(CONTROL_HOVER),
    muted: hex(SKELETON),
    accent: hex(SURFACE_HOVER),
    row_hover: hex(ROW_HOVER),
    row_active: hex(ROW_ACTIVE),
    current: hex(CURRENT_TINT),
    input: hex(0x2c2c2c),
    knob: hex(0x9e9e9e),
    track: hex(0x252834),
    track_mark: hex(0x323644),

    border: hex(BORDER),
    panel_border: hex(PANEL_BORDER),
    ring: hex(FOCUS_BORDER),
    selection: hex(SELECTION),
    resize_handle: hex(RESIZE_HANDLE),
    resize_handle_hover: hex(RESIZE_HANDLE_HOVER),

    destructive: hex(DANGER),
    destructive_solid: hex(0x5a2327),
    destructive_solid_foreground: hex(0xffd9dc),
    destructive_solid_hover: hex(0x74292e),
    destructive_subtle: hex(0x251417),
    destructive_subtle_border: hex(0x55242a),
    success: hex(OK),
    success_foreground: hex(0x0e160c),
    success_hover: hex(0x92c27d),
    success_subtle: hex(0x1b2b20),
    success_subtle_foreground: hex(0xd9f2df),
    success_subtle_hover: hex(0x233829),
    warning: hex(WARN),
    warning_foreground: hex(0x16120b),
    warning_hover: hex(0xedd092),
    info: hex(ANSI[4]),
    info_foreground: hex(0x0c141c),
    info_hover: hex(0x7cc0f5),
    notice: hex(NOTICE_BG),
    notice_border: hex(NOTICE_BORDER),
    busy: hex(BUSY),

    diff_added: hex(DIFF_ADDED_BG),
    diff_removed: hex(DIFF_REMOVED_BG),
    state_open: hex(0x3fb950),
    state_merged: hex(0xa371f7),
    state_closed: hex(0xf85149),
    state_neutral: hex(0x8b949e),
    state_pending: hex(0xd29922),

    claude: hex(CLAUDE),
    codex: hex(CODEX),
    antigravity: hex(ANTIGRAVITY),

    ansi: {
        let mut out = [hex(0); 16];
        let mut i = 0;
        while i < 16 {
            out[i] = hex(ANSI[i]);
            i += 1;
        }
        out
    },
};

/// The active theme.
pub fn theme() -> &'static Theme {
    &DARK
}

/// Switches gpui-kit to its dark mode and hands it the theme, so its text inputs, editor and lists match the app.
/// Use this instead of calling `gpui_kit::component::Theme::change` directly.
pub fn install(cx: &mut gpui::App) {
    use gpui_kit::component::{Theme as KitTheme, ThemeMode};
    KitTheme::change(ThemeMode::Dark, None, cx);
    let t = theme();
    let c = &mut KitTheme::global_mut(cx).colors;
    c.background = t.background;
    c.foreground = t.foreground;
    c.border = t.border;
    c.input = t.border;
    c.ring = t.ring;
    c.caret = t.foreground;
    c.selection = t.primary.opacity(0.24);
    c.popover = t.popover;
    c.popover_foreground = t.foreground;
    c.muted = t.muted;
    c.muted_foreground = t.muted_foreground;
    c.accent = t.accent;
    c.accent_foreground = t.foreground;
    c.list_hover = t.row_hover;
    c.list_active = t.row_active;
    c.primary = t.primary;
    c.primary_foreground = t.primary_foreground;
    c.primary_hover = t.primary_hover;
    c.secondary = t.secondary;
    c.secondary_foreground = t.foreground;
    c.secondary_hover = t.secondary_hover;
    c.danger = t.destructive;
    c.success = t.success;
    c.warning = t.warning;
    c.info = t.info;
    c.skeleton = t.muted;
}

/// `0xRRGGBB` as a color, computed at compile time exactly as `gpui::rgb(..).into()` does.
pub const fn hex(c: u32) -> Hsla {
    hexa((c << 8) | 0xff)
}

/// `0xRRGGBBAA` as a color.
pub const fn hexa(c: u32) -> Hsla {
    let r = ((c >> 24) & 0xff) as f32 / 255.0;
    let g = ((c >> 16) & 0xff) as f32 / 255.0;
    let b = ((c >> 8) & 0xff) as f32 / 255.0;
    let a = (c & 0xff) as f32 / 255.0;
    let max = r.max(g.max(b));
    let min = r.min(g.min(b));
    let delta = max - min;
    let l = (max + min) / 2.0;
    let s = if l == 0.0 || l == 1.0 {
        0.0
    } else if l < 0.5 {
        delta / (2.0 * l)
    } else {
        delta / (2.0 - 2.0 * l)
    };
    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        let x = (g - b) / delta;
        (if x < 0.0 { x + 6.0 } else { x }) / 6.0
    } else if max == g {
        ((b - r) / delta + 2.0) / 6.0
    } else {
        ((r - g) / delta + 4.0) / 6.0
    };
    Hsla { h, s, l, a }
}

/// A color back as `0xRRGGBB`, for the code that speaks hex (inline UI's CSS).
pub fn to_hex(c: Hsla) -> u32 {
    let Rgba { r, g, b, .. } = c.into();
    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u32;
    (byte(r) << 16) | (byte(g) << 8) | byte(b)
}

/// Legacy names for the dark palette's raw values, which [`DARK`] is built from. New code reads [`theme()`]; these go
/// once nothing outside this module uses them.
pub const APP_BG: u32 = 0x181818;
pub const PANEL_BG: u32 = 0x121212;
pub const TERMINAL_BG: u32 = PANEL_BG;
pub const SIDEBAR_BG: u32 = PANEL_BG;
pub const SURFACE_ELEVATED: u32 = 0x1a1a1a;
pub const SURFACE_HOVER: u32 = 0x222222;
pub const BORDER: u32 = 0x242424;
pub const PANEL_BORDER: u32 = 0x282828;
pub const FOCUS_BORDER: u32 = 0x444444;
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
pub const CONTROL: u32 = 0x1b1b1b;
pub const CONTROL_HOVER: u32 = 0x262626;
pub const BACKDROP: u32 = 0x000000b0;
pub const RESIZE_HANDLE: u32 = 0x2b2b30;
pub const RESIZE_HANDLE_HOVER: u32 = 0x4a4a52;
pub const NOTICE_BG: u32 = 0x1d1512;
pub const NOTICE_BORDER: u32 = 0x3a2a24;
pub const CURRENT_TINT: u32 = 0x2a211d;
pub const SKELETON: u32 = 0x1f1f21;
pub const BUSY: u32 = 0xc8c8c8;
pub const DIFF_ADDED_BG: u32 = 0x16301e;
pub const DIFF_REMOVED_BG: u32 = 0x3a1a1d;
pub const CLAUDE: u32 = 0xd97757;
pub const CODEX: u32 = 0x61afef;
pub const ANTIGRAVITY: u32 = 0xc678dd;

/// Corner radius of the window's panels.
pub const PANEL_RADIUS: f32 = 8.0;
/// Gutter between the panels and the window edges.
pub const PANEL_GAP: f32 = 6.0;
/// Height of the window's top bar. macOS traffic lights are centered in it (see `main.rs`).
pub const TOPBAR_HEIGHT: f32 = 34.0;

const FG: Rgb = Rgb { r: (TEXT >> 16) as u8, g: (TEXT >> 8) as u8, b: TEXT as u8 };
const BG: Rgb = Rgb { r: (TERMINAL_BG >> 16) as u8, g: (TERMINAL_BG >> 8) as u8, b: TERMINAL_BG as u8 };

const ANSI: [u32; 16] = [
    0x3b3b3b, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xc8ccd4, //
    0x5c6370, 0xff7b86, 0xa9d98a, 0xf0d08c, 0x74bdff, 0xd68aee, 0x67c7d4, 0xffffff,
];

/// Legacy: a raw `0xRRGGBB` as a color. New code reads [`theme()`].
pub fn hsla(c: u32) -> Hsla {
    hex(c)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `hex` runs at compile time; it must give exactly what `gpui::rgb` gives at run time.
    #[test]
    fn hex_matches_gpui() {
        for c in [APP_BG, PANEL_BG, TEXT, ACCENT, OK, WARN, DANGER, CLAUDE, CODEX, ANTIGRAVITY, 0x000000, 0x0c141c, 0x3fb950] {
            assert_eq!(hex(c), Hsla::from(gpui::rgb(c)), "{c:06x}");
            assert_eq!(to_hex(hex(c)), c, "{c:06x}");
        }
        assert_eq!(hexa(BACKDROP), Hsla::from(gpui::rgba(BACKDROP)));
    }
}
