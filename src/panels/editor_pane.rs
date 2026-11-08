//! Editor mode's panel beside the terminal: the shown session's files on the left, the open files' tabs over the
//! editor on the right (state in `app::editor_mode`).

use std::borrow::Cow;

use rune::tok::motion;
use rune::{Color, Cursor, El, GrowthSpec, PointerButton, ScrollSpec, Transition, Tx, col, el, row, text};

use crate::app::editor_mode::{TreeRow, relative, tree_key};
use crate::app::{Conflict, EditorPane, Workspace};
use crate::files::Change;
use crate::panels::icons;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{button, font, icon, icon_button, label};
use crate::platform::path_key;
use crate::store::Id;

const TREE_W: f32 = 240.0;
const TREE_ROW_H: f32 = 24.0;
const TABS_H: f32 = 32.0;

impl Workspace {
    /// The panel, while editor mode shows a session.
    pub fn render_editor_pane(&self) -> Option<El> {
        let (id, pane) = self.shown_pane()?;
        let tree = self.render_tree(id, pane);
        let body = match pane.shown() {
            Some(tab) if tab.note.is_some() => centered(tab.note.clone().unwrap_or_default()),
            Some(tab) => {
                let focused = self.keys_to.as_deref() == Some(tab.editor.key.as_str()) && self.modal.is_none() && !self.search.focused;
                let mut body = col().key(format!("editor-{}", tab.editor.key)).w_full().grow(1.0).shrink().min_h(0.0).transition(Transition::opacity());
                if let Some(c) = tab.conflict {
                    body = body.child(conflict_bar(id, &tab.path, c));
                }
                body.child(el().w_full().grow(1.0).shrink().min_h(0.0).child(tab.editor.render(focused, crate::panels::scale::screen())))
            }
            None => centered(format!("Pick a file on the left, or press {}.", crate::panels::widgets::shortcut("P"))),
        };
        let right = col().key("editor-right").grow(1.0).shrink().min_w(0.0).h_full().child(self.render_tabs(id, pane)).child(body);
        Some(
            row()
                .key("editor-pane")
                .grow(1.0)
                .shrink()
                .min_w(0.0)
                .h_full()
                .radius(theme::PANEL_RADIUS)
                .clip_rounded(theme::PANEL_RADIUS)
                .border(1.0, rgb(theme::PANEL_BORDER))
                .fill(rgb(theme::PANEL_BG))
                .transition(Transition::opacity().combined(Transition::offset(16.0, 0.0)))
                .child(tree)
                .child(right),
        )
    }

