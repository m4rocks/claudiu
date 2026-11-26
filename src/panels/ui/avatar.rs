use gpui::{Div, FontWeight, ParentElement, Styled, div, px};
use crate::panels::theme::theme;

/// Monogram avatar: the first letter of `seed` on a color picked from it, so each account keeps its color.
pub fn avatar(seed: &str, size: f32) -> Div {
    let t = theme();
    let colors = [t.primary, t.ansi[4], t.ansi[2], t.ansi[5], t.ansi[3], t.ansi[6]];
    let hash = seed.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let letter = seed.chars().find(|c| c.is_alphanumeric()).map_or('?', |c| c.to_ascii_uppercase());
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(colors[hash as usize % colors.len()])
        .text_size(crate::panels::scale::px(size * 0.55))
        .font_weight(FontWeight::BOLD)
        .text_color(theme().primary_foreground)
        .child(letter.to_string())
}

