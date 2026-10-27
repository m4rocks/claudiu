//! Editor mode's right-hand panel: the shown session's files beside an editor (state in `app::editor_mode`).

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    div, prelude::FluentBuilder, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, Input, RopeExt};
use crate::git::lines::{Change as LineChange, Kind};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::tree::tree;

use crate::app::picker::{Picker, PickerKind, relative};
use crate::app::editing::Conflict;
use crate::files::Change;
use crate::app::{Act, Ask, AskExplain, AskFix, AskFixProblem, AskTests, EditorCommand, EditorPane, MenuItem, GoBack, GoForward, GoToLine, QuickOpen, SaveFile, Tab, Workspace};
use crate::panels::glide::Follow;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, icon};
use crate::platform::path_key;

const TREE_W: f32 = 240.0;

impl Workspace {
    pub fn render_editor_pane(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ws = cx.entity().downgrade();
        let menu_ws = ws.clone();
        let server = self.active_id().cloned().and_then(|id| self.server_status(&id));
        let (id, pane) = self.shown_pane(cx)?;
        let tabs = render_tabs(&id, pane, window, cx);
        let changed = pane.changed.clone();
        let empty_folders = pane.empty_folders.clone();
        let tree_id = id.clone();
        let files = tree(&pane.tree, move |_ix, entry, _selected, _w, _cx| {
            let path = std::path::PathBuf::from(entry.item().id.as_ref());
            let folder = entry.is_folder() || empty_folders.contains(entry.item().id.as_ref());
            let change = changed.get(&path_key(&path)).copied();
            let color = match change {
                Some(Change::Modified) => theme::WARN,
                Some(Change::Added) => theme::OK,
                None => theme::TEXT,
            };
            let chevron = folder.then(|| icon(if entry.is_expanded() { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0));
            let glyph = match (folder, entry.is_expanded()) {
                (true, true) => IconName::FolderOpen,
                (true, false) => IconName::Folder,
                (false, _) => file_icon(&path),
            };
            // Like VS Code: a letter on a changed file, a dot on a folder holding changes.
            let badge = change.map(|c| match (folder, c) {
                (true, _) => "●",
                (false, Change::Modified) => "M",
                (false, Change::Added) => "A",
            });
            let row = div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .pl(px(8.0 + 12.0 * entry.depth() as f32 + if folder { 0.0 } else { 17.0 }))
                .pr(px(10.0))
                .text_size(crate::panels::scale::px(12.5))
                .text_color(hsla(color))
                .children(chevron)
                .child(icon(glyph, 13.0).text_color(hsla(color)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(entry.item().label.clone()))
                .children(badge.map(|b| div().flex_none().text_size(crate::panels::scale::px(if folder { 8.0 } else { 11.0 })).child(b)));
            let (menu_ws, menu_id, menu_path) = (ws.clone(), tree_id.clone(), path.clone());
            let item = ListItem::new(entry.item().id.clone()).py(px(2.0)).child(row).on_mouse_down(MouseButton::Right, move |ev, _w, cx| {
                cx.stop_propagation(); // the tree's own menu is for the session folder
                let _ = menu_ws.update(cx, |ws, cx| {
                    let items = ws.file_menu(&menu_id, Some(&menu_path), folder);
                    ws.open_menu(ev.position, items, cx);
                });
            });
            if folder {
                return item;
            }
            let (ws, id) = (ws.clone(), tree_id.clone());
            item.on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| ws.open_file(&id, path.clone(), None, window, cx));
            })
        });
        let truncated = pane.truncated.then(|| {
            div()
                .px(px(10.0))
                .py(px(6.0))
                .text_size(crate::panels::scale::px(11.0))
                .text_color(hsla(theme::TEXT_FAINT))
                .child("Too many files to list them all here.")
        });
        let body: AnyElement = match pane.shown() {
            Some(Tab { note: Some(note), .. }) => centered(note.clone()),
            Some(tab) => div()
                .size_full()
                .flex()
                .flex_col()
                .children(tab.conflict.map(|c| render_conflict(&id, &tab.path, c, cx)))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(Editor::new(&tab.buffer).appearance(false).h_full().context_menu(move |menu, window, cx| {
                            // The menu is Claudiu's own: the system's menu keeps the editor's text cursor over it.
                            let at = window.mouse_position();
                            let _ = menu_ws.update(cx, |ws, cx| ws.open_menu(at, editor_menu(), cx));
                            menu
                        }))
                        .child(change_marks(tab.buffer.clone(), tab.marks.clone(), tab.follow.clone())),
                )
                .children(server.map(|s| {
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(3.0))
                        .border_t_1()
                        .border_color(hsla(theme::BORDER))
                        .text_size(crate::panels::scale::px(11.0))
                        .text_color(hsla(theme::TEXT_FAINT))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(s))
                }))
                .into_any_element(),
            None => centered("Pick a file on the left, or press Ctrl+P.".into()),
        };
        let picker = pane.picker.as_ref().map(|p| render_picker(p, &pane.root, cx));
        Some(
            div()
                .id("editor-pane")
                .key_context("EditorPane")
                .track_focus(&pane.focus)
                .on_action(cx.listener(|ws, _: &QuickOpen, window, cx| ws.open_picker(PickerKind::Files, window, cx)))
                .on_action(cx.listener(|ws, _: &SaveFile, _w, cx| ws.save_shown(cx)))
                .on_action(cx.listener(|ws, _: &GoToLine, window, cx| ws.ask_go_to_line(window, cx)))
                .on_action(cx.listener(|ws, _: &GoBack, window, cx| ws.go_back(window, cx)))
                .on_action(cx.listener(|ws, _: &GoForward, window, cx| ws.go_forward(window, cx)))
                .on_action(cx.listener(|ws, _: &AskExplain, window, cx| ws.ask_claude(Ask::Explain, window, cx)))
                .on_action(cx.listener(|ws, _: &AskFix, window, cx| ws.ask_claude(Ask::Fix, window, cx)))
                .on_action(cx.listener(|ws, _: &AskTests, window, cx| ws.ask_claude(Ask::Tests, window, cx)))
                .on_action(cx.listener(|ws, _: &AskFixProblem, window, cx| ws.ask_claude(Ask::Problem, window, cx)))
                .relative()
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
                        // Rows don't take the keyboard themselves; the pane does, so its shortcuts work from here.
                        .on_mouse_down(MouseButton::Left, cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                            if let Some(pane) = ws.active_id().and_then(|id| ws.panes.get(id)) {
                                window.focus(&pane.focus, cx);
                            }
                        }))
                        // Right-click below the rows (or on a row, which opens its own menu instead).
                        .on_mouse_down(MouseButton::Right, cx.listener(|ws, ev: &MouseDownEvent, _w, cx| {
                            if let Some(id) = ws.active_id().cloned() {
                                let items = ws.file_menu(&id, None, true);
                                ws.open_menu(ev.position, items, cx);
                            }
                        }))
                        .child(pane.tree_scroll.watch(div().id("file-tree").flex_1().min_h_0(), window).child(files))
                        .children(truncated),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(tabs)
                        .child(div().flex_1().min_h_0().child(body)),
                )
                .children(picker)
                .into_any_element(),
        )
    }
}

