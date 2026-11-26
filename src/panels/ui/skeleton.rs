use gpui::{Div, ParentElement, Styled, div, px};
use crate::panels::theme::theme;

pub fn skeleton_rows(count: usize) -> Div {
    div().flex().flex_col().gap(px(14.0)).p(px(12.0)).children((0..count).map(|row| {
        div().flex().flex_col().gap(px(6.0))
            .child(div().h(px(10.0)).w(px(112.0 + (row % 3) as f32 * 22.0)).rounded(px(4.0)).bg(theme().muted))
            .child(div().h(px(8.0)).w(px(76.0)).rounded(px(4.0)).bg(theme().muted))
    }))
}

