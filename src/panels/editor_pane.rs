//! Editor mode's right-hand panel: the shown session's files beside an editor (state in `app::editor_mode`).

use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, Window, div, prelude::FluentBuilder, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Editor;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::tree;

use crate::app::Workspace;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::icon;
use crate::platform::path_key;

const TREE_W: f32 = 240.0;

impl Workspace {
    pub fn render_editor_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let root = self.active_id().and_then(|id| self.store.session(id)).map(|s| s.cwd.clone())?;
        let ws = cx.entity().downgrade();
        let (id, pane) = self.shown_pane(window, cx)?;
        let changed = pane.changed.clone();
        let files = tree(&pane.tree, move |_ix, entry, _selected, _w, _cx| {
            let path = std::path::PathBuf::from(entry.item().id.as_ref());
            let color = if changed.contains(&path_key(&path)) { theme::WARN } else if entry.is_folder() { theme::TEXT_DIM } else { theme::TEXT };
            let chevron = entry.is_folder().then(|| icon(if entry.is_expanded() { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0));
            let row = div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .pl(px(8.0 + 12.0 * entry.depth() as f32 + if entry.is_folder() { 0.0 } else { 16.0 }))
                .text_size(crate::panels::scale::px(12.5))
                .text_color(hsla(color))
                .children(chevron)
                .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(entry.item().label.clone()));
            let item = ListItem::new(entry.item().id.clone()).py(px(2.0)).child(row);
            if entry.is_folder() {
                return item;
            }
            let (ws, id) = (ws.clone(), id.clone());
            item.on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| ws.open_file(&id, path.clone(), window, cx));
            })
        });
        let title = match &pane.file {
            Some(f) => f.strip_prefix(&root).unwrap_or(f).to_string_lossy().replace('\\', "/"),
            None => "Choose a file".into(),
        };
        let truncated = pane.truncated.then(|| {
            div()
                .px(px(10.0))
                .py(px(6.0))
                .text_size(crate::panels::scale::px(11.0))
                .text_color(hsla(theme::TEXT_FAINT))
                .child("Too many files to list them all here.")
        });
        let body: AnyElement = match (&pane.note, &pane.file) {
            (Some(note), _) => centered(note.clone()),
            (None, Some(_)) => Editor::new(&pane.buffer)
                // Saving arrives with the "Editing" part of issue #60; until then a change here would be lost.
                .readonly(true)
                .appearance(false)
                .h_full()
                .into_any_element(),
            (None, None) => centered("Pick a file on the left.".into()),
        };
        Some(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .rounded(px(theme::PANEL_RADIUS))
                .overflow_hidden()
                .border_1()
                .border_color(hsla(theme::PANEL_BORDER))
                .bg(hsla(theme::PANEL_BG))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .w(px(TREE_W))
                        .h_full()
                        .py(px(6.0))
                        .border_r_1()
                        .border_color(hsla(theme::BORDER))
                        .child(div().flex_1().min_h_0().child(files))
                        .children(truncated),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(
                            div()
                                .flex_none()
                                .h(px(30.0))
                                .px(px(12.0))
                                .flex()
                                .items_center()
                                .border_b_1()
                                .border_color(hsla(theme::BORDER))
                                .text_size(crate::panels::scale::px(12.0))
                                .text_color(hsla(theme::TEXT_DIM))
                                .when(pane.file.is_some(), |d| d.text_color(hsla(theme::TEXT)))
                                .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title)),
                        )
                        .child(div().flex_1().min_h_0().child(body)),
                )
                .into_any_element(),
        )
    }
}

fn centered(text: String) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .px(px(24.0))
        .text_size(crate::panels::scale::px(12.5))
        .text_color(hsla(theme::TEXT_FAINT))
        .child(text)
        .into_any_element()
}
