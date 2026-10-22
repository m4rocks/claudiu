//! Left sidebar: one list of Claude Code sessions and chats (running first, then recent, then older), a project
//! filter, and the account usage meters.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, AnyElement, AppContext, DragMoveEvent, SharedString, WeakEntity, ease_out_quint, ClickEvent, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Render,
    Stateful, StatefulInteractiveElement, Styled, Window, WindowControlArea, div, prelude::FluentBuilder, px,
};

use gpui_kit::component::input::Input;
use crate::chat::store::ChatRecord;
use crate::app::{Act, Activity, MenuItem, SideDrag, Workspace};
use crate::store::{ExternalState, Id, Project, SessionKind, SessionRecord, now};
use crate::theme::{self, hsla};
use gpui_kit::assets::IconName;
use crate::widgets::{ago, avatar, button, clawd, countdown, icon, icon_button, meter, section_label, spinner, tick};

pub const SIDEBAR_W: f32 = 284.0;
pub const SIDEBAR_MIN: f32 = 220.0;
pub const SIDEBAR_MAX: f32 = 520.0;
/// Entries shown under RECENT before the rest go behind OLDER.
const RECENT_LIMIT: usize = 12;

#[derive(Clone)]
struct Row {
    id: Id,
    glyph: IconName,
    glyph_color: Hsla,
    /// The icon is a spinner: the Claude session is working.
    pulse: bool,
    title: String,
    right: String,
    active: bool,
    dim: bool,
    running: bool,
    indent: bool,
    stale: bool,
    archived: bool,
    /// Which sidebar section renders the row; keeps element ids unique when a session shows in several.
    scope: &'static str,
}

/// Drag payload: a running session row, dropped onto another to reorder.
struct DragSession(Id);

/// The drag view GPUI requires. It draws nothing: the dragged row itself stays in the list (lifted, see
/// `lifted`) and slides to wherever the cursor is, so it can never leave its list.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Marks `id` as the item being dragged and builds the (invisible) drag view.
fn begin_drag(ws: &WeakEntity<Workspace>, id: &Id, cx: &mut gpui::App) -> gpui::Entity<DragGhost> {
    ws.update(cx, |ws, cx| {
        ws.dragging = Some(id.clone());
        ws.drag_pos = None;
        cx.notify();
    })
    .ok();
    cx.new(|_| DragGhost)
}

/// Style of the row or header being dragged.
fn lifted<T: Styled>(el: T) -> T {
    el.bg(hsla(theme::ROW_ACTIVE)).border_1().border_color(hsla(theme::TEXT_FAINT))
}

/// Height of a session row plus the gap between rows.
const ROW_PITCH: f32 = 29.0;
const SLIDE_MS: u64 = 160;

/// A row or project block that just changed places: it starts `from` px away from its new slot and slides in.
/// `epoch` makes each reorder a fresh animation.
#[derive(Clone, Copy)]
pub struct Slide {
    from: f32,
    epoch: u64,
}

/// True when `new` holds the same items as `prev` in a different order (not an add or remove).
fn is_reorder(prev: &[Id], new: &[Id]) -> bool {
    prev != new && prev.len() == new.len() && new.iter().all(|i| prev.contains(i))
}

/// Wraps `el` in the slide-in animation when `slide` is set.
fn slid(el: impl IntoElement + gpui::Styled + 'static, id: &Id, slide: Option<Slide>) -> AnyElement {
    match slide {
        Some(Slide { from, epoch }) => el
            .with_animation(
                SharedString::from(format!("slide-{id}-{epoch}")),
                Animation::new(Duration::from_millis(SLIDE_MS)).with_easing(ease_out_quint()),
                move |d, t| d.relative().top(px(from * (1.0 - t))),
            )
            .into_any_element(),
        None => el.into_any_element(),
    }
}

