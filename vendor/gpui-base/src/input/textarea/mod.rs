use gpui::{App, Context, Entity, IntoElement, RenderOnce, Window};

use super::{InputBaseState, TextDecoration, TextareaMode};

/// State for editing ordinary multi-line text.
///
/// This is the shared editing engine in its multi-line kind. Code-editor
/// facilities such as languages, diagnostics, folding, and LSP do not exist on
/// this type — those methods live on [`super::EditorState`].
pub type TextareaState = InputBaseState<TextareaMode>;

impl TextareaState {
    /// Replace the inline text decorations rendered over this textarea's content.
    pub fn set_decorations(
        &mut self,
        mut decorations: Vec<TextDecoration>,
        cx: &mut Context<Self>,
    ) {
        let len = self.text.len();
        decorations.retain(|d| d.range.start < d.range.end && d.range.end <= len);
        decorations.sort_by_key(|d| (d.range.start, d.range.end));
        if self.extras.decorations != decorations {
            self.extras.decorations = decorations;
            cx.notify();
        }
    }
}

/// An unstyled ordinary multi-line text input.
#[derive(IntoElement)]
pub struct Textarea {
    presentation: super::InlineTokenPresentation,
    state: Entity<TextareaState>,
}

impl Textarea {
    pub fn new(state: &Entity<TextareaState>) -> Self {
        Self {
            state: state.clone(),
            presentation: Default::default(),
        }
    }
    /// The element each atomic token renders as; the input keeps editing and history.
    pub fn token<R: IntoElement>(
        mut self,
        render: impl Fn(&super::InlineTokenContext, &mut Window, &mut App) -> R + 'static,
    ) -> Self {
        self.presentation = self.presentation.token(render);
        self
    }
    /// Open a reference after a completed, unconsumed token click.
    pub fn on_token_click(
        mut self,
        listener: impl Fn(&super::InlineTokenClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.presentation = self.presentation.on_token_click(listener);
        self
    }
    /// Report pointer presence over a token; hover never selects or edits.
    pub fn on_token_hover(
        mut self,
        listener: impl Fn(&super::InlineTokenHoverEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.presentation = self.presentation.on_token_hover(listener);
        self
    }
}

impl RenderOnce for Textarea {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        self.state.update(cx, |state, _| {
            state.set_token_presentation(self.presentation)
        });
        self.state
    }
}
