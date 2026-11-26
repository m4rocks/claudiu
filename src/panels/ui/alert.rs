//! A callout after shadcn's Alert: an icon, a title and a description in a bordered box tinted by its tone. Add
//! actions as more children; they sit at the right.

use gpui::{Div, FontWeight, Hsla, ParentElement, SharedString, Styled, div, px};
use gpui_kit::assets::IconName;

use crate::panels::scale;
use crate::panels::theme::theme;
use crate::panels::widgets::icon;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    /// Something to know.
    #[default]
    Default,
    /// Something to look at before going on.
    Warning,
    /// Something went wrong.
    Destructive,
}

impl Tone {
    fn colors(self) -> (Hsla, Hsla, Hsla, IconName) {
        let t = theme();
        match self {
            Tone::Default => (t.card, t.border, t.foreground, IconName::Info),
            Tone::Warning => (t.notice, t.notice_border, t.warning, IconName::TriangleAlert),
            Tone::Destructive => (t.destructive_subtle, t.destructive_subtle_border, t.destructive, IconName::CircleAlert),
        }
    }
}

/// The callout. `title` is the bold first line; `description` the muted text under it.
pub fn alert(tone: Tone, title: Option<SharedString>, description: impl Into<SharedString>) -> Div {
    let (bg, border, accent, glyph) = tone.colors();
    let text = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .children(title.map(|t| div().font_weight(FontWeight::MEDIUM).text_color(accent).child(t)))
        .child(div().text_color(if tone == Tone::Default { theme().muted_foreground } else { accent.opacity(0.85) }).child(description.into()));
    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(scale::px(12.5))
        .child(div().flex_none().text_color(accent).child(icon(glyph, 15.0)))
        .child(text)
}

/// [`alert`] as a strip along a panel's edge: square, ruled only below.
pub fn banner(tone: Tone, description: impl Into<SharedString>) -> Div {
    alert(tone, None, description).rounded(px(0.0)).border_0().border_b_1()
}