/// Moves `v[from]` into the slot of `v[to]`, shifting the items in between.
fn move_to<T>(v: &mut [T], from: usize, to: usize) {
    if from < to {
        v[from..=to].rotate_left(1);
    } else {
        v[to..=from].rotate_right(1);
    }
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Show in Explorer" }
}

impl Workspace {
    /// True when the pointer is somewhere new. A list shifting under a still pointer (a session ending, say) makes
    /// GPUI re-send a move at the same spot; that must not reorder anything.
    ///
    /// Also false while the previous swap is still sliding: rows are hit-tested where they are painted, so a row
    /// mid-slide sits under the pointer and would be swapped straight back (flicker).
    fn pointer_moved(&mut self, pos: gpui::Point<Pixels>) -> bool {
        if self.last_swap.elapsed() < Duration::from_millis(SLIDE_MS + 40) {
            return false;
        }
        let moved = self.drag_pos != Some(pos);
        self.drag_pos = Some(pos);
        moved
    }

    fn project_name(&self, id: Option<&Id>) -> String {
        id.and_then(|i| self.store.project(i)).map(|p| p.name.clone()).unwrap_or_default()
    }

    fn matches(&self, q: &str, rec: &SessionRecord) -> bool {
        q.is_empty()
            || rec.display_title().to_lowercase().contains(q)
            || rec.cwd.to_string_lossy().to_lowercase().contains(q)
            || self.project_name(rec.project_id.as_ref()).to_lowercase().contains(q)
            || (rec.kind == SessionKind::Shell && "shell terminal".contains(q))
    }

    fn row_for(&self, rec: &SessionRecord, running: bool, with_project: bool) -> Row {
        let activity = self.activity.get(&rec.id).copied();
        let (glyph, color) = match (rec.kind, running) {
            // Orange only when the session stopped and wants the user; gray otherwise, pulsing while it works.
            (SessionKind::Claude, true) => (
                IconName::Brackets,
                match activity {
                    Some(Activity::Attention) => hsla(theme::ACCENT),
                    Some(Activity::Working) => hsla(0xa8a8a8),
                    None => hsla(0x6b6b6b),
                },
            ),
            (SessionKind::Shell, true) => (IconName::Terminal, hsla(theme::OK)),
            (SessionKind::Claude, false) => (IconName::Brackets, hsla(theme::TEXT_FAINT)),
            (SessionKind::Shell, false) => (IconName::Terminal, hsla(theme::TEXT_FAINT)),
        };
        let stale = rec.claude.as_ref().is_some_and(|c| c.external == ExternalState::Missing);
        Row {
            id: rec.id.clone(),
            glyph,
            glyph_color: color,
            pulse: running && activity == Some(Activity::Working),
            title: rec.display_title().to_string(),
            right: if with_project {
                let p = self.project_name(rec.project_id.as_ref());
                if p.is_empty() {
                    rec.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                } else {
                    p
                }
            } else {
                ago(rec.last_active, now())
            },
            active: self.selected.as_deref() == Some(rec.id.as_str()),
            dim: !running,
            running,
            indent: !with_project,
            stale,
            archived: rec.hidden,
            scope: if with_project { "cur" } else { "prj" },
        }
    }

    fn session_menu(&self, rec: &SessionRecord, running: bool) -> Vec<MenuItem> {
        let mut items = vec![];
        let item = |label: &str, act: Act, danger: bool, sep: bool| MenuItem { label: label.into(), hint: None, act, danger, disabled: false, separator_before: sep };
        if running {
            items.push(item("Switch to session", Act::Select(rec.id.clone()), false, false));
        } else if rec.kind == SessionKind::Claude {
            items.push(item("Resume session", Act::Resume(rec.id.clone()), false, false));
        }
        if running && rec.kind == SessionKind::Claude {
            items.push(item("Show or hide shell", Act::ToggleShell(rec.id.clone()), false, false));
        }
        items.push(item("Rename…", Act::Rename(rec.id.clone()), false, false));
        if rec.kind == SessionKind::Claude {
            items.push(item("Regenerate title", Act::RegenTitle(rec.id.clone()), false, false));
        }
        if let Some(c) = &rec.claude {
            items.push(item("Copy Claude session ID", Act::CopyText(c.session_id.clone()), false, true));
        }
        items.push(item("Copy folder path", Act::CopyText(rec.cwd.to_string_lossy().into_owned()), false, rec.claude.is_none()));
        items.push(item(reveal_label(), Act::Reveal(rec.cwd.clone()), false, false));
        if running {
            items.push(item("End session", Act::CloseLive(rec.id.clone()), true, true));
        } else {
            items.push(item(if rec.hidden { "Unarchive" } else { "Archive" }, Act::Archive(rec.id.clone(), !rec.hidden), false, true));
        }
        items
    }