/// One tab per open file: click to show it, × or a middle click to close it. The full path is in the tab's tooltip.
/// The editor's right-click menu: the usual editing items, then requests to Claude about the selection (or the clicked
/// line) and about the language server's problem there. The same for every click: the editor builds it while it is
/// itself being updated, so it can't be read for a selection or a problem here; each request checks when it runs.
fn editor_menu() -> Vec<MenuItem> {
    let entry = |label: &str, command| MenuItem::new(label, Act::Editor(command));
    let mut items = vec![
        entry("Go to Definition", EditorCommand::GoToDefinition),
        entry("Cut", EditorCommand::Cut),
        entry("Copy", EditorCommand::Copy),
        entry("Paste", EditorCommand::Paste),
        entry("Select All", EditorCommand::SelectAll),
        entry("Ask Claude to explain this", EditorCommand::Ask(Ask::Explain)),
        entry("Ask Claude to fix this", EditorCommand::Ask(Ask::Fix)),
        entry("Ask Claude to write tests for this", EditorCommand::Ask(Ask::Tests)),
        entry("Ask Claude to fix the problem on this line", EditorCommand::Ask(Ask::Problem)),
    ];
    for ix in [1, 5] {
        items[ix].separator_before = true;
    }
    items
}

/// The text scrolls through the editor's own offset, which the editor clamps itself. The wheel is taken before the editor
/// sees it and eased here, one frame at a time; the canvas below hosts it because it is painted every frame, over the editor.
fn glide_text(buffer: &gpui::Entity<EditorState>, follow: &std::cell::RefCell<Follow>, window: &mut gpui::Window, cx: &mut gpui::App) {
    let here = f32::from(buffer.read(cx).scroll_offset().y);
    if let Some((y, more)) = follow.borrow_mut().frame(here, window.scale_factor()) {
        buffer.update(cx, |editor, cx| editor.set_scroll_offset(gpui::point(editor.scroll_offset().x, px(y)), cx));
        if more {
            window.request_animation_frame();
        }
    }
}