    /// The session's folder as a tree: folders open in place, files open in a tab. Only the rows on screen are built.
    fn render_tree(&self, id: &Id, pane: &EditorPane) -> El {
        let shown = pane.shown().map(|t| path_key(&t.path));
        let (key_id, build_id) = (id.clone(), id.clone());
        let rows = col()
            .key(format!("tree-rows-{id}"))
            .w_full()
            .growth(GrowthSpec::default())
            .virtual_rows(
                pane.rows.len(),
                TREE_ROW_H,
                move |ws: &Workspace, i| -> Cow<'_, str> {
                    match ws.panes.get(&key_id).and_then(|p| p.rows.get(i)) {
                        Some(r) => Cow::Owned(format!("tree-{}", path_key(&r.path))),
                        None => Cow::Owned(format!("tree-gone-{i}")),
                    }
                },
                move |ws: &Workspace, i| {
                    let Some(pane) = ws.panes.get(&build_id) else { return el() };
                    let Some(r) = pane.rows.get(i) else { return el() };
                    let shown = pane.shown().is_some_and(|t| t.path == r.path);
                    tree_row(r, shown)
                },
            )
            .virtual_rows_version((pane.rows_version, shown));
        let mut tree = col()
            .key("file-tree")
            .w(TREE_W)
            .h_full()
            .py(6.0)
            .border_sides([0.0, 1.0, 0.0, 0.0], rgb(theme::BORDER))
            .child(col().key(format!("tree-scroll-{id}")).w_full().grow(1.0).shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(rows));
        if pane.truncated {
            tree = tree.child(label("Too many files to list them all here.", 11.0, theme::TEXT_FAINT).px(10.0).py(6.0).wrap());
        }
        tree
    }

    /// One tab per open file: click to show it, × or a middle click to close it. The path is in the tab's tooltip.
    fn render_tabs(&self, id: &Id, pane: &EditorPane) -> El {
        let mut tabs = row().key("tabs-row").h(TABS_H);
        for (i, tab) in pane.tabs.iter().enumerate() {
            let shown = pane.active == Some(i);
            let name = tab.path.file_name().map_or_else(|| tab.path.to_string_lossy(), |n| n.to_string_lossy()).to_string();
            let key = format!("tab-{}", path_key(&tab.path));
            let (pressed, close) = (id.clone(), id.clone());
            // Handlers find their tab by its file: another tab closing moves it.
            let path = tab.path.clone();
            let find = move |ws: &Workspace, id: &Id| ws.panes.get(id).and_then(|p| p.tabs.iter().position(|t| t.path == path));
            let find_close = find.clone();
            let mut t = row()
                .key(key.clone())
                .h_full()
                .pl(12.0)
                .pr(6.0)
                .gap(6.0)
                .center_y()
                .border_sides([0.0, 1.0, 0.0, 0.0], rgb(theme::BORDER))
                .cursor(Cursor::Pointer)
                .role("tab")
                .hover_group()
                .help(relative(&tab.path, &pane.root))
                .motion(motion::REFLOW)
                .transition(Transition::opacity().combined(Transition::offset(0.0, 4.0)))
                .pointer_buttons([PointerButton::Primary, PointerButton::Middle])
                .on_drag(move |ws: &mut Workspace, d: &mut rune::Drag, tx: &mut Tx| {
                    if d.phase != rune::Phase::Up {
                        return;
                    }
                    if let Some(ix) = find(ws, &pressed) {
                        if tx.button() == PointerButton::Middle {
                            ws.request_close_tab(&pressed, ix);
                        } else {
                            ws.show_tab(&pressed, ix);
                        }
                    }
                    ws.after_input(tx);
                })
                .child(label(name, 12.0, if shown { theme::TEXT } else { theme::TEXT_DIM }));
            if shown {
                t = t.fill(rgb(theme::ROW_ACTIVE));
            } else {
                t = t.hover(Color::WHITE.alpha(0.03));
            }
            if tab.dirty {
                t = t.child(el().key(format!("{key}-dirty")).size(6.0, 6.0).radius(3.0).fill(rgb(theme::ACCENT)).transition(Transition::scale(0.4).combined(Transition::opacity())));
            }
            let x = icon_button(&icons::X).key(format!("{key}-close")).help("Close").on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                if let Some(ix) = find_close(ws, &close) {
                    ws.request_close_tab(&close, ix);
                }
                ws.after_input(tx);
            });
            // The shown tab always shows its ×; the others only under the pointer.
            t = t.child(if shown { x } else { x.group_opacity(0.0, 1.0) });
            tabs = tabs.child(t);
        }
        row()
            .key("tabs")
            .w_full()
            .h(TABS_H)
            .border_sides([0.0, 0.0, 1.0, 0.0], rgb(theme::BORDER))
            .child(col().w_full().h(TABS_H).scroll_x().child(tabs))
    }
}