    fn project_menu(&self, p: &Project) -> Vec<MenuItem> {
        let item = |label: String, act: Act, danger: bool, sep: bool| MenuItem { label, hint: None, act, danger, disabled: false, separator_before: sep };
        let mut items = vec![
            item("New Claude session here".into(), Act::NewClaude { cwd: p.path.clone() }, false, false),
            item("New chat here".into(), Act::NewChat { cwd: Some(p.path.clone()) }, false, false),
        ];
        if p.git.is_some() {
            items.push(item("GitHub issues & PRs".into(), Act::OpenGithub { cwd: p.path.clone() }, false, true));
        }
        let mut first = true;
        for (editor, found) in &self.editors {
            if *found {
                items.push(item(format!("Open in {}", editor.label()), Act::OpenEditor(*editor, p.path.clone()), false, first));
                first = false;
            }
        }
        items.push(item(reveal_label().into(), Act::Reveal(p.path.clone()), false, first));
        items.push(item("Copy path".into(), Act::CopyText(p.path.to_string_lossy().into_owned()), false, false));
        items.push(item("Remove project".into(), Act::RemoveProject(p.id.clone()), true, true));
        items
    }

    /// A session row you can drag: onto a project header to file it there and, in CURRENT SESSION (`reorder`),
    /// over other rows to reorder them live.
    fn session_row(&self, row: Row, reorder: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = row.id.clone();
        let ws = cx.entity().downgrade();
        let this = id.clone();
        self.render_row(row, cx)
            .on_drag(DragSession(id.clone()), move |d, _, _, cx| begin_drag(&ws, &d.0, cx))
            .when(self.dragging.as_ref() == Some(&id), lifted)
            .when(reorder, |d| {
                d.on_drag_move(cx.listener(move |ws, ev: &DragMoveEvent<DragSession>, _w, cx| {
                    let from = ev.drag(cx).0.clone();
                    if from == this || !ev.bounds.contains(&ev.event.position) || !ws.pointer_moved(ev.event.position) {
                        return;
                    }
                    let at = |id: &Id| ws.live.iter().position(|l| l.id == *id);
                    if let (Some(from), Some(to)) = (at(&from), at(&this)) {
                        move_to(&mut ws.live, from, to);
                        ws.last_swap = Instant::now();
                        cx.notify();
                    }
                }))
            })
    }

