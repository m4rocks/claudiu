//! A titled version of the universal notice. Add
//! actions as more children; they sit at the right.

use gpui::{Div, SharedString, Styled, px};

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
    let text = title.map_or_else(|| description.to_string(), |title| format!("{title}\n{description}"));
    super::notice::notice(text, tone)
}

/// The same notice fitted to the edge of a panel.
pub fn banner(tone: Tone, description: impl Into<SharedString>) -> Div {
    alert(tone, None, description).rounded(px(0.0)).border_0().border_b_1()
}