/// A bar at the editor's left edge, over the line numbers, for each line that differs from the last commit. The editor
/// has no gutter of its own to draw in, so this paints over it and asks the editor where each line is, which
/// follows scrolling, soft wrapping and folds. Changes are as of the last save (`Tab::marks`).
fn change_marks(buffer: gpui::Entity<EditorState>, marks: std::sync::Arc<Vec<LineChange>>, follow: std::rc::Rc<std::cell::RefCell<Follow>>) -> impl IntoElement {
    const BAR_WIDTH: f32 = 3.0;
    gpui::canvas(
        |_, _, _| (),
        move |viewport, _, window, cx| {
            glide_text(&buffer, &follow, window, cx);
            let (wheel_buffer, wheel_follow, view) = (buffer.clone(), follow.clone(), window.current_view());
            window.on_mouse_event(move |ev: &gpui::ScrollWheelEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Capture || !viewport.contains(&ev.position) || ev.modifiers.modified() {
                    return;
                }
                if ev.delta.precise() {
                    wheel_follow.borrow_mut().cancel();
                    return;
                }
                let here = f32::from(wheel_buffer.read(cx).scroll_offset().y);
                let by = f32::from(ev.delta.pixel_delta(window.line_height()).y) * crate::panels::glide::NOTCH;
                wheel_follow.borrow_mut().wheel(here, by);
                cx.stop_propagation();
                // Only this view: a refresh would redraw every cached one too.
                cx.notify(view);
            });
            let editor = buffer.read(cx);
            let (Some(area), Some(visible)) = (editor.text_bounds(), editor.visible_row_range()) else { return };
            let text = editor.text();
            let lines = text.lines_len();
            // Clip to what is on screen: the editor's text area is the whole scrolled content, which moves away from the view.
            window.with_content_mask(Some(gpui::ContentMask { bounds: viewport }), |window| {
                for mark in marks.iter() {
                    let color = match mark.kind {
                        Kind::Added => hsla(theme::OK),
                        Kind::Modified => hsla(0x61afef),
                        Kind::Deleted => hsla(theme::DANGER),
                    };
                    if mark.kind == Kind::Deleted {
                        // A removal has no line of its own: a short bar on the border between the two lines it was between.
                        let at = mark.lines.start.min(lines.saturating_sub(1));
                        if at < visible.start || at > visible.end {
                            continue;
                        }
                        if let Some(top) = editor.range_to_bounds(&(text.line_start_offset(at)..text.line_start_offset(at))) {
                            window.paint_quad(gpui::fill(gpui::Bounds::new(gpui::point(area.origin.x, top.origin.y - px(1.0)), gpui::size(px(BAR_WIDTH * 2.0), px(2.0))), color));
                        }
                        continue;
                    }
                    // Lines above the screen would be placed at its top edge, and none lies below the rows on screen.
                    for line in (mark.lines.start.max(visible.start)..mark.lines.end).take_while(|&l| l <= visible.end && l < lines) {
                        let (start, end) = (text.line_start_offset(line), text.line_end_offset(line));
                        if let Some(bounds) = editor.range_to_bounds(&(start..end)) {
                            window.paint_quad(gpui::fill(gpui::Bounds::new(gpui::point(area.origin.x, bounds.origin.y), gpui::size(px(BAR_WIDTH), bounds.size.height)), color));
                        }
                    }
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn render_tabs(id: &crate::store::Id, pane: &EditorPane, window: &mut gpui::Window, cx: &mut Context<Workspace>) -> AnyElement {
    let tabs = pane.tabs.iter().enumerate().map(|(i, tab)| {
        let shown = pane.active == Some(i);
        let name = tab.path.file_name().map_or_else(|| tab.path.to_string_lossy(), |n| n.to_string_lossy()).to_string();
        let rel = relative(&tab.path, &pane.root);
        let (show, close, middle) = (id.clone(), id.clone(), id.clone());
        div()
            .id(SharedString::from(format!("tab-{i}")))
            .group(SharedString::from(format!("tab-{i}")))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .h_full()
            .pl(px(12.0))
            .pr(px(6.0))
            .border_r_1()
            .border_color(hsla(theme::BORDER))
            .text_size(crate::panels::scale::px(12.0))
            .text_color(hsla(if shown { theme::TEXT } else { theme::TEXT_DIM }))
            .when(shown, |d| d.bg(hsla(theme::ROW_ACTIVE)))
            .cursor_pointer()
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(rel.clone()).build(window, cx))
            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.show_tab(&show, i, window, cx)))
            .on_mouse_up(MouseButton::Middle, cx.listener(move |ws, _: &gpui::MouseUpEvent, window, cx| ws.request_close_tab(&middle, i, window, cx)))
            .child(name)
            .when(tab.dirty, |d| d.child(div().text_color(hsla(theme::ACCENT)).child("●")))
            .child(
                div()
                    .id(SharedString::from(format!("tab-close-{i}")))
                    .rounded(px(3.0))
                    .hover(|s| s.bg(hsla(0x2a2a2a)))
                    .invisible()
                    .when(shown, |d| d.visible())
                    .group_hover(SharedString::from(format!("tab-{i}")), |s| s.visible())
                    .child(icon(IconName::X, 12.0))
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        ws.request_close_tab(&close, i, window, cx);
                    })),
            )
    });
    pane.tabs_scroll
        .wire(div().id("tabs"), window)
        .flex_none()
        .h(px(30.0))
        .flex()
        .border_b_1()
        .border_color(hsla(theme::BORDER))
        .children(tabs)
        .into_any_element()
}