/// A row of the file tree: indented by its depth, a chevron on a folder, a letter on a changed file and a dot on a
/// folder holding changes (as VS Code shows them).
fn tree_row(r: &TreeRow, shown: bool) -> El {
    let color = match r.change {
        Some(Change::Modified) => theme::WARN,
        Some(Change::Added) => theme::OK,
        None => theme::TEXT,
    };
    let glyph = match (r.folder, r.open) {
        (true, true) => &icons::FOLDER_OPEN,
        (true, false) => &icons::FOLDER,
        (false, _) => file_icon(&r.path),
    };
    let indent = 8.0 + 12.0 * r.depth as f32 + if r.folder { 0.0 } else { 17.0 };
    let mut out = row()
        .key(format!("tree-{}", path_key(&r.path)))
        .w_full()
        .h(TREE_ROW_H)
        .pl(indent)
        .pr(10.0)
        .gap(5.0)
        .center_y()
        .cursor(Cursor::Pointer)
        .role(if r.folder { "treeitem" } else { "button" })
        .hover(Color::WHITE.alpha(0.04));
    if shown {
        out = out.fill(rgb(theme::ROW_ACTIVE));
    }
    if r.folder {
        out = out.child(icon(&icons::CHEVRON_RIGHT, 12.0, theme::TEXT_FAINT).rotate(if r.open { 90.0 } else { 0.0 }).motion(motion::SNAP));
    }
    out = out
        .child(icon(glyph, 13.0, color))
        .child(text(r.name.clone()).font(font(crate::panels::scale::px(12.5), 400.0)).color(rgb(color)).truncate().grow(1.0).shrink());
    let badge = r.change.map(|c| match (r.folder, c) {
        (true, _) => "●",
        (false, Change::Modified) => "M",
        (false, Change::Added) => "A",
    });
    if let Some(b) = badge {
        out = out.child(text(b).font(font(crate::panels::scale::px(if r.folder { 8.0 } else { 11.0 }), 500.0)).color(rgb(color)));
    }
    let (path, folder) = (r.path.clone(), r.folder);
    out.on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
        if folder {
            ws.toggle_folder(&path);
        } else if let Some(id) = ws.active_id().cloned() {
            ws.focus_terminal(&tree_key(&id));
            ws.open_file(&id, path.clone(), None);
        }
        ws.after_input(tx);
    })
}

/// Over the editor when the file and the text disagree: reload from disk, or keep the text and save over it later.
fn conflict_bar(id: &Id, path: &std::path::Path, conflict: Conflict) -> El {
    let message = match conflict {
        Conflict::Changed => "This file changed on disk while you had unsaved edits.",
        Conflict::Deleted => "This file was deleted on disk.",
    };
    let mut buttons = row().gap(6.0);
    if conflict == Conflict::Changed {
        let (id, path) = (id.clone(), path.to_path_buf());
        buttons = buttons.child(button("Reload", false).key("conflict-reload").on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            ws.reload_tab(&id, path.clone());
            ws.after_input(tx);
        }));
    }
    let (id, path) = (id.clone(), path.to_path_buf());
    buttons = buttons.child(button("Keep mine", true).key("conflict-keep").on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
        ws.keep_mine(&id, &path);
        ws.after_input(tx);
    }));
    row()
        .key("conflict")
        .w_full()
        .px(12.0)
        .py(6.0)
        .gap(8.0)
        .center_y()
        .justify_between()
        .fill(rgb(0x1d1512))
        .border_sides([0.0, 0.0, 1.0, 0.0], rgb(0x3a2a24))
        .transition(Transition::opacity().combined(Transition::offset(0.0, -6.0)))
        .child(label(message, 12.0, theme::WARN).shrink())
        .child(buttons)
}

/// Source code, prose, or anything else.
fn file_icon(path: &std::path::Path) -> &'static rune::Icon {
    match crate::files::language(path) {
        Some("markdown") => &icons::FILE_TEXT,
        Some(_) => &icons::FILE_CODE,
        None if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt")) => &icons::FILE_TEXT,
        None => &icons::FILE,
    }
}

fn centered(message: String) -> El {
    col().key(format!("editor-note-{message}")).w_full().grow(1.0).center().px(24.0).transition(Transition::opacity()).child(label(message, 12.5, theme::TEXT_FAINT).wrap())
}