    fn render_row(&self, row: Row, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = row.id.clone();
        let click_id = id.clone();
        let menu_id = id.clone();
        let close_id = id.clone();
        let archive_id = id.clone();
        let dot = div().w(px(14.0)).flex_none().text_color(row.glyph_color);
        let dot = if row.pulse {
            dot.child(spinner(gpui::SharedString::from(format!("spin-{}-{}", row.scope, id)), 14.0))
        } else {
            dot.child(icon(row.glyph, 14.0))
        };
        div()
            .id(gpui::SharedString::from(format!("row-{}-{}", row.scope, id)))
            .group("row")
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(27.0))
            .pl(px(if row.indent { 26.0 } else { 12.0 }))
            .pr(px(8.0))
            .mx(px(6.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .when(row.active, |d| d.bg(hsla(theme::ROW_ACTIVE)))
            .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Select(click_id.clone()), window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |ws, ev: &MouseDownEvent, _window, cx| {
                    if let Some(rec) = ws.store.session(&menu_id).cloned() {
                        let running = ws.is_running(&menu_id, cx);
                        let items = ws.session_menu(&rec, running);
                        ws.open_menu(ev.position, items, cx);
                    } else if ws.chat_store.get(&menu_id).is_some() {
                        let items = ws.chat_menu(&menu_id);
                        ws.open_menu(ev.position, items, cx);
                    }
                }),
            )
            .child(dot)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(13.0))
                    .text_color(hsla(if row.dim { theme::TEXT_DIM } else { theme::TEXT }))
                    .child(row.title),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(96.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(11.0))
                    .text_color(hsla(if row.stale { theme::WARN } else { theme::TEXT_FAINT }))
                    .child(if row.archived { "archived".to_string() } else if row.stale { "stale".to_string() } else { row.right }),
            )
            .when(row.running, |d| {
                d.child(
                    icon_button(gpui::SharedString::from(format!("close-{}-{close_id}", row.scope)), IconName::X)
                        .invisible()
                        .group_hover("row", |s| s.visible())
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            ws.act(Act::CloseLive(close_id.clone()), window, cx);
                        })),
                )
            })
            // A past chat is removed from its right-click menu only: a button here sits where the end button was.
            .when(!row.running && row.scope != "chat", |d| {
                let archive = !row.archived;
                d.child(
                    icon_button(gpui::SharedString::from(format!("archive-{}-{archive_id}", row.scope)), if archive { IconName::Archive } else { IconName::ArchiveRestore })
                        .invisible()
                        .group_hover("row", |s| s.visible())
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            ws.act(Act::Archive(archive_id.clone(), archive), window, cx);
                        })),
                )
            })
    }

    fn chat_row(&self, r: &ChatRecord, cx: &gpui::App) -> Row {
        let live = self.chat_pane(&r.id).map(|p| p.view.read(cx).is_busy());
        let project = self.store.project_for_cwd(&r.cwd).and_then(|p| self.store.project(&p)).map(|p| p.name.clone());
        Row {
            glyph: IconName::MessageSquare,
            glyph_color: hsla(match live {
                Some(true) => 0xa8a8a8,
                Some(false) => 0x6b6b6b,
                None => theme::TEXT_FAINT,
            }),
            pulse: live == Some(true),
            title: r.title.clone(),
            right: project.unwrap_or_else(|| ago(r.last_active, now())),
            active: self.selected.as_deref() == Some(r.id.as_str()),
            dim: live.is_none(),
            running: live.is_some(),
            indent: false,
            stale: false,
            archived: false,
            scope: "chat",
            id: r.id.clone(),
        }
    }

    /// Code sessions shown in the list: matching the search, and in the project filter when there is one.
    fn listed(&self, q: &str, filter: Option<&Id>, rec: &SessionRecord) -> bool {
        self.matches(q, rec) && filter.is_none_or(|f| rec.project_id.as_ref() == Some(f))
    }

    /// Chats shown in the list: the same two rules, a chat's project being the one its folder belongs to.
    fn chat_listed(&self, q: &str, filter: Option<&Id>, r: &ChatRecord) -> bool {
        let project = self.store.project_for_cwd(&r.cwd);
        let matches = q.is_empty()
            || r.title.to_lowercase().contains(q)
            || project.as_ref().and_then(|p| self.store.project(p)).is_some_and(|p| p.name.to_lowercase().contains(q))
            || (crate::chat::store::is_tied(&r.cwd) && r.cwd.to_string_lossy().to_lowercase().contains(q));
        matches && filter.is_none_or(|f| project.as_ref() == Some(f))
    }

    /// The project the list is filtered to, if any (a removed project filters nothing).
    pub fn project_filter(&self) -> Option<&Project> {
        self.store.data.layout.project_filter.as_ref().and_then(|id| self.store.project(id))
    }

    /// A row of the list: a chat opens by click, a Claude Code session can also be dragged.
    fn list_row(&self, row: Row, cx: &mut Context<Self>) -> Stateful<Div> {
        if row.scope == "chat" { self.render_row(row, cx) } else { self.session_row(row, false, cx) }
    }

    /// One list for chats and Claude Code sessions: what is running first (terminals in the order you arranged them,
    /// then open chats), then the most recent, with the rest behind "Older". The project filter narrows all of it.
    pub fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        if self.dragging.is_some() && !cx.has_active_drag() {
            self.dragging = None;
        }
        let q = self.search.trim().to_lowercase();
        let searching = !q.is_empty();
        let filter = self.project_filter().map(|p| p.id.clone());

        // ---- RUNNING
        let mut running: Vec<Row> = self
            .live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
            .filter_map(|l| self.store.session(&l.id))
            .filter(|r| self.listed(&q, filter.as_ref(), r))
            .map(|r| self.row_for(r, true, true))
            .collect();
        running.extend(
            self.chats
                .iter()
                .filter_map(|p| self.chat_store.get(&p.id))
                .filter(|r| self.chat_listed(&q, filter.as_ref(), r))
                .map(|r| self.chat_row(r, cx)),
        );

        // ---- RECENT: everything else, newest first
        let mut recent: Vec<(i64, Row)> = vec![];
        for rec in self.store.data.sessions.iter().filter(|s| s.kind == SessionKind::Claude && (searching || !s.hidden) && self.listed(&q, filter.as_ref(), s)) {
            if !self.is_running(&rec.id, cx) {
                let mut row = self.row_for(rec, false, true);
                row.scope = "rec";
                recent.push((rec.last_active, row));
            }
        }
        for r in self.chat_store.shown().filter(|r| self.chat_pane(&r.id).is_none() && self.chat_listed(&q, filter.as_ref(), r)) {
            recent.push((r.last_active, self.chat_row(r, cx)));
        }
        recent.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        let older = recent.split_off(recent.len().min(if searching { usize::MAX } else { RECENT_LIMIT }));

        let mut body = div().flex().flex_col().gap(px(2.0));
        body = body.child(self.section_header("RUNNING", Some(running.len().to_string()), None::<Stateful<Div>>));
        if running.is_empty() {
            let text = if searching || filter.is_some() { "Nothing running matches" } else { "Nothing running. Start a session or a chat." };
            body = body.child(div().px(px(18.0)).py(px(6.0)).text_size(px(12.0)).text_color(hsla(theme::TEXT_FAINT)).child(text));
        }
        let ids: Vec<Id> = running.iter().map(|r| r.id.clone()).collect();
        if is_reorder(&self.prev_current, &ids) {
            self.slide_gen += 1;
            for (new, id) in ids.iter().enumerate() {
                if let Some(old) = self.prev_current.iter().position(|x| x == id).filter(|&old| old != new && self.dragging.as_ref() != Some(id)) {
                    self.slides.insert(id.clone(), Slide { from: (old as f32 - new as f32) * ROW_PITCH, epoch: self.slide_gen });
                }
            }
        }
        self.prev_current = ids;
        for row in running {
            let (id, slide) = (row.id.clone(), self.slides.get(&row.id).copied());
            let el = if row.scope == "chat" { self.render_row(row, cx) } else { self.session_row(row, true, cx) };
            body = body.child(slid(el, &id, slide));
        }

        if !recent.is_empty() || !older.is_empty() {
            body = body.child(div().h(px(10.0))).child(self.section_header("RECENT", None, None::<Stateful<Div>>));
            for (_, row) in recent {
                body = body.child(self.list_row(row, cx));
            }
        } else if searching || filter.is_some() {
            body = body.child(div().px(px(18.0)).py(px(6.0)).text_size(px(12.0)).text_color(hsla(theme::TEXT_FAINT)).child("No sessions or chats match"));
        }
        if !older.is_empty() {
            let open = self.store.data.layout.older_open;
            body = body.child(
                div()
                    .id("older-header")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(18.0))
                    .h(px(22.0))
                    .cursor_pointer()
                    .child(div().text_color(hsla(theme::TEXT_FAINT)).child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
                    .child(section_label("OLDER"))
                    .child(div().text_size(px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child(older.len().to_string()))
                    .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleOlder, window, cx))),
            );
            if open {
                for (_, row) in older {
                    body = body.child(self.list_row(row, cx));
                }
            }
        }

        self.sidebar_frame(body, window, cx)
    }

    /// "+ Code session" with no project chosen: pick where it starts.
    fn new_session_menu(&self) -> Vec<MenuItem> {
        let item = |label: String, hint: Option<String>, act: Act, disabled: bool, sep: bool| MenuItem { label, hint, act, danger: false, disabled, separator_before: sep };
        let mut items = vec![item("Start the session in…".into(), None, Act::Dismiss, true, false)];
        items.extend(self.store.data.projects.iter().map(|p| item(p.name.clone(), Some(crate::widgets::short_path(&p.path)), Act::NewClaude { cwd: p.path.clone() }, false, false)));
        items.push(item("Add project…".into(), None, Act::ImportProject, false, true));
        items
    }

    /// The project filter: a chip that lists the projects, and (with a project chosen) a ⋯ button for its actions.
    fn render_project_filter(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self.project_filter().cloned();
        let name = current.as_ref().map_or("All projects".to_string(), |p| p.name.clone());
        let chip = div()
            .id("project-filter")
            .flex_1()
            .min_w_0()
            .h(px(26.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded(px(6.0))
            .bg(hsla(0x141414))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(0x1b1b1b)))
            .text_size(px(12.0))
            .text_color(hsla(if current.is_some() { theme::TEXT } else { theme::TEXT_DIM }))
            .child(icon(IconName::FolderOpen, 13.0))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(name))
            .child(div().flex_none().text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::ChevronDown, 12.0)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, ev: &MouseDownEvent, _w, cx| {
                    let chosen = ws.project_filter().map(|p| p.id.clone());
                    let item = |label: String, act: Act, sep: bool| MenuItem { label, hint: None, act, danger: false, disabled: false, separator_before: sep };
                    let mut items = vec![item(format!("{}All projects", tick(chosen.is_none())), Act::FilterProject(None), false)];
                    items.extend(ws.store.data.projects.iter().map(|p| item(format!("{}{}", tick(chosen.as_ref() == Some(&p.id)), p.name), Act::FilterProject(Some(p.id.clone())), false)));
                    items.push(item("Add project…".into(), Act::ImportProject, true));
                    ws.open_menu(ev.position, items, cx);
                }),
            );
        div()
            .occlude()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(chip)
            .when_some(current, |d, p| {
                d.child(icon_button("project-more", IconName::Ellipsis).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                        let items = ws.project_menu(&p);
                        ws.open_menu(ev.position, items, cx);
                    }),
                ))
            })
    }

    /// The sidebar chrome around either list: header, scrolling body, usage footer.
    fn sidebar_frame(&mut self, body: Div, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let width = self.left_width(f32::from(window.viewport_size().width));
        // The right edge is the drag handle (inside the sidebar, so the terminal beside it cannot cover it).
        let handle = div()
            .id("left-resize")
            .absolute()
            .top_0()
            .right_0()
            .w(px(5.0))
            .h_full()
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                    ws.left_drag = Some(SideDrag { x: f32::from(ev.position.x), width });
                    cx.notify();
                }),
            );
        div()
            .id("sidebar")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .bg(hsla(theme::SIDEBAR_BG))
            .border_r_1()
            .border_color(hsla(theme::BORDER))
            // macOS traffic lights sit here; the strip stays draggable like the top bar next to it.
            .when(cfg!(target_os = "macos"), |d| {
                d.child(
                    div()
                        .id("traffic-light-strip")
                        .h(px(40.0))
                        .flex_none()
                        .on_mouse_down(MouseButton::Left, |ev, window, _| {
                            if ev.click_count == 2 {
                                window.titlebar_double_click();
                            } else {
                                window.start_window_move();
                            }
                        }),
                )
            })
            .child(self.render_sidebar_header(cx))
            .child(self.sidebar_scroll.wire(div().id("sidebar-scroll").flex_1().min_h_0(), window).pb(px(10.0)).child(body))
            .child(self.render_footer(cx))
            .child(handle)
    }

    fn render_sidebar_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // With a project chosen a new session starts there (and the button says so); with "All projects" it asks.
        let filter = self.project_filter().map(|p| p.name.clone());
        let label = match filter {
            Some(name) => format!("+ Code in {}", if name.chars().count() > 12 { format!("{}…", name.chars().take(11).collect::<String>()) } else { name }),
            None => "+ Code session".to_string(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(12.0))
            .pt(px(14.0))
            .pb(px(12.0))
            // Same as the top bar: dragging the empty header moves the window, controls occlude it.
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(4.0))
                    .child(clawd(24.0))
                    .child(div().flex_1().text_size(px(13.0)).font_weight(FontWeight::BOLD).text_color(hsla(theme::TEXT)).child("CLAUDIU"))
                    .child(icon_button("settings", IconName::Settings).occlude().on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::OpenSettings, window, cx)))),
            )
            .child(
                div()
                    .occlude()
                    .flex()
                    .gap(px(6.0))
                    .child(button("new-claude", label, true).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, ev: &MouseDownEvent, window, cx| match ws.project_filter().map(|p| p.path.clone()) {
                            Some(cwd) => ws.act(Act::NewClaude { cwd }, window, cx),
                            None if ws.store.data.projects.is_empty() => {
                                let cwd = ws.active_cwd();
                                ws.act(Act::NewClaude { cwd }, window, cx)
                            }
                            None => {
                                let items = ws.new_session_menu();
                                ws.open_menu(ev.position, items, cx);
                            }
                        }),
                    ))
                    .child(
                        button("new-chat", "+ Chat", false)
                            .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::NewChat { cwd: None }, window, cx))),
                    ),
            )
            .child(self.render_project_filter(cx))
            .child(
                div().occlude().child(
                    Input::new(&self.search_input)
                        .h(px(28.0))
                        .text_size(px(12.5))
                        .prefix(div().text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::Search, 14.0))),
                ),
            )
    }

    fn section_header(&self, label: &str, count: Option<String>, trailing: Option<Stateful<Div>>) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(18.0))
            .pr(px(12.0))
            .h(px(22.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(section_label(label.to_string()))
                    .when_some(count, |d, c| d.child(div().text_size(px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child(c))),
            )
            .when_some(trailing, |d, t| d.child(t))
    }

    /// Switch account, add one, or sign in again / remove the active alt (shown greyed out for the main account).
    fn account_menu(&self) -> Vec<MenuItem> {
        let active = self.store.active_account().map(|a| a.id.clone());
        let item = |label: String, hint: Option<String>, act: Act, danger: bool, sep: bool| MenuItem {
            label,
            hint,
            act,
            danger,
            disabled: false,
            separator_before: sep,
        };
        let accounts = &self.store.data.accounts;
        let mut items: Vec<MenuItem> = accounts
            .iter()
            .map(|a| {
                let mark = tick(active.as_ref() == Some(&a.id));
                item(format!("{mark}{}", a.name()), Some(account_hint(a)), Act::SwitchAccount(a.id.clone()), false, false)
            })
            .collect();
        items.push(item("Add account…".into(), None, Act::AddAccount, false, true));
        let id = active.unwrap_or_default();
        let main = self.store.active_account().is_none_or(|a| a.is_main());
        for (label, act, danger) in [
            ("Sign in to this account again…", Act::SignIn(id.clone()), false),
            ("Remove this account…", Act::RemoveAccount(id), true),
        ] {
            items.push(MenuItem { disabled: main, ..item(label.into(), None, act, danger, false) });
        }
        items
    }

    fn render_account_switcher(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let acct = self.store.active_account();
        let name = acct.map_or_else(|| "Main account".to_string(), |a| a.name());
        // `Pro · Alt`, so the row says which kind of account is in use.
        let plan = acct.map(account_hint).unwrap_or_default();
        div()
            .id("account-switcher")
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(28.0))
            .mx(px(-6.0))
            .px(px(6.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, ev: &MouseDownEvent, _window, cx| {
                    let items = ws.account_menu();
                    ws.open_menu(ev.position, items, cx);
                }),
            )
            .child(avatar(&name, 18.0))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.5)).text_color(hsla(theme::TEXT)).child(name))
            .child(div().flex_none().text_size(px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child(plan))
            .child(div().flex_none().text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::ChevronDown, 12.0)))
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = now();
        let usage = self.store.active_account().map(|a| a.usage.clone()).unwrap_or_default();

        // Captions are short: two meters share a row.
        let limit = |label: &str, l: &Option<crate::store::RateLimit>| -> Div {
            match l {
                Some(l) => match (l.resets_at, l.utilization) {
                    (Some(reset), util) if reset > t => {
                        let mut caption = format!("resets {}", countdown(reset - t));
                        if t - l.seen_at > 900 {
                            caption.push_str(&format!(" · {} ago", ago(l.seen_at, t)));
                        }
                        meter(label, util, caption, None)
                    }
                    (Some(_), _) => meter(label, None, "reset · unknown".into(), None),
                    (None, util) => meter(label, util, format!("seen {} ago", ago(l.seen_at, t)), None),
                },
                None => meter(label, None, "after first message".into(), None),
            }
        };

        let pair = |a: Div, b: Div| div().flex().gap(px(14.0)).child(a.flex_1().min_w_0()).child(b.flex_1().min_w_0());
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(18.0))
            .pt(px(8.0))
            .pb(px(12.0))
            .border_t_1()
            .border_color(hsla(theme::BORDER))
            .child(self.render_account_switcher(cx))
            .child(pair(limit("5 HOUR", &usage.five_hour), limit("7 DAY", &usage.seven_day)))
    }
}