/// Above the editor when the file and the buffer disagree: reload from disk, or keep the buffer and save over it later.
fn render_conflict(id: &crate::store::Id, path: &std::path::Path, conflict: Conflict, cx: &mut Context<Workspace>) -> AnyElement {
    let text = match conflict {
        Conflict::Changed => "This file changed on disk while you had unsaved edits.",
        Conflict::Deleted => "This file was deleted on disk.",
    };
    let (reload_id, reload_path, keep_id, keep_path) = (id.clone(), path.to_path_buf(), id.clone(), path.to_path_buf());
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(6.0))
        .bg(hsla(0x1d1512))
        .border_b_1()
        .border_color(hsla(0x3a2a24))
        .text_size(crate::panels::scale::px(12.0))
        .text_color(hsla(theme::WARN))
        .child(text)
        .child(
            div()
                .flex()
                .gap(px(6.0))
                .when(conflict == Conflict::Changed, |d| {
                    d.child(button("conflict-reload", "Reload", false).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        ws.reload_tab(&reload_id, reload_path.clone(), window, cx)
                    })))
                })
                .child(button("conflict-keep", "Keep mine", true).on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| ws.keep_mine(&keep_id, &keep_path, cx)))),
        )
        .into_any_element()
}

/// The quick open / find in files box: a field over a list, Up/Down to move, Enter to open, Escape to close.
fn render_picker(p: &Picker, root: &std::path::Path, cx: &mut Context<Workspace>) -> AnyElement {
    let start = p.selected.saturating_sub(VISIBLE_ROWS - 1);
    let rows = p.hits.iter().enumerate().skip(start).take(VISIBLE_ROWS).map(|(i, hit)| {
        let (label, detail) = match p.kind {
            PickerKind::Files => (hit.text.clone(), None),
            PickerKind::Text => (format!("{}:{}", relative(&hit.path, root), hit.line + 1), Some(hit.text.clone())),
        };
        div()
            .id(SharedString::from(format!("picked-{i}")))
            .flex()
            .flex_col()
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(5.0))
            .cursor_pointer()
            .when(i == p.selected, |d| d.bg(hsla(theme::ROW_ACTIVE)))
            .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
            .child(div().text_size(crate::panels::scale::px(12.5)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
            .children(detail.map(|d| {
                div().text_size(crate::panels::scale::px(11.5)).text_color(hsla(theme::TEXT_DIM)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(d)
            }))
            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                ws.pick(i, cx);
                ws.choose_picked(window, cx);
            }))
    });
    let list = div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .capture_key_down(cx.listener(|ws, ev: &KeyDownEvent, _w, cx| {
            match ev.keystroke.key.as_str() {
                "up" => ws.move_picked(-1, cx),
                "down" => ws.move_picked(1, cx),
                _ => return,
            }
            cx.stop_propagation();
        }))
        .child(Input::new(&p.input))
        .child(div().flex().flex_col().children(rows));
    floating("picker", list, |ws, window, cx| ws.close_picker(window, cx), cx)
}

