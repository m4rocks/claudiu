//! Left sidebar: the running agent sessions, a project filter, the recent ones in a list that opens at the
//! bottom, and the harness usage meters.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, AnyElement, AppContext, DragMoveEvent, SharedString, WeakEntity, ease_out_quint, ClickEvent, Context, Div, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Render,
    Stateful, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use gpui_kit::component::input::Input;
use crate::app::{Act, Activity, MenuItem, SideDrag, Workspace};
use crate::store::{Id, Project, SessionKind, SessionRecord, now};
use crate::panels::theme::{self, theme};
use gpui_kit::assets::IconName;
use crate::panels::status_icon::{SessionDot, StatusIcon};
use crate::panels::logos;
use crate::panels::widgets::{Trigger, ago, countdown, icon};
use crate::panels::ui::button::{self as btn, focusable};
use crate::panels::ui::button::icon_button;
use crate::panels::ui::label::section_label;
use crate::panels::ui::progress::limit_bar;

pub const SIDEBAR_W: f32 = 284.0;
pub const SIDEBAR_MIN: f32 = 220.0;
pub const SIDEBAR_MAX: f32 = 520.0;
/// The open RECENT list takes at most this share of the window's height, and scrolls.
const RECENT_SHARE: f32 = 0.4;

#[derive(Clone, PartialEq)]
struct Row {
    id: Id,
    /// What the leading status icon shows.
    dot: SessionDot,
    /// Which harness this session runs in (for its subtle badge).
    harness: Option<crate::harness::Harness>,
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
    archived: bool,
    /// Which sidebar section renders the row; keeps element ids unique when a session shows in several.
    scope: &'static str,
}

/// Drag payload: a running session row, dropped onto another to reorder.
struct DragSession(Id);

/// The drag view GPUI requires. It draws nothing: the dragged row itself stays in the list (lifted, see
/// `lifted`) and slides to wherever the cursor is, so it can never leave its list.
pub(super) struct DragGhost;

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
pub(super) fn lifted<T: Styled>(el: T) -> T {
    el.bg(theme().row_active).border_1().border_color(theme().subtle_foreground)
}

/// Height of a running session's row plus the gap between rows.
const ROW_PITCH: f32 = RUNNING_ROW_H + LIST_GAP;
/// Heights of the list's parts, fixed so where each one sits is known before anything is drawn (see `visible_items`).
const ROW_H: f32 = 25.0;
/// A running session's row has three lines: project, title and branch.
/// Its lines are 14 + 17 + 14 px with 2 px gaps (see `render_row`), which leaves 5.5 px above and below.
const RUNNING_ROW_H: f32 = 60.0;
const HEADER_H: f32 = 20.0;
const NEW_ROW_H: f32 = 32.0;
const NOTE_H: f32 = 24.0;
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
    pub(super) from: f32,
    pub(super) epoch: u64,
}

/// True when `new` holds the same items as `prev` in a different order (not an add or remove).
pub(super) fn is_reorder<T: PartialEq>(prev: &[T], new: &[T]) -> bool {
    prev != new && prev.len() == new.len() && new.iter().all(|i| prev.contains(i))
}