/// `email · Pro (Main)` for the menu.
/// `Pro · Main`, shown right of the email in the account menu.
fn account_hint(a: &crate::store::Account) -> String {
    a.plan.as_deref().map_or(kind_label(a).to_string(), |p| format!("{} · {}", plan_label(p), kind_label(a)))
}

fn kind_label(a: &crate::store::Account) -> &'static str {
    if a.is_main() { "Main" } else { "Alt" }
}

/// `pro` → `Pro`.
fn plan_label(plan: &str) -> String {
    let mut c = plan.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

#[cfg(test)]
mod tests {
    use super::{is_reorder, move_to};

    #[test]
    fn moved_item_takes_the_targets_slot() {
        let mut v = ['a', 'b', 'c', 'd'];
        move_to(&mut v, 0, 2);
        assert_eq!(v, ['b', 'c', 'a', 'd']);
        move_to(&mut v, 3, 1);
        assert_eq!(v, ['b', 'd', 'c', 'a']);
    }

    #[test]
    fn only_same_items_in_a_new_order_is_a_reorder() {
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_reorder(&ids(&["a", "b"]), &ids(&["b", "a"])));
        assert!(!is_reorder(&ids(&["a", "b"]), &ids(&["a", "b"])));
        assert!(!is_reorder(&ids(&["a", "b"]), &ids(&["a", "c"])));
        assert!(!is_reorder(&ids(&["a"]), &ids(&["a", "b"])));
    }
}