/// A box floating near the top of the editor pane (quick open, find in files); Escape closes it.
fn floating(
    id: &'static str,
    content: impl IntoElement,
    close: impl Fn(&mut Workspace, &mut gpui::Window, &mut Context<Workspace>) + 'static,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    div()
        .absolute()
        .top(px(36.0))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .id(id)
                .occlude()
                .w(px(560.0))
                .max_w_full()
                .p(px(6.0))
                .rounded(px(8.0))
                .bg(hsla(0x181818))
                .border_1()
                .border_color(hsla(0x2c2c2c))
                .shadow_lg()
                .capture_key_down(cx.listener(move |ws, ev: &KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "escape" {
                        close(ws, window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(content),
        )
        .into_any_element()
}

const VISIBLE_ROWS: usize = 12;

/// Source code, prose, or anything else.
fn file_icon(path: &std::path::Path) -> IconName {
    match crate::files::language(path) {
        Some("markdown") => IconName::FileText,
        Some(_) => IconName::FileCode,
        None if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt")) => IconName::FileText,
        None => IconName::File,
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, Entity, TestAppContext, Window, point};

    struct Pane {
        buffer: Entity<EditorState>,
        follow: std::rc::Rc<std::cell::RefCell<Follow>>,
    }

    impl gpui::Render for Pane {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .relative()
                .child(Editor::new(&self.buffer).h_full())
                .child(change_marks(self.buffer.clone(), Default::default(), self.follow.clone()))
        }
    }

    /// Where the editor drew its text this frame, as an offset from the top of its view. The editor's own offset is
    /// clamped after drawing, so it can't show a frame drawn past the end.
    fn drawn(pane: &Entity<Pane>, cx: &mut TestAppContext) -> f32 {
        pane.read_with(cx, |pane, app| {
            let editor = pane.buffer.read(app);
            f32::from(editor.text_bounds().unwrap().origin.y - editor.input_bounds().origin.y)
        })
    }

    #[test]
    fn a_wheel_glide_into_either_end_never_draws_past_it() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (window, pane) = cx
            .update(|app| {
                gpui_kit::open_window(Default::default(), app, |window, app| {
                    let buffer = app.new(|cx| EditorState::new(window, cx));
                    let text: String = (0..300).map(|i| format!("line {i}\n")).collect();
                    buffer.update(app, |b, cx| b.set_value(text, window, cx));
                    app.new(|_| Pane { buffer, follow: Default::default() })
                })
            })
            .unwrap();
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        };
        let scroll_to = |y: f32, cx: &mut TestAppContext| {
            pane.update(cx, |pane, cx| pane.buffer.update(cx, |b, cx| b.set_scroll_offset(point(px(0.0), px(y)), cx)));
            draw(cx);
            draw(cx);
        };
        // Turns the wheel `notches` times (negative scrolls down) and returns every offset drawn while it glides.
        let glide = |notches: f32, cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, app| {
                let at = point(px(200.0), px(200.0));
                window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position: at, pressed_button: None, modifiers: Default::default() }), app);
                let wheel = gpui::ScrollWheelEvent { position: at, delta: gpui::ScrollDelta::Lines(point(0.0, notches)), modifiers: Default::default(), touch_phase: gpui::TouchPhase::Moved };
                window.dispatch_event(gpui::PlatformInput::ScrollWheel(wheel), app);
            })
            .unwrap();
            let mut seen = Vec::new();
            for _ in 0..60 {
                std::thread::sleep(std::time::Duration::from_millis(8));
                draw(cx);
                seen.push(drawn(&pane, cx));
            }
            seen
        };
        draw(&mut cx);
        scroll_to(-1.0e6, &mut cx);
        let bottom = drawn(&pane, &mut cx);
        assert!(bottom < -1000.0, "the editor never scrolled: {bottom}");

        scroll_to(bottom + 60.0, &mut cx);
        let seen = glide(-20.0, &mut cx);
        assert!(seen.iter().all(|&y| y >= bottom), "drew past the bottom at {bottom}: {seen:?}");
        assert_eq!(seen.last(), Some(&bottom), "{seen:?}");

        scroll_to(-60.0, &mut cx);
        let seen = glide(20.0, &mut cx);
        assert!(seen.iter().all(|&y| y <= 0.0), "drew past the top: {seen:?}");
        assert_eq!(seen.last(), Some(&0.0), "{seen:?}");
    }
}
