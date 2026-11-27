//! A titled version of the universal notice. Add
//! actions as more children; they sit at the right.

use gpui::{Div, FontWeight, ParentElement, SharedString, Styled, div, px};

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

/// A titled notice; the shared notice component owns all surface styling.
pub fn alert(tone: Tone, title: Option<SharedString>, description: impl Into<SharedString>) -> Div {
    let description = description.into();
    let body = div().flex().flex_col().gap(px(2.0))
        .children(title.map(|title| div().font_weight(FontWeight::SEMIBOLD).child(title)))
        .child(description);
    super::notice::content(body, tone)
}

/// The same notice fitted to the edge of a panel.
pub fn banner(tone: Tone, description: impl Into<SharedString>) -> Div {
    alert(tone, None, description).rounded(px(0.0)).border_0().border_b_1()
}
