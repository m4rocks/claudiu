//! A 1px rule after shadcn's Separator.

use gpui::{Div, Styled, div, px};

use super::token;

/// A rule across the width of its parent.
pub fn horizontal() -> Div {
    div().flex_none().h(px(1.0)).w_full().bg(token::separator())
}

/// A rule down the height of its parent.
pub fn vertical() -> Div {
    div().flex_none().w(px(1.0)).h_full().bg(token::separator())
}
