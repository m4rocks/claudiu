//! Markdown in Claudiu's look: GitHub bodies and comments, and the model's cards. Links open in the browser.

use rune_kit::{Markdown, MarkdownStyle};

use crate::app::Workspace;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::font;

/// The look every Markdown in the app shares, on the dark panels.
pub fn style() -> MarkdownStyle {
    MarkdownStyle::neutral(true)
        .text(rgb(theme::TEXT))
        .muted(rgb(theme::TEXT_DIM))
        .link(rgb(theme::ACCENT))
        .link_hover(rgb(0xe58a6b))
        .inline_code_bg(rgb(0x1d1d20))
        .code_block(rgb(0x111113), rgb(theme::BORDER))
        .code_block_text(rgb(0xd4d4d4))
        .rule(rgb(theme::BORDER))
        .quote_rule(rgb(theme::PANEL_BORDER))
        .selection(rgb(theme::SELECTION))
        .body_font(font(13.0, 400.0))
}

/// `src` as Markdown: selectable, with web links opening in the browser. Finish it with `.render(&style())`.
pub fn markdown(src: &str) -> Markdown {
    Markdown::new(src.to_string()).selectable(true).on_link_tx(|ws: &mut Workspace, url: &str, tx: &mut rune::Tx| {
        if url.starts_with("https://") || url.starts_with("http://") {
            let _ = tx.open_url(url);
        }
        ws.after_input(tx);
    })
}
