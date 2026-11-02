//! Left sidebar: one list of Claude Code sessions (running first, then recent, then older), a project filter, and
//! the account usage meters.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, AnyElement, AppContext, DragMoveEvent, SharedString, WeakEntity, ease_out_quint, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Render,
    Stateful, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use gpui_kit::component::input::Input;
use crate::app::{Act, Activity, MenuItem, SideDrag, Workspace};
use crate::store::{ExternalState, Id, Project, SessionKind, SessionRecord, now};
use crate::panels::theme::{self, hsla};
use gpui_kit::assets::IconName;
use crate::panels::status_icon::{SessionDot, StatusIcon};
use crate::panels::widgets::{Trigger, ago, avatar, button, clawd, countdown, icon, icon_button, key_hint, meter, section_label, shortcut, tick};

pub const SIDEBAR_W: f32 = 284.0;
pub const SIDEBAR_MIN: f32 = 220.0;
pub const SIDEBAR_MAX: f32 = 520.0;
/// The account warning's entrance: its height at the end of the expand (the box grows to it), and its duration.
/// Entries shown under RECENT before the rest go behind OLDER.
const RECENT_LIMIT: usize = 12;

#[derive(Clone, PartialEq)]
struct Row {
    id: Id,
    /// What the leading status icon shows.
    dot: SessionDot,
    title: String,
    right: String,
    /// The project's name, on the first line of a running session's row.
    project: String,
    /// The branch checked out in the session's own working tree, on the last line of a running session's row.
    branch: Option<String>,
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

/// Height of a running session's row plus the gap between rows.
const ROW_PITCH: f32 = RUNNING_ROW_H + LIST_GAP;
/// Heights of the list's parts, fixed so where each one sits is known before anything is drawn (see `visible_items`).
const ROW_H: f32 = 25.0;
/// A running session's row has three lines: project, title and branch.
/// Its lines are 14 + 17 + 14 px with 2 px gaps (see `render_row`), which leaves 5.5 px above and below.
const RUNNING_ROW_H: f32 = 60.0;
const HEADER_H: f32 = 20.0;
const NOTE_H: f32 = 24.0;
const SPACER_H: f32 = 10.0;
const LIST_GAP: f32 = 2.0;
/// Drawn beyond each edge of the view. The list is rebuilt for every frame its offset moves, so one row is enough.
const OVERSCAN: f32 = RUNNING_ROW_H + LIST_GAP;
const SLIDE_MS: u64 = 160;
const CLOSE_SLIDE_MS: u64 = 140;
/// Width of a row's close button, and the gap between a row's parts.
const CLOSE_W: f32 = 22.0;
const ROW_GAP: f32 = 7.0;

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

/// The items stacked `gap` apart that overlap `from..to`, each with its top, and the height of the whole stack.
fn visible_items(heights: &[f32], gap: f32, from: f32, to: f32) -> (Vec<(usize, f32)>, f32) {
    let mut shown = vec![];
    let mut y = 0.0;
    for (ix, h) in heights.iter().enumerate() {
        if y < to && y + h > from {
            shown.push((ix, y));
        }
        y += h + gap;
    }
    (shown, (y - gap).max(0.0))
}

/// One part of the session list, top to bottom.
#[derive(Clone, PartialEq)]
enum Item {
    Header(&'static str, Option<String>),
    /// A line of text standing in for rows that aren't there.
    Note(&'static str),
    Spacer,
    /// A session; `reorder` lets it be dragged among the others (the running ones).
    Session { row: Row, reorder: bool },
    Older { count: usize, open: bool },
}

impl Item {
    fn height(&self) -> f32 {
        match self {
            Item::Header(..) | Item::Older { .. } => HEADER_H,
            Item::Note(_) => NOTE_H,
            Item::Spacer => SPACER_H,
            Item::Session { row, .. } if row.running => RUNNING_ROW_H,
            Item::Session { .. } => ROW_H,
        }
    }
}

/// Everything the session list shows, worked out on each drawing of the workspace (see `render_sidebar`).
#[derive(Clone, PartialEq, Default)]
pub struct ListModel {
    items: Vec<Item>,
    /// The session being dragged, drawn lifted.
    dragging: Option<Id>,
    /// The UI scale (Settings), which sizes the rows' text.
    scale: u32,
}

/// The session list as its own view, so gpui reuses its last drawing on frames that don't change it: terminal output,
/// editor scrolling and the right panel's animations redraw the window, and rebuilding every row for each of those
/// frames was most of their cost. It draws the workspace's `ListModel` and nothing else of its state, so comparing
/// the model tells when the drawing is out of date.
pub struct SessionList {
    ws: WeakEntity<Workspace>,
}

impl SessionList {
    pub fn new(ws: WeakEntity<Workspace>) -> Self {
        Self { ws }
    }
}

impl Render for SessionList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.ws.upgrade() {
            Some(ws) => ws.update(cx, |ws, cx| ws.render_session_list(window, cx)),
            None => div().into_any_element(),
        }
    }
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Show in Explorer" }
}

impl Workspace {
    /// Keeps a running row's × slot drawn while it shrinks away after the pointer left, then redraws the list without it.
    fn shrink_close_slot(&mut self, key: SharedString, cx: &mut Context<Self>) {
        self.unhovered_rows.insert(key.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(CLOSE_SLIDE_MS)).await;
            let _ = this.update(cx, |ws, cx| {
                ws.unhovered_rows.remove(&key);
                if let Some(list) = &ws.session_list {
                    list.update(cx, |_, cx| cx.notify());
                }
            });
        })
        .detach();
    }

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
        // Anything running is at least active; Claude's own status upgrades it to working or attention.
        let dot = if !running {
            SessionDot::Inactive
        } else {
            match activity {
                Some(Activity::Waiting) => SessionDot::Waiting,
                Some(Activity::Attention) => SessionDot::Attention,
                Some(Activity::Working) => SessionDot::Working,
                None => SessionDot::Active,
            }
        };
        let stale = rec.claude.as_ref().is_some_and(|c| c.external == ExternalState::Missing);
        let project = {
            let p = self.project_name(rec.project_id.as_ref());
            if p.is_empty() { rec.cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { p }
        };
        let branch = running
            .then(|| rec.project_id.as_ref().and_then(|id| self.store.project(id)).and_then(|p| p.git.as_ref()).and_then(|g| g.branch_at(&rec.cwd)).map(str::to_string))
            .flatten();
        Row {
            id: rec.id.clone(),
            dot,
            title: rec.display_title().to_string(),
            right: if with_project && !running { project.clone() } else { ago(rec.last_active, now()) },
            project,
            branch,
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
        let item = |label: &str, act: Act, danger: bool, sep: bool| MenuItem { label: label.into(), hint: None, detail: None, act, danger, disabled: false, separator_before: sep, remove: None, icon: None };
        if running {
            items.push(item("Switch to session", Act::Select(rec.id.clone()), false, false));
        } else if rec.kind == SessionKind::Claude {
            items.push(item("Resume session", Act::Resume(rec.id.clone()), false, false));
        }
        if running && rec.kind == SessionKind::Claude {
            items.push(item("Show or hide shell", Act::ToggleShell(rec.id.clone()), false, false));
        }
        items.push(item("Rename…", Act::Rename(rec.id.clone()), false, false));
        // The new title is written by the session's own mod, so the session has to be running.
        if rec.kind == SessionKind::Claude && running {
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
        let item = |label: String, act: Act, danger: bool, sep: bool| MenuItem { label, hint: None, detail: None, act, danger, disabled: false, separator_before: sep, remove: None, icon: None };
        let mut items = vec![item("New Claude session here".into(), Act::NewClaude { cwd: p.path.clone() }, false, false)];
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
    fn session_row(&self, row: Row, reorder: bool, list: gpui::EntityId, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = row.id.clone();
        let ws = cx.entity().downgrade();
        let this = id.clone();
        self.render_row(row, list, cx)
            .on_drag(DragSession(id.clone()), move |d, _, _, cx| begin_drag(&ws, &d.0, cx))
            .when(self.list_model.dragging.as_ref() == Some(&id), lifted)
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

    /// `list` is the view the row is drawn in, redrawn when the pointer enters or leaves the row.
    fn render_row(&self, row: Row, list: gpui::EntityId, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = row.id.clone();
        let click_id = id.clone();
        let menu_id = id.clone();
        let close_id = id.clone();
        let middle_id = id.clone();
        let archive_id = id.clone();
        let dot = StatusIcon::new(format!("dot-{}-{}", row.scope, id), row.dot);
        let key = gpui::SharedString::from(format!("row-{}-{}", row.scope, id));
        // Only the row under the pointer gets its × or archive button; the others keep the space empty. Building a
        // hidden button on every row cost more than the rest of the row.
        let hovered = self.hovered_row.as_ref() == Some(&key);
        let closing = !hovered && self.unhovered_rows.contains(&key);
        let running = row.running;
        let ws = cx.entity().downgrade();
        let hover_key = key.clone();
        let right_text = if row.archived { "archived".to_string() } else if row.stale { "stale".to_string() } else { row.right.clone() };
        let right_color = hsla(if row.stale { theme::WARN } else { theme::TEXT_FAINT });
        let title_color = hsla(if row.dim { theme::TEXT_DIM } else { theme::TEXT });
        let ellipsis = |el: Div| el.min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis();
        let content = if row.running {
            // Project and age, then the title with the status icon, then the branch.
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .flex()
                        .gap(px(6.0))
                        .text_size(crate::panels::scale::px(11.0))
                        .line_height(crate::panels::scale::px(14.0))
                        .text_color(hsla(theme::TEXT_FAINT))
                        .child(ellipsis(div().flex_1()).child(row.project.clone()))
                        .child(div().flex_none().text_color(right_color).child(right_text)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(ellipsis(div().flex_1()).text_size(crate::panels::scale::px(13.0)).line_height(crate::panels::scale::px(17.0)).text_color(title_color).child(row.title.clone()))
                        .child(dot),
                )
                .children(row.branch.clone().map(|b| ellipsis(div()).text_size(crate::panels::scale::px(11.0)).line_height(crate::panels::scale::px(14.0)).text_color(hsla(theme::TEXT_FAINT)).child(b)))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(dot)
                .child(ellipsis(div().flex_1()).text_size(crate::panels::scale::px(13.0)).text_color(title_color).child(row.title.clone()))
                .child(div().flex_none().max_w(px(96.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(crate::panels::scale::px(11.0)).text_color(right_color).child(right_text))
                .into_any_element()
        };
        div()
            .id(key)
            .group("row")
            .on_hover(move |inside, _window, cx| {
                let changed = ws
                    .update(cx, |ws, cx| {
                        let now = if *inside { Some(hover_key.clone()) } else { ws.hovered_row.clone().filter(|k| *k != hover_key) };
                        let moved = std::mem::replace(&mut ws.hovered_row, now.clone()) != now;
                        if *inside {
                            return ws.unhovered_rows.remove(&hover_key) || moved;
                        }
                        if running {
                            ws.shrink_close_slot(hover_key.clone(), cx);
                        }
                        moved || running
                    })
                    .unwrap_or(false);
                if changed {
                    cx.notify(list);
                }
            })
            .flex()
            .items_center()
            .gap(px(ROW_GAP))
            .h(px(if row.running { RUNNING_ROW_H } else { ROW_H }))
            .pl(px(if row.indent { 22.0 } else { 10.0 }))
            .pr(px(6.0))
            .mx(px(5.0))
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
                    }
                }),
            )
            .child(content)
            .when(row.running, |d| {
                // A middle click ends a running session, like its ×.
                d.on_mouse_up(
                    MouseButton::Middle,
                    cx.listener(move |ws, _: &gpui::MouseUpEvent, window, cx| ws.act(Act::CloseLive(middle_id.clone()), window, cx)),
                )
            })
            .when(!hovered && !row.running, |d| d.child(div().flex_none().size(px(22.0))))
            .when((hovered || closing) && row.running, |d| {
                // The slot grows from nothing to the button's width, pushing the age to the left, and shrinks back
                // the same way once the pointer leaves. Its margin cancels the row's gap when the slot is empty, so
                // nothing jumps.
                let slide_key = SharedString::from(format!("close-{}-{}-{close_id}", if closing { "hide" } else { "show" }, row.scope));
                let button = icon_button(gpui::SharedString::from(format!("close-{}-{close_id}", row.scope)), IconName::X)
                    // Shown by hover style: a list that scrolled under a still pointer leaves the old row marked.
                    // While shrinking away the pointer is outside the row, so it is simply shown.
                    .when(!closing, |b| b.invisible().group_hover("row", |s| s.visible()))
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        ws.act(Act::CloseLive(close_id.clone()), window, cx);
                    }));
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .justify_end()
                        .overflow_hidden()
                        .child(button)
                        .with_animation(
                            slide_key,
                            Animation::new(Duration::from_millis(CLOSE_SLIDE_MS)).with_easing(ease_out_quint()),
                            move |d, t| {
                            let open = if closing { 1.0 - t } else { t };
                            d.w(px(CLOSE_W * open)).ml(px(-ROW_GAP * (1.0 - open)))
                        },
                        ),
                )
            })
            .when(hovered && !row.running, |d| {
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

    /// Code sessions shown in the list: matching the search, and in the project filter when there is one.
    fn listed(&self, q: &str, filter: Option<&Id>, rec: &SessionRecord) -> bool {
        self.matches(q, rec) && filter.is_none_or(|f| rec.project_id.as_ref() == Some(f))
    }

    /// The project the list is filtered to, if any (a removed project filters nothing).
    pub fn project_filter(&self) -> Option<&Project> {
        self.store.data.layout.project_filter.as_ref().and_then(|id| self.store.project(id))
    }

    /// The sidebar: header, the session list (its own cached view, see `SessionList`), and the usage footer.
    pub fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model = self.list_model(cx);
        let changed = model != self.list_model;
        self.list_model = model;
        let me = cx.entity().downgrade();
        let list = self.session_list.get_or_insert_with(|| cx.new(|_| SessionList::new(me))).clone();
        // A changed list is drawn afresh; otherwise its last drawing is reused, unless the list asked to be drawn
        // again itself (it scrolled, or the pointer moved to another row).
        let list = if changed {
            div().flex_1().min_h_0().child(list).into_any_element()
        } else {
            div().flex_1().min_h_0().child(list.cached(gpui::StyleRefinement::default().size_full())).into_any_element()
        };
        self.sidebar_frame(list, window, cx)
    }

    /// One list of Claude Code sessions: what is running first (in the order you arranged them), then the most
    /// recent, with the rest behind "Older". The project filter narrows all of it. Rows behind a closed "Older" are
    /// counted, not built.
    fn list_model(&mut self, cx: &mut Context<Self>) -> ListModel {
        if self.dragging.is_some() && !cx.has_active_drag() {
            self.dragging = None;
        }
        let q = self.search.trim().to_lowercase();
        let searching = !q.is_empty();
        let filter = self.project_filter().map(|p| p.id.clone());

        // ---- RUNNING
        let numbered = self.running_ids(cx);
        let running: Vec<Row> = numbered
            .iter()
            .filter_map(|id| self.store.session(id))
            .filter(|r| self.listed(&q, filter.as_ref(), r))
            .map(|r| {
                let mut row = self.row_for(r, true, true);
                // Numbered among all running sessions, so a search or project filter doesn't renumber them.
                if self.shortcuts_shown
                    && let Some(n) = numbered.iter().position(|id| *id == r.id).filter(|&i| i < 9)
                {
                    row.right = shortcut(&(n + 1).to_string());
                }
                row
            })
            .collect();
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

        // ---- RECENT: everything else, newest first
        let mut recent: Vec<&SessionRecord> = self
            .store
            .data
            .sessions
            .iter()
            .filter(|s| s.kind == SessionKind::Claude && (searching || !s.hidden) && !numbered.contains(&s.id) && self.listed(&q, filter.as_ref(), s))
            .collect();
        recent.sort_by_key(|r| std::cmp::Reverse(r.last_active));
        let older = recent.split_off(recent.len().min(if searching { usize::MAX } else { RECENT_LIMIT }));
        let history = |rec: &SessionRecord| {
            let mut row = self.row_for(rec, false, true);
            row.scope = "rec";
            Item::Session { row, reorder: false }
        };

        let mut items = vec![Item::Header("RUNNING", Some(running.len().to_string()))];
        if running.is_empty() {
            items.push(Item::Note(if searching || filter.is_some() { "Nothing running matches" } else { "Nothing running. Start a session." }));
        }
        items.extend(running.into_iter().map(|row| Item::Session { row, reorder: true }));
        if !recent.is_empty() || !older.is_empty() {
            items.extend([Item::Spacer, Item::Header("RECENT", None)]);
            items.extend(recent.into_iter().map(history));
        } else if searching || filter.is_some() {
            items.push(Item::Note("No sessions match"));
        }
        if !older.is_empty() {
            let open = self.store.data.layout.older_open;
            items.push(Item::Older { count: older.len(), open });
            if open {
                items.extend(older.into_iter().map(history));
            }
        }
        ListModel { items, dragging: self.dragging.clone(), scale: crate::panels::scale::percent() }
    }

    /// The session list's own view (see `SessionList`): the parts of `list_model` in view, each at its place.
    pub fn render_session_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let list = window.current_view();
        let scroll = self.sidebar_scroll.wire(div().id("sidebar-scroll").size_full(), window).pb(px(8.0));
        // Where the view stands now (the glide has just moved it). The window's height stands in for the list's: the
        // list's own is only known from the frame before, which is too short right after the window grows.
        let tall = f32::from(window.viewport_size().height);
        let heights: Vec<f32> = self.list_model.items.iter().map(Item::height).collect();
        let total = heights.iter().sum::<f32>() + LIST_GAP * heights.len().saturating_sub(1) as f32;
        // A list that just got shorter is still scrolled to where the longer one was; gpui moves it back within the
        // new end only while laying it out, after this, so the rows are picked from where it will stand.
        let top = (-f32::from(self.sidebar_scroll.handle().offset().y)).min((total - tall).max(0.0));
        let (shown, _) = visible_items(&heights, LIST_GAP, top - OVERSCAN, top + tall + OVERSCAN);
        let mut stack = div().relative().w_full().h(px(total));
        for (ix, y) in shown {
            let el = self.list_item(self.list_model.items[ix].clone(), list, cx);
            stack = stack.child(div().absolute().top(px(y)).left_0().right_0().child(el));
        }
        scroll.child(stack).into_any_element()
    }

    fn list_item(&self, item: Item, list: gpui::EntityId, cx: &mut Context<Self>) -> AnyElement {
        match item {
            Item::Header(label, count) => self.section_header(label, count, None::<Stateful<Div>>).into_any_element(),
            Item::Note(text) => div()
                .h(px(NOTE_H))
                .flex()
                .items_center()
                .px(px(14.0))
                .text_size(crate::panels::scale::px(12.0))
                .text_color(hsla(theme::TEXT_FAINT))
                .child(text)
                .into_any_element(),
            Item::Spacer => div().h(px(SPACER_H)).into_any_element(),
            Item::Session { row, reorder: true } => {
                let (id, slide) = (row.id.clone(), self.slides.get(&row.id).copied());
                slid(self.session_row(row, true, list, cx), &id, slide)
            }
            Item::Session { row, reorder: false } => self.session_row(row, false, list, cx).into_any_element(),
            Item::Older { count, open } => div()
                .id("older-header")
                .flex()
                .items_center()
                .gap(px(5.0))
                .px(px(14.0))
                .h(px(HEADER_H))
                .cursor_pointer()
                .child(div().text_color(hsla(theme::TEXT_FAINT)).child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
                .child(section_label("OLDER"))
                .child(div().text_size(crate::panels::scale::px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child(count.to_string()))
                .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleOlder, window, cx)))
                .into_any_element(),
        }
    }

    /// "+ Code session" with no project chosen: pick where it starts.
    fn new_session_menu(&self) -> Vec<MenuItem> {
        let item = |label: String, detail: Option<String>, act: Act, disabled: bool, sep: bool| MenuItem { label, hint: None, detail, act, danger: false, disabled, separator_before: sep, remove: None, icon: None };
        let mut items = vec![item("Start the session in…".into(), None, Act::Dismiss, true, false)];
        items.extend(self.store.data.projects.iter().map(|p| item(p.name.clone(), Some(crate::panels::widgets::short_path(&p.path)), Act::NewClaude { cwd: p.path.clone() }, false, false)));
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
            .h(px(24.0))
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .rounded(px(6.0))
            .bg(hsla(0x141414))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(0x1b1b1b)))
            .text_size(crate::panels::scale::px(12.0))
            .text_color(hsla(if current.is_some() { theme::TEXT } else { theme::TEXT_DIM }))
            .child(icon(IconName::FolderOpen, 13.0))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(name))
            .child(div().flex_none().text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::ChevronDown, 12.0)))
            .dropdown(
                cx,
                move |ws, at, _w, cx| {
                    let chosen = ws.project_filter().map(|p| p.id.clone());
                    let item = |label: String, act: Act, sep: bool| MenuItem { label, hint: None, detail: None, act, danger: false, disabled: false, separator_before: sep, remove: None, icon: None };
                    let mut items = vec![item(format!("{}All projects", tick(chosen.is_none())), Act::FilterProject(None), false)];
                    items.extend(ws.store.data.projects.iter().map(|p| item(format!("{}{}", tick(chosen.as_ref() == Some(&p.id)), p.name), Act::FilterProject(Some(p.id.clone())), false)));
                    items.push(item("Add project…".into(), Act::ImportProject, true));
                    ws.open_dropdown(at, items, cx);
                },
            );
        div()
            .occlude()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(chip)
            .when_some(current, |d, p| {
                d.child(icon_button("project-more", IconName::Ellipsis).dropdown(
                    cx,
                    move |ws, at, _w, cx| {
                        let items = ws.project_menu(&p);
                        ws.open_dropdown(at, items, cx);
                    },
                ))
            })
    }

    /// The sidebar chrome around either list: header, scrolling body, usage footer.
    fn sidebar_frame(&mut self, list: impl IntoElement, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let width = self.left_width(f32::from(window.viewport_size().width));
        // The right edge is the drag handle: it fills the gutter with a small visible grip.
        let handle_center = theme::PANEL_GAP / 2.0 + 3.0;
        let handle = div()
            .id("left-resize")
            .group("left-resize")
            .absolute()
            .top_0()
            .right(px(-handle_center))
            .w(px(6.0))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                    ws.left_drag = Some(SideDrag { x: f32::from(ev.position.x), width });
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(2.0))
                    .h(px(28.0))
                    .rounded_full()
                    .bg(hsla(0x2b2b30))
                    .group_hover("left-resize", |s| s.bg(hsla(0x4a4a52))),
            );
        div()
            .id("sidebar")
            .relative()
            .flex_none()
            .w(px(width))
            .h_full()
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .rounded(px(theme::PANEL_RADIUS))
                    .overflow_hidden()
                    .border_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .bg(hsla(theme::SIDEBAR_BG))
                    .child(self.render_sidebar_header(cx))
                    .child(list)
                    .child(self.render_footer(cx)),
            )
            .child(handle)
    }

    fn render_sidebar_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // With a project chosen a new session starts there (and the button says so); with "All projects" it asks.
        let filter = self.project_filter().map(|p| p.name.clone());
        let hints = self.shortcuts_shown;
        let label = match filter {
            Some(name) => format!("+ New in {}", if name.chars().count() > 12 { format!("{}…", name.chars().take(11).collect::<String>()) } else { name }),
            None => "+ New".to_string(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(8.0))
            .pt(px(8.0))
            .pb(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(2.0))
                    .child(clawd(24.0))
                    .child(div().flex_1().text_size(crate::panels::scale::px(13.0)).font_weight(FontWeight::BOLD).text_color(hsla(theme::TEXT)).child("CLAUDIU"))
                    .when(hints, |d| d.child(key_hint(",")))
                    .child(icon_button("settings", IconName::Settings).occlude().on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::OpenSettings, window, cx)))),
            )
            .child(
                div()
                    .occlude()
                    .flex()
                    .gap(px(6.0))
                    .child(button("new-claude", label, true).dropdown(
                        cx,
                        |ws, at, window, cx| match ws.project_filter().map(|p| p.path.clone()) {
                            Some(cwd) => ws.act(Act::NewClaude { cwd }, window, cx),
                            None if ws.store.data.projects.is_empty() => {
                                let cwd = ws.active_cwd();
                                ws.act(Act::NewClaude { cwd }, window, cx)
                            }
                            None => {
                                let items = ws.new_session_menu();
                                ws.open_dropdown(at, items, cx);
                            }
                        },
                    ))
                    .when(hints, |d| d.items_center().child(key_hint("Shift+N"))),
            )
            .child(self.render_project_filter(cx))
            .child(
                div().occlude().child(
                    Input::new(&self.search_input)
                        .h(px(26.0))
                        .text_size(crate::panels::scale::px(12.5))
                        .prefix(div().text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::Search, 14.0)))
                        .when(hints, |i| i.suffix(key_hint("Shift+F"))),
                ),
            )
    }

    fn section_header(&self, label: &str, count: Option<String>, trailing: Option<Stateful<Div>>) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(14.0))
            .pr(px(10.0))
            .h(px(HEADER_H))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(section_label(label.to_string()))
                    .when_some(count, |d, c| d.child(div().text_size(crate::panels::scale::px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child(c))),
            )
            .when_some(trailing, |d, t| d.child(t))
    }

    /// Switch account, add one, or sign in again / remove the active alt (shown greyed out for the main account).
    fn account_menu(&self) -> Vec<MenuItem> {
        let active = self.store.active_account().map(|a| a.id.clone());
        let item = |label: String, hint: Option<String>, act: Act, danger: bool, sep: bool| MenuItem {
            label,
            hint,
            detail: None,
            act,
            danger,
            disabled: false,
            separator_before: sep,
            remove: None,
            icon: None,
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
            .h(px(26.0))
            .mx(px(-5.0))
            .px(px(5.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
            .dropdown(cx, |ws, at, _window, cx| {
                let items = ws.account_menu();
                ws.open_dropdown(at, items, cx);
            })
            .child(avatar(&name, 18.0))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(crate::panels::scale::px(12.5)).text_color(hsla(theme::TEXT)).child(name))
            .child(div().flex_none().text_size(crate::panels::scale::px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child(plan))
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

        let pair = |a: Div, b: Div| div().flex().gap(px(12.0)).child(a.flex_1().min_w_0()).child(b.flex_1().min_w_0());
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(14.0))
            .pt(px(6.0))
            .pb(px(8.0))
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
    use super::{Item, ROW_H, RUNNING_ROW_H, Row, SessionDot, is_reorder, move_to, visible_items};

    fn row(running: bool) -> Row {
        Row {
            id: "s".into(),
            dot: SessionDot::Active,
            title: String::new(),
            right: String::new(),
            project: String::new(),
            branch: None,
            active: false,
            dim: false,
            running,
            indent: false,
            stale: false,
            archived: false,
            scope: "cur",
        }
    }

    #[test]
    fn a_running_session_gets_the_tall_row_and_a_recent_one_stays_short() {
        assert_eq!(Item::Session { row: row(true), reorder: true }.height(), RUNNING_ROW_H);
        assert_eq!(Item::Session { row: row(false), reorder: false }.height(), ROW_H);
    }

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

    #[test]
    fn only_items_overlapping_the_view_are_drawn_at_their_place_in_the_stack() {
        // Five items of 10, 20, 10, 10, 10 px stacked 2 px apart sit at 0, 12, 34, 46 and 58; the stack is 68 tall.
        let heights = [10.0, 20.0, 10.0, 10.0, 10.0];
        assert_eq!(visible_items(&heights, 2.0, 15.0, 40.0), (vec![(1, 12.0), (2, 34.0)], 68.0));
        // An item ending exactly where the view starts is out of it; one starting inside is in.
        assert_eq!(visible_items(&heights, 2.0, 44.0, 47.0).0, vec![(3, 46.0)]);
        assert_eq!(visible_items(&heights, 2.0, -100.0, 1000.0).0.len(), 5);
        assert_eq!(visible_items(&[], 2.0, 0.0, 100.0), (vec![], 0.0));
    }
}