/// Wraps `el` in the slide-in animation when `slide` is set.
pub(super) fn slid(el: impl IntoElement + gpui::Styled + 'static, id: &str, slide: Option<Slide>) -> AnyElement {
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
pub(super) fn move_to<T>(v: &mut [T], from: usize, to: usize) {
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
    /// Opens the Welcome screen to start a session: in the chosen project (named), or asking for one. Lit while the
    /// Welcome screen is shown.
    NewSession { project: Option<String>, shown: bool },
    /// A line of text standing in for rows that aren't there.
    Note(&'static str),
    /// A session; `reorder` lets it be dragged among the others (the running ones).
    Session { row: Row, reorder: bool },
}

impl Item {
    fn height(&self) -> f32 {
        match self {
            Item::NewSession { .. } => NEW_ROW_H,
            Item::Note(_) => NOTE_H,
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

#[derive(PartialEq)]
struct RecentKey { revision: u64, len: usize, query: String, filter: Option<Id>, running: Vec<Id>, open: bool }

/// Filtered session indices, sorted only when Recent is open. Row models are built only for visible entries.
#[derive(Default)]
pub struct RecentCache {
    key: Option<RecentKey>,
    rows: std::sync::Arc<Vec<usize>>,
}

/// The running list has its own cached view so spinner frames do not rebuild the workspace.
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
    pub(super) fn pointer_moved(&mut self, pos: gpui::Point<Pixels>) -> bool {
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
            harness: rec.agent.as_ref().map(|c| c.harness),
            title: rec.display_title().to_string(),
            right: if with_project && !running { project.clone() } else { ago(rec.last_active, now()) },
            project,
            branch,
            active: self.selected.as_deref() == Some(rec.id.as_str()),
            dim: !running,
            running,
            indent: !with_project,
            archived: rec.hidden,
            scope: if with_project { "cur" } else { "prj" },
        }
    }

    fn session_menu(&self, rec: &SessionRecord, running: bool) -> Vec<MenuItem> {
        let mut items = vec![];
        let item = |label: &str, act: Act, danger: bool, sep: bool| MenuItem { label: label.into(), hint: None, detail: None, act, danger, disabled: false, separator_before: sep, remove: None, icon: None, checked: false };
        if running {
            items.push(item("Switch to session", Act::Select(rec.id.clone()), false, false));
        } else if rec.kind == SessionKind::Agent {
            items.push(item("Resume session", Act::Resume(rec.id.clone()), false, false));
        }
        if running && rec.kind == SessionKind::Agent {
            items.push(item("Show or hide shell", Act::ToggleShell(rec.id.clone()), false, false));
        }
        items.push(item("Rename…", Act::Rename(rec.id.clone()), false, false));
        // The first user message is available from the open chat.
        if rec.kind == SessionKind::Agent && running {
            items.push(item("Regenerate title", Act::RegenTitle(rec.id.clone()), false, false));
        }
        if let Some(c) = &rec.agent {
            items.push(item("Copy session ID", Act::CopyText(c.session_id.clone()), false, true));
        }
        items.push(item("Copy folder path", Act::CopyText(rec.cwd.to_string_lossy().into_owned()), false, rec.agent.is_none()));
        items.push(item(reveal_label(), Act::Reveal(rec.cwd.clone()), false, false));
        if running {
            items.push(item("End session", Act::CloseLive(rec.id.clone()), true, true));
        } else {
            items.push(item(if rec.hidden { "Unarchive" } else { "Archive" }, Act::Archive(rec.id.clone(), !rec.hidden), false, true));
        }
        items
    }

    fn project_menu(&self, p: &Project) -> Vec<MenuItem> {
        let item = |label: String, act: Act, danger: bool, sep: bool| MenuItem { label, hint: None, detail: None, act, danger, disabled: false, separator_before: sep, remove: None, icon: None, checked: false };
        let mut items = vec![
            item("New session here".into(), Act::NewSession { cwd: p.path.clone() }, false, false),
            item("Pick up a session here…".into(), Act::ListSessions { cwd: p.path.clone() }, false, false),
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
        let waiting = row.dot == SessionDot::Waiting;
        let row_anchor = crate::panels::widgets::Anchor::default();
        let hover_anchor = row_anchor.clone();
        let hover_id = id.clone();
        let ws = cx.entity().downgrade();
        let hover_key = key.clone();
        let right_text = if row.archived { "archived".to_string() } else { row.right.clone() };
        let right_color = theme().subtle_foreground;
        let title_color = if row.dim { theme().muted_foreground } else { theme().foreground };
        let metadata = row.running.then(|| div().flex().items_center().gap(px(5.0))
            .text_size(crate::panels::scale::px(11.0)).text_color(theme().subtle_foreground)
            .children(row.harness.map(|h| logos::draw(logos::for_harness(h), 11.0, theme().subtle_foreground)))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(row.project.clone()))
            .into_any_element());
        let content = crate::panels::ui::item::content(
            dot,
            div().text_color(title_color).child(row.title.clone()),
            metadata,
            row.running.then(|| row.branch.clone()).flatten(),
            div().text_color(right_color).child(right_text),
        );
        crate::panels::ui::item::item(key, row.active)
            .relative()
            .group("row")
            .when(waiting && running, |d| d.child(row_anchor.probe()))
            .on_hover(move |inside, _window, cx| {
                let changed = ws
                    .update(cx, |ws, cx| {
                        let now = if *inside { Some(hover_key.clone()) } else { ws.hovered_row.clone().filter(|k| *k != hover_key) };
                        let moved = std::mem::replace(&mut ws.hovered_row, now.clone()) != now;
                        if *inside {
                            if waiting
                                && running
                                && ws.live_view(&hover_id).and_then(|v| v.thread()).is_some_and(|t| t.read(cx).is_waiting())
                            {
                                ws.question_popover_seq = ws.question_popover_seq.wrapping_add(1);
                                ws.question_popover = Some(crate::app::QuestionPopover {
                                    id: hover_id.clone(),
                                    row_bounds: hover_anchor.bounds(),
                                });
                                cx.notify();
                            }
                            return ws.unhovered_rows.remove(&hover_key) || moved;
                        }
                        if ws.question_popover.as_ref().is_some_and(|p| p.id == hover_id) {
                            ws.schedule_close_question_popover(cx);
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

            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Select(click_id.clone()), window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |ws, ev: &MouseDownEvent, window, cx| {
                    if let Some(rec) = ws.store.session(&menu_id).cloned() {
                        let running = ws.is_running(&menu_id, cx);
                        let items = ws.session_menu(&rec, running);
                        ws.open_menu(ev.position, items, window, cx);
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
                    // Otherwise the row starts a drag when the pointer moves during the press, and the click is lost.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        ws.act(Act::CloseLive(close_id.clone()), window, cx);
                    }));
                d.child(
                    div()
                        .self_start()
                        .mt(px(4.0))
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
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            ws.act(Act::Archive(archive_id.clone(), archive), window, cx);
                        })),
                )
            })
    }

    /// Code sessions shown in the list (never a subagent): matching the search, and in the project filter when there is one.
    fn listed(&self, q: &str, filter: Option<&Id>, rec: &SessionRecord) -> bool {
        !rec.is_subagent() && self.matches(q, rec) && filter.is_none_or(|f| rec.project_id.as_ref() == Some(f))
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

    /// The running Claude Code sessions, in the order you arranged them, narrowed by the search and project filter.
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
                self.row_for(r, true, true)
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

        let project = self.project_filter().map(|p| p.name.clone());
        let mut items = vec![Item::NewSession { project, shown: matches!(self.main, crate::app::Main::Home) }];
        if running.is_empty() && (searching || filter.is_some()) {
            items.push(Item::Note("Nothing running matches"));
        }
        items.extend(running.into_iter().map(|row| Item::Session { row, reorder: true }));
        ListModel { items, dragging: self.dragging.clone(), scale: crate::panels::scale::percent() }
    }

    /// The Claude Code sessions that aren't running, newest first, narrowed like the running ones. Archived ones only
    /// show up in a search.
    fn recent_rows(&mut self, cx: &gpui::App) -> std::sync::Arc<Vec<usize>> {
        let q = self.search.trim().to_lowercase();
        let filter = self.project_filter().map(|p| p.id.clone());
        let running = self.running_ids(cx);
        let open = !q.is_empty() || self.store.data.layout.recent_open;
        let key = RecentKey { revision: self.store.revision(), len: self.store.data.sessions.len(), query: q.clone(), filter: filter.clone(), running: running.clone(), open };
        if self.recent_cache.key.as_ref() != Some(&key) {
            let running: std::collections::HashSet<_> = running.iter().collect();
            let mut rows: Vec<_> = self.store.data.sessions.iter().enumerate()
                .filter(|(_, s)| s.kind == SessionKind::Agent && (!q.is_empty() || !s.hidden) && !running.contains(&s.id) && self.listed(&q, filter.as_ref(), s))
                .map(|(ix, _)| ix).collect();
            if open { rows.sort_by_key(|&ix| std::cmp::Reverse(self.store.data.sessions[ix].last_active)); }
            self.recent_cache = RecentCache { key: Some(key), rows: std::sync::Arc::new(rows) };
        }
        self.recent_cache.rows.clone()
    }

    /// RECENT at the bottom of the sidebar: a header that opens a list of the sessions that aren't running. The list
    /// takes at most `RECENT_SHARE` of the window and scrolls. A search opens it, so what matches shows. Hidden when
    /// there are no such sessions and nothing is searched.
    fn recent_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let rows = self.recent_rows(cx);
        let searching = !self.search.trim().is_empty();
        if rows.is_empty() && !searching && self.project_filter().is_none() {
            return None;
        }
        let open = searching || self.store.data.layout.recent_open;
        let header = div()
            .id("recent-header")
            .flex()
            .items_center()
            .gap(px(5.0))
            .px(px(14.0))
            .h(px(HEADER_H + 8.0))
            .cursor_pointer()
            .hover(|s| s.bg(theme().row_hover))
            .child(div().text_color(theme().subtle_foreground).child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
            .child(section_label("RECENT"))
            .child(div().text_size(crate::panels::scale::px(10.5)).text_color(theme().subtle_foreground).child(rows.len().to_string()))
            .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleRecent, window, cx)));
        let mut section = div().flex().flex_col().flex_none().w_full().border_t_1().border_color(theme().border).child(header);
        if open {
            let me = cx.entity_id();
            let mut list = div().flex().flex_col().w_full().gap(px(LIST_GAP)).pb(px(6.0));
            if rows.is_empty() {
                list = list.child(self.list_item(Item::Note("No sessions match"), me, cx));
            }
            let max = (f32::from(window.viewport_size().height) * RECENT_SHARE).max(ROW_H * 4.0);
            let pitch = ROW_H + LIST_GAP;
            let total = (rows.len() as f32 * pitch - LIST_GAP).max(0.0);
            let top = (-f32::from(self.recent_scroll.handle().offset().y)).min((total - max).max(0.0));
            let first = ((top - OVERSCAN).max(0.0) / pitch).floor() as usize;
            let end = (((top + max + OVERSCAN) / pitch).ceil() as usize).min(rows.len());
            let mut stack = div().relative().w_full().h(px(total));
            for pos in first..end {
                let mut row = self.row_for(&self.store.data.sessions[rows[pos]], false, true);
                row.scope = "rec";
                stack = stack.child(div().absolute().top(px(pos as f32 * pitch)).left_0().right_0().child(self.session_row(row, false, me, cx)));
            }
            list = list.child(stack);
            section = section.child(self.recent_scroll.wire(div().id("recent-scroll").w_full().max_h(px(max)), window).child(list));
        }
        Some(section)
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
            Item::NewSession { project, shown } => self.new_session_row(project, shown, cx).into_any_element(),
            Item::Note(text) => div()
                .h(px(NOTE_H))
                .flex()
                .items_center()
                .px(px(14.0))
                .text_size(crate::panels::scale::px(12.0))
                .text_color(theme().subtle_foreground)
                .child(text)
                .into_any_element(),
            Item::Session { row, reorder: true } => {
                let (id, slide) = (row.id.clone(), self.slides.get(&row.id).copied());
                slid(self.session_row(row, true, list, cx), &id, slide)
            }
            Item::Session { row, reorder: false } => self.session_row(row, false, list, cx).into_any_element(),
        }
    }

    /// The project filter: a chip that opens the universal Project picker, and (with a project chosen) a ⋯ button for its actions.
    fn render_project_filter(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self.project_filter().cloned();
        let name = current.as_ref().map_or("All projects".to_string(), |p| p.name.clone());
        let filter_open = matches!(
            self.picker_kind(),
            Some(crate::app::WorkspacePicker::Project { mode: crate::app::ProjectPickerMode::SidebarFilter, .. })
        );
        let chip = btn::button("project-filter", None, "", btn::Variant::Ghost, btn::Size::Sm)
            .flex_1()
            .min_w_0()
            .h(px(24.0))
            .px(px(6.0))
            .justify_start()
            .gap(px(6.0))
            .rounded(px(6.0))
            .bg(if filter_open { theme().row_active } else { theme().sidebar })
            .border_color(if filter_open { theme().primary } else { theme().sidebar })
            .cursor_pointer()
            .text_size(crate::panels::scale::px(12.0))
            .text_color(if current.is_some() || filter_open { theme().foreground } else { theme().muted_foreground })
            .child(icon(IconName::FolderOpen, 13.0))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(name))
            .child(div().flex_none().text_color(theme().subtle_foreground).child(icon(IconName::ChevronDown, 12.0)))
            .dropdown(cx, |ws, at, window, cx| {
                ws.open_project_picker(crate::app::ProjectPickerMode::SidebarFilter, at, window, cx);
            });
        div()
            .occlude()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(chip)
            .when_some(current, |d, p| {
                d.child(icon_button("project-more", IconName::Ellipsis).dropdown(
                    cx,
                    move |ws, at, window, cx| {
                        let items = ws.project_menu(&p);
                        ws.open_dropdown(at, items, window, cx);
                    },
                ))
            })
            .child(icon_button("settings", IconName::Settings).occlude().on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::OpenSettings, window, cx))))
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
                    .bg(theme().resize_handle)
                    .group_hover("left-resize", |s| s.bg(theme().resize_handle_hover)),
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
                    .border_color(theme().panel_border)
                    .bg(theme().sidebar)
                    .child(self.render_sidebar_header(cx))
                    .child(list)
                    .children(self.recent_section(window, cx))
                    .children(self.render_footer(cx)),
            )
            .child(handle)
    }

    fn render_sidebar_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(8.0))
            .pt(px(10.0))
            .pb(px(8.0))
            .child(self.render_project_filter(cx))
            .child(
                div().occlude().child(
                    Input::new(&self.search_input)
                        .h(px(26.0))
                        .text_size(crate::panels::scale::px(12.5))
                        .prefix(div().text_color(theme().subtle_foreground).child(icon(IconName::Search, 14.0))),
                ),
            )
    }

    /// The first row of the session list: a neutral row like a session's, which opens the Welcome screen. With a
    /// project chosen a new session starts there, and the row says so; with "All projects" the Welcome screen asks.
    fn new_session_row(&self, project: Option<String>, shown: bool, cx: &mut Context<Self>) -> gpui_kit::base::Button {
        let label = match project {
            Some(name) => format!("New session in {name}"),
            None => "New session".to_string(),
        };
        focusable("new-claude")
            .justify_start()
            .gap(px(ROW_GAP))
            .h(px(NEW_ROW_H))
            .pl(px(10.0))
            .pr(px(8.0))
            .mx(px(5.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .text_color(if shown { theme().foreground } else { theme().muted_foreground })
            .when(shown, |d| d.bg(theme().row_active))
            .hover(|s| s.bg(theme().row_hover).text_color(theme().foreground))
            .child(icon(IconName::Plus, 13.0).text_color(theme().subtle_foreground))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(crate::panels::scale::px(13.0)).child(label))
            .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.open_home(window, cx)))
    }

    /// Connected harnesses' usage limits. Hidden when no connected harness has a limits adapter.
    fn render_footer(&self, cx: &mut Context<Self>) -> Option<Div> {
        use crate::harness::Harness;
        use crate::store::UsageDisplay;
        /// Readings older than this are dimmed to signal staleness.
        const STALE_AFTER_SECS: i64 = 15 * 60;

        let t = now();
        let settings = &self.store.data.settings;
        let display = settings.usage_display;
        let usage = self.store.data.usage_pools(Harness::Claude).into_iter().next().map(|pool| pool.usage).unwrap_or_default();

        // A reset time stays true however old the reading is, so it shows whenever it is still ahead, including for a
        // reading saved before launch. Without one, the reading's age says how far to trust it.
        let limit = |id: String, l: &Option<crate::store::RateLimit>, calm: gpui::Hsla, loading: bool| -> Stateful<Div> {
            let (used, right, stale) = match l {
                Some(l) => {
                    let old = t.saturating_sub(l.seen_at) > STALE_AFTER_SECS;
                    match l.resets_at {
                        Some(reset) if reset > t => (l.utilization, countdown(reset - t), old),
                        Some(_) => (None, if loading { "…".into() } else { "—".into() }, true),
                        None => {
                            let age = match ago(l.seen_at, t).as_str() {
                                "now" => "just now".into(),
                                other => format!("{other} ago"),
                            };
                            (l.utilization, age, old)
                        }
                    }
                }
                None => (None, if loading { "…".into() } else { "—".into() }, false),
            };
            let fill = used.map(|f| match display {
                UsageDisplay::Used => f,
                UsageDisplay::Left => (1.0 - f).clamp(0.0, 1.0),
            });
            let left = fill.map(|f| format!("{:.0}%", (f * 100.0).round())).unwrap_or_else(|| "—".into());
            limit_bar(id, fill, used, left, right, Some(calm), stale).flex_1().min_w_0()
        };
        // Fits the widest marks: two logos, or a logo and a two-letter name, plus an optional error dot.
        let label_w = crate::panels::scale::px(30.0);
        let gap = crate::panels::scale::px(6.0);
        let source_row = |marks: &[&'static logos::Logo], name: Option<String>, color: gpui::Hsla, errored: bool, five: AnyElement, seven: AnyElement| -> Div {
            let mut who = div().flex().flex_none().items_center().gap(crate::panels::scale::px(3.0)).w(label_w).overflow_hidden();
            for logo in marks {
                who = who.child(logos::draw(logo, 12.0, color));
            }
            if let Some(name) = name {
                who = who.child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(crate::panels::scale::px(10.5)).text_color(color).child(name));
            }
            if errored {
                who = who.child(div().flex_none().size(px(5.0)).rounded_full().bg(theme().warning));
            }
            div().flex().items_center().gap(gap).child(who).child(five).child(seven)
        };
        let divider = || div().h(px(1.0)).w_full().my(px(2.0)).bg(theme().border);
        let any_loading = Harness::registered().any(|h| self.limits_state.get(h).loading);
        let refresh_btn = div()
            .id("limits-refresh")
            .flex_none()
            .w(label_w)
            .flex()
            .items_center()
            .cursor_pointer()
            .rounded(px(3.0))
            .text_color(if any_loading { theme().muted_foreground } else { theme().subtle_foreground })
            .hover(|s| s.text_color(theme().foreground))
            .child(icon(
                if any_loading { IconName::LoaderCircle } else { IconName::RefreshCw },
                11.0,
            ))
            .on_click(cx.listener(|ws, _: &ClickEvent, _, cx| ws.refresh_all_limits(cx)));
        let (h5, h7) = match display {
            UsageDisplay::Used => ("5 HOUR", "7 DAY"),
            UsageDisplay::Left => ("5H LEFT", "7D LEFT"),
        };
        let headers = div()
            .flex()
            .items_center()
            .gap(gap)
            .child(refresh_btn)
            .child(section_label(h5).flex_1().min_w_0())
            .child(section_label(h7).flex_1().min_w_0());

        let mut rows: Vec<Div> = Vec::new();
        if settings.harness_enabled(Harness::Claude) {
            let state = self.limits_state.get(Harness::Claude);
            rows.push(source_row(
                &[&logos::CLAUDE],
                None,
                theme().claude,
                state.error.is_some(),
                limit("meter-5h".into(), &usage.five_hour, theme().claude, state.loading).into_any_element(),
                limit("meter-7d".into(), &usage.seven_day, theme().claude, state.loading).into_any_element(),
            ));
        }
        let subagents = &settings.custom_subagents;
        for agent in crate::harness::subagents::Agent::ALL {
            let harness = Harness::from_agent(agent);
            let provider = agent.provider();
            if !harness.is_registered() || !(settings.harness_enabled(harness) || subagents.get(agent).on) {
                continue;
            }
            if provider.dashboard_url.is_some() && !settings.harness_enabled(harness) {
                continue;
            }
            if harness.adapter().usage.is_none() {
                if let Some(url) = provider.dashboard_url {
                    if !rows.is_empty() {
                        rows.push(divider());
                    }
                    let color = (provider.color)(theme());
                    let who = div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(crate::panels::scale::px(3.0))
                        .w(label_w)
                        .overflow_hidden()
                        .child(logos::draw(logos::for_harness(harness), 12.0, color));
                    let link = div()
                        .id(SharedString::from(format!("limits-dashboard-{}", provider.key)))
                        .flex_1()
                        .min_w_0()
                        .h(crate::panels::scale::px(18.0))
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .bg(theme().background)
                        .border_1()
                        .border_color(theme().panel_border)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(4.0))
                        .cursor_pointer()
                        .text_size(crate::panels::scale::px(10.5))
                        .text_color(theme().muted_foreground)
                        .hover(|s| s.bg(theme().accent).text_color(theme().foreground))
                        .child("Usage dashboard")
                        .child(icon(IconName::ExternalLink, 10.5))
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.open_url(url)));
                    rows.push(div().flex().items_center().gap(gap).child(who).child(link));
                }
                continue;
            }
            let state = self.limits_state.get(harness);
            let pools = self.store.data.subagent_limits.get(provider.key).filter(|pools| !pools.is_empty());
            match pools {
                Some(pools) => {
                    if !rows.is_empty() {
                        rows.push(divider());
                    }
                    for (i, pool) in pools.iter().enumerate() {
                        let source = format!("{}-{i}", provider.key);
                        let cell = |window: &str, l: &Option<crate::store::RateLimit>| match l {
                            Some(_) => limit(format!("meter-{source}-{window}"), l, (provider.color)(theme()), state.loading).into_any_element(),
                            None => div().flex_1().min_w_0().h(crate::panels::scale::px(16.0)).into_any_element(),
                        };
                        let (marks, name) = (provider.marks)(&pool.name);
                        rows.push(source_row(
                            marks,
                            name,
                            (provider.color)(theme()),
                            i == 0 && state.error.is_some(),
                            cell("5h", &pool.usage.five_hour),
                            cell("7d", &pool.usage.seven_day),
                        ));
                    }
                }
                // An adapter read only on request shows its row empty until the user refreshes.
                None if state.loading || state.error.is_some() || harness.adapter().usage.as_ref().is_some_and(|usage| usage.every.is_none()) => {
                    if !rows.is_empty() {
                        rows.push(divider());
                    }
                    let (marks, name) = (provider.marks)(provider.name);
                    rows.push(source_row(
                        marks,
                        name,
                        (provider.color)(theme()),
                        state.error.is_some(),
                        limit(format!("meter-{}-5h", provider.key), &None, (provider.color)(theme()), state.loading).into_any_element(),
                        limit(format!("meter-{}-7d", provider.key), &None, (provider.color)(theme()), state.loading).into_any_element(),
                    ));
                }
                None => {}
            }
        }
        for harness in Harness::registered() {
            if settings.harness_enabled(harness) && self.limits_state.get(harness).authentication_required {
                rows.push(div().flex().flex_wrap().items_center().gap(px(6.0))
                    .child(div().text_size(crate::panels::scale::px(11.0)).text_color(theme().warning).child(format!("{} usage is stale.", harness.name())))
                    .child(crate::panels::ui::button::text(format!("usage-sign-in-{}", harness.key()), "Sign in", false)
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.sign_in_to_harness(harness, window, cx)))));
            }
        }
        if rows.is_empty() {
            return None;
        }
        let mut limits = div().flex().flex_col().gap(crate::panels::scale::px(3.0)).child(headers);
        for row in rows {
            limits = limits.child(row);
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .px(px(14.0))
                .pt(px(8.0))
                .pb(px(10.0))
                .border_t_1()
                .border_color(theme().border)
                .child(limits),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Item, ROW_H, RUNNING_ROW_H, Row, SessionDot, is_reorder, move_to, visible_items};

    fn row(running: bool) -> Row {
        Row {
            id: "s".into(),
            dot: SessionDot::Active,
            harness: None,
            title: String::new(),
            right: String::new(),
            project: String::new(),
            branch: None,
            active: false,
            dim: false,
            running,
            indent: false,
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
