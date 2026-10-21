//! Left sidebar: CURRENT SESSION, PROJECTS, other sessions, and the usage meters.

use std::{cell::RefCell, collections::HashMap, rc::Rc, time::{Duration, Instant}};

use gpui::{
    Animation, AnimationExt, AnyElement, AppContext, DragMoveEvent, SharedString, WeakEntity, canvas, ease_out_quint, ClickEvent, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Render,
    Stateful, StatefulInteractiveElement, Styled, Window, WindowControlArea, div, prelude::FluentBuilder, pulsating_between, px,
};

use gpui_kit::component::input::Input;
use crate::app::{CLOSE_MS, Act, Activity, MenuItem, Workspace};
use crate::store::{ExternalState, Id, Project, SessionKind, SessionRecord, now};
use crate::theme::{self, hsla};
use crate::widgets::{ago, avatar, button, clawd, cog, countdown, divider, icon_button, meter, section_label};

pub const SIDEBAR_W: f32 = 284.0;
const PROJECT_SESSION_LIMIT: usize = 5;

#[derive(Clone)]
struct Row {
    id: Id,
    glyph: &'static str,
    glyph_color: Hsla,
    /// The dot is pulsing: the Claude session is working.
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

/// Drag payloads: a running session row / a project header, dropped onto another of the same kind.
struct DragSession(Id);
struct DragProject(Id);

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

/// Records the painted height of its parent (which must be `relative`) under `id`.
fn measure_height(heights: &Rc<RefCell<HashMap<Id, f32>>>, id: &Id) -> impl IntoElement {
    let (heights, id) = (heights.clone(), id.clone());
    canvas(move |b, _, _| {
            heights.borrow_mut().insert(id, f32::from(b.size.height));
        }, |_, _, _, _| {})
        .absolute()
        .size_full()
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
                "●",
                match activity {
                    Some(Activity::Attention) => hsla(theme::ACCENT),
                    Some(Activity::Working) => hsla(0xa8a8a8),
                    None => hsla(0x6b6b6b),
                },
            ),
            (SessionKind::Shell, true) => ("$", hsla(theme::OK)),
            (SessionKind::Claude, false) => ("○", hsla(theme::TEXT_FAINT)),
            (SessionKind::Shell, false) => ("$", hsla(theme::TEXT_FAINT)),
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
        } else {
            items.push(item("Open new shell here", Act::NewShell { cwd: rec.cwd.clone() }, false, false));
        }
        items.extend(self.split_items(&rec.id, running || rec.kind == SessionKind::Claude));
        items.push(item("Rename…", Act::Rename(rec.id.clone()), false, false));
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

    /// Which split holds `id`: `Some(None)` the one on screen, `Some(Some(i))` the parked split `groups[i]`.
    fn split_slot(&self, id: &Id) -> Option<Option<usize>> {
        if self.panes.len() > 1 && self.panes.contains(id) {
            Some(None)
        } else {
            self.groups.iter().position(|g| g.contains(id)).map(Some)
        }
    }

    /// "Add to split" / "Remove from split" for a session row, when it applies.
    fn split_items(&self, id: &Id, can_open: bool) -> Option<MenuItem> {
        let item = |label: &str, act: Act| MenuItem { label: label.into(), hint: None, act, danger: false, disabled: false, separator_before: false };
        if self.split_slot(id).is_some() {
            Some(item("Remove from split", Act::RemoveFromSplit(id.clone())))
        } else {
            (can_open && !self.panes.is_empty() && self.panes.len() < 4 && !self.panes.contains(id)).then(|| item("Add to split", Act::AddToSplit(id.clone())))
        }
    }

    fn project_menu(&self, p: &Project) -> Vec<MenuItem> {
        let item = |label: String, act: Act, danger: bool, sep: bool| MenuItem { label, hint: None, act, danger, disabled: false, separator_before: sep };
        let mut items = vec![
            item("New Claude session here".into(), Act::NewClaude { cwd: p.path.clone() }, false, false),
            item("New shell here".into(), Act::NewShell { cwd: p.path.clone() }, false, false),
        ];
        if p.git.is_some() {
            items.push(item("GitHub issues & PRs".into(), Act::OpenGithub { cwd: p.path.clone(), split: false }, false, true));
            if !self.panes.is_empty() {
                items.push(item("GitHub issues & PRs in split".into(), Act::OpenGithub { cwd: p.path.clone(), split: true }, false, false));
            }
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
                    // Rows of a split card reorder that split; the others reorder the session list.
                    match (ws.split_slot(&from), ws.split_slot(&this)) {
                        (Some(a), Some(b)) if a == b => {
                            let focused = ws.panes.get(ws.focused).cloned();
                            let v = match a {
                                None => &mut ws.panes,
                                Some(g) => &mut ws.groups[g],
                            };
                            if let (Some(from), Some(to)) = (v.iter().position(|p| *p == from), v.iter().position(|p| *p == this)) {
                                move_to(v, from, to);
                                if a.is_none() {
                                    ws.focused = focused.and_then(|f| ws.panes.iter().position(|p| *p == f)).unwrap_or(0);
                                }
                                ws.last_swap = Instant::now();
                                cx.notify();
                            }
                        }
                        (None, None) => {
                            let at = |id: &Id| ws.live.iter().position(|l| l.id == *id);
                            if let (Some(from), Some(to)) = (at(&from), at(&this)) {
                                move_to(&mut ws.live, from, to);
                                ws.last_swap = Instant::now();
                                cx.notify();
                            }
                        }
                        _ => {}
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
        let dot = div().w(px(12.0)).flex_none().text_size(px(11.0)).text_color(row.glyph_color).child(row.glyph);
        let dot: AnyElement = if row.pulse {
            dot.with_animation(
                gpui::SharedString::from(format!("pulse-{}-{}", row.scope, id)),
                Animation::new(Duration::from_millis(1400)).repeat().with_easing(pulsating_between(0.25, 1.0)),
                |d, t| d.opacity(t),
            )
            .into_any_element()
        } else {
            dot.into_any_element()
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
                    } else if ws.gh_pane(&menu_id).is_some() {
                        let item = |label: &str, act: Act, danger: bool| MenuItem { label: label.into(), hint: None, act, danger, disabled: false, separator_before: false };
                        let mut items = vec![item("Switch to pane", Act::Select(menu_id.clone()), false)];
                        items.extend(ws.split_items(&menu_id, true));
                        items.push(MenuItem { separator_before: true, ..item("Close", Act::CloseLive(menu_id.clone()), true) });
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
                    icon_button(gpui::SharedString::from(format!("close-{}-{close_id}", row.scope)), "×")
                        .invisible()
                        .group_hover("row", |s| s.visible())
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            ws.act(Act::CloseLive(close_id.clone()), window, cx);
                        })),
                )
            })
            .when(!row.running, |d| {
                let archive = !row.archived;
                d.child(
                    icon_button(gpui::SharedString::from(format!("archive-{}-{archive_id}", row.scope)), if archive { "⊟" } else { "↺" })
                        .invisible()
                        .group_hover("row", |s| s.visible())
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            ws.act(Act::Archive(archive_id.clone(), archive), window, cx);
                        })),
                )
            })
    }

    /// The sessions in the split view as one card: an accent rail down the edge and a header with a
    /// miniature of the layout (one block per pane, the focused one lit).
    fn render_split_group(&self, rows: Vec<Row>, size: usize, lit: Option<usize>, cx: &mut Context<Self>) -> Div {
        let map = (0..size).fold(div().flex().flex_none().gap(px(2.0)).w(px(16.0)).h(px(9.0)), |d, i| {
            let lit = lit == Some(i);
            d.child(div().flex_1().rounded(px(2.0)).bg(if lit { hsla(theme::ACCENT) } else { hsla(theme::TEXT_FAINT).opacity(0.55) }))
        });
        let map = if self.split_vertical { map.flex_col() } else { map.flex_row() };
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(19.0))
            .pl(px(12.0))
            .pr(px(10.0))
            .child(section_label("SPLIT"))
            .child(map);
        rows.into_iter().fold(
            div()
                .relative()
                .mx(px(6.0))
                .pb(px(2.0))
                .rounded(px(8.0))
                .border_1()
                .border_color(hsla(theme::BORDER))
                .bg(hsla(0x101011))
                .child(div().absolute().left(px(0.0)).top(px(8.0)).bottom(px(8.0)).w(px(2.0)).rounded_r(px(2.0)).bg(if lit.is_some() { hsla(theme::ACCENT).opacity(0.75) } else { hsla(theme::TEXT_FAINT).opacity(0.4) }))
                .child(header),
            |card, row| card.child(self.session_row(row, true, cx).h(px(24.0)).mx(px(3.0))),
        )
    }

    pub fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        if self.dragging.is_some() && !cx.has_active_drag() {
            self.dragging = None;
        }
        let q = self.search.trim().to_lowercase();
        let searching = !q.is_empty();

        // ---- CURRENT SESSION
        let mut rows: Vec<Row> = self
            .live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
            .filter_map(|l| self.store.session(&l.id))
            .filter(|r| self.matches(&q, r))
            .map(|r| self.row_for(r, true, true))
            .collect();
        rows.extend(
            self.gh
                .iter()
                .filter(|p| q.is_empty() || "github issues pull requests".contains(&q) || p.project.to_lowercase().contains(&q))
                .map(|p| Row {
                    id: p.id.clone(),
                    glyph: "◉",
                    glyph_color: hsla(0x58a6ff),
                    pulse: false,
                    title: "GitHub".into(),
                    right: p.project.clone(),
                    active: self.selected.as_deref() == Some(p.id.as_str()),
                    dim: false,
                    running: true,
                    indent: false,
                    stale: false,
                    archived: false,
                    scope: "cur",
                }),
        );
        // Sessions sharing a split are drawn together, in pane order: one card per split (the shown one lit).
        let splits = self.splits();
        let shown = (self.panes.len() > 1).then(|| self.panes.clone());
        let mut cards: Vec<(Vec<Row>, usize, Option<usize>)> = vec![];
        for g in &splits {
            let mut members: Vec<Row> = rows.iter().filter(|r| g.contains(&r.id)).cloned().collect();
            members.sort_by_key(|r| g.iter().position(|p| *p == r.id));
            if !members.is_empty() {
                let lit = (shown.as_ref() == Some(g)).then_some(self.focused);
                cards.push((members, g.len(), lit));
            }
        }
        let current: Vec<Row> = rows.into_iter().filter(|r| !splits.iter().any(|g| g.contains(&r.id))).collect();
        let current_total = self.running_count(cx) + self.gh.len();

        let mut body = div().flex().flex_col().gap(px(2.0));

        body = body.child(self.section_header("CURRENT SESSION", Some(current_total.to_string()), None::<Stateful<Div>>));
        let has_group = !cards.is_empty();
        for (members, size, lit) in cards {
            body = body.child(self.render_split_group(members, size, lit, cx));
        }
        if current.is_empty() && !has_group {
            body = body.child(
                div().px(px(18.0)).py(px(6.0)).text_size(px(12.0)).text_color(hsla(theme::TEXT_FAINT)).child(if searching {
                    "No running sessions match"
                } else {
                    "Nothing running. Start a Claude session or a shell."
                }),
            );
        }
        let ids: Vec<Id> = current.iter().map(|r| r.id.clone()).collect();
        if is_reorder(&self.prev_current, &ids) {
            self.slide_gen += 1;
            for (new, id) in ids.iter().enumerate() {
                if let Some(old) = self.prev_current.iter().position(|x| x == id).filter(|&old| old != new && self.dragging.as_ref() != Some(id)) {
                    self.slides.insert(id.clone(), Slide { from: (old as f32 - new as f32) * ROW_PITCH, epoch: self.slide_gen });
                }
            }
        }
        self.prev_current = ids;
        for row in current {
            let (id, slide) = (row.id.clone(), self.slides.get(&row.id).copied());
            body = body.child(slid(self.session_row(row, true, cx), &id, slide));
        }

        // ---- PROJECTS
        let add_btn = icon_button("add-project", "+").on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ImportProject, window, cx)));
        body = body.child(div().h(px(10.0)));
        body = body.child(self.section_header("PROJECTS", None, Some(add_btn)));

        let projects: Vec<Project> = self.store.data.projects.clone();
        if projects.is_empty() {
            body = body.child(
                div()
                    .px(px(18.0))
                    .py(px(6.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(div().text_size(px(12.0)).text_color(hsla(theme::TEXT_FAINT)).child("Add a Git project to group its sessions."))
                    .child(
                        div().child(
                            button("import-empty", "Add project folder", false)
                                .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ImportProject, window, cx))),
                        ),
                    ),
            );
        }
        // Projects that changed places slide from where their block was painted to where it is now.
        let ids: Vec<Id> = projects.iter().map(|p| p.id.clone()).collect();
        if !searching && is_reorder(&self.prev_projects, &ids) {
            let heights = self.block_heights.borrow();
            let top = |order: &[Id], id: &Id| -> f32 {
                order.iter().take_while(|x| *x != id).map(|x| heights.get(x).map_or(0.0, |h| h + 2.0)).sum()
            };
            let moves: Vec<(Id, f32)> = ids
                .iter()
                .filter(|id| self.dragging.as_ref() != Some(*id))
                .map(|id| (id.clone(), top(&self.prev_projects, id) - top(&ids, id)))
                .filter(|(_, d)| d.abs() > 0.5)
                .collect();
            drop(heights);
            self.slide_gen += 1;
            for (id, from) in moves {
                self.slides.insert(id, Slide { from, epoch: self.slide_gen });
            }
        }
        self.prev_projects = ids;
        for p in projects {
            let sessions: Vec<SessionRecord> = {
                let mut v: Vec<SessionRecord> = self
                    .store
                    .data
                    .sessions
                    .iter()
                    .filter(|s| s.kind == SessionKind::Claude && s.project_id.as_deref() == Some(p.id.as_str()) && (searching || !s.hidden))
                    .cloned()
                    .collect();
                v.sort_by_key(|s| std::cmp::Reverse(s.last_active));
                v
            };
            let name_match = searching && p.name.to_lowercase().contains(&q);
            let matching: Vec<&SessionRecord> = sessions.iter().filter(|s| name_match || self.matches(&q, s)).collect();
            if searching && !name_match && matching.is_empty() {
                continue;
            }
            let expanded = searching || !p.collapsed;
            let closing = !expanded && self.closing.contains(&p.id);
            let mut group = div().relative().flex().flex_col().child(measure_height(&self.block_heights, &p.id)).child(self.render_project_header(&p, sessions.len(), expanded, cx));
            if expanded || closing {
                let mut kids = div().flex().flex_col();
                let limit = if searching || self.show_all.contains(&p.id) { usize::MAX } else { PROJECT_SESSION_LIMIT };
                let hidden_count = matching.len().saturating_sub(limit);
                for rec in matching.iter().take(limit) {
                    let running = self.is_running(&rec.id, cx);
                    kids = kids.child(self.session_row(self.row_for(rec, running, false), false, cx));
                }
                if hidden_count > 0 {
                    let pid = p.id.clone();
                    kids = kids.child(
                        div()
                            .id(gpui::SharedString::from(format!("more-{pid}")))
                            .ml(px(32.0))
                            .py(px(3.0))
                            .text_size(px(11.5))
                            .text_color(hsla(theme::TEXT_FAINT))
                            .cursor_pointer()
                            .hover(|s| s.text_color(hsla(theme::TEXT_DIM)))
                            .child(format!("Show {hidden_count} more"))
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::ShowAll(pid.clone()), window, cx))),
                    );
                }
                if matching.is_empty() && sessions.is_empty() {
                    kids = kids.child(div().ml(px(32.0)).py(px(3.0)).text_size(px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child("No sessions yet"));
                }
                // Slides open each time the project is expanded (the element is new again after a collapse),
                // and shut again while it is in `closing`. Row heights are fixed, so the full height is known.
                let full = (matching.len().min(limit) as f32 * 27.0 + if hidden_count > 0 || matching.is_empty() && sessions.is_empty() { 22.0 } else { 0.0 }).max(1.0);
                let (key, ms) = if closing { ("shut", CLOSE_MS) } else { ("open", 200) };
                group = group.child(
                    div().overflow_hidden().child(kids).with_animation(
                        gpui::SharedString::from(format!("kids-{key}-{}", p.id)),
                        Animation::new(Duration::from_millis(ms)).with_easing(gpui::ease_in_out),
                        move |d, t| {
                            let t = if closing { 1.0 - t } else { t };
                            d.max_h(px(full * t)).opacity(t)
                        },
                    ),
                );
            }
            body = body.child(slid(group, &p.id, self.slides.get(&p.id).copied()));
        }

        // ---- OTHER SESSIONS (imported, not belonging to a registered project)
        let mut others: Vec<SessionRecord> = self
            .store
            .data
            .sessions
            .iter()
            .filter(|s| s.kind == SessionKind::Claude && s.project_id.is_none() && (searching || !s.hidden) && self.matches(&q, s))
            .cloned()
            .collect();
        others.sort_by_key(|s| std::cmp::Reverse(s.last_active));
        if !others.is_empty() {
            let collapsed = self.store.data.layout.other_collapsed && !searching;
            body = body.child(div().h(px(10.0)));
            body = body.child(
                div()
                    .id("other-header")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(18.0))
                    .h(px(22.0))
                    .cursor_pointer()
                    .child(div().text_size(px(9.0)).text_color(hsla(theme::TEXT_FAINT)).child(if collapsed { "▸" } else { "▾" }))
                    .child(section_label("OTHER SESSIONS"))
                    .child(div().text_size(px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child(others.len().to_string()))
                    .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleOther, window, cx))),
            );
            if !collapsed {
                for rec in others.iter().take(if searching { usize::MAX } else { 12 }) {
                    let running = self.is_running(&rec.id, cx);
                    let mut row = self.row_for(rec, running, true);
                    row.indent = false;
                    row.scope = "oth";
                    body = body.child(self.session_row(row, false, cx));
                }
            }
        }

        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_W))
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
            .child(div().id("sidebar-scroll").flex_1().min_h_0().overflow_y_scroll().pb(px(10.0)).child(body))
            .child(self.render_footer(cx))
    }

    fn render_sidebar_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cwd = self.active_cwd();
        let (cwd_a, cwd_b) = (cwd.clone(), cwd);
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
                    .child(icon_button("settings", "").occlude().group("settings").child(cog("settings")).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::OpenSettings, window, cx)))),
            )
            .child(
                div()
                    .occlude()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        button("new-claude", "+ Claude", true)
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::NewClaude { cwd: cwd_a.clone() }, window, cx))),
                    )
                    .child(
                        button("new-shell", "+ Shell", false)
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::NewShell { cwd: cwd_b.clone() }, window, cx))),
                    )
                    .child(
                        button("new-github", "GitHub", false)
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                                let cwd = ws.active_cwd();
                                ws.act(Act::OpenGithub { cwd, split: false }, window, cx)
                            })),
                    ),
            )
            .child(
                div().occlude().child(
                    Input::new(&self.search_input)
                        .h(px(28.0))
                        .text_size(px(12.5))
                        .prefix(div().text_color(hsla(theme::TEXT_FAINT)).child("⌕")),
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

    fn render_project_header(&self, p: &Project, session_count: usize, expanded: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let (toggle_id, menu_id, new_id_) = (p.id.clone(), p.id.clone(), p.path.clone());
        let branch = p.git.as_ref().and_then(|g| g.branch.clone()).unwrap_or_default();
        let (target, drop_target) = (p.id.clone(), p.id.clone());
        let ws_weak = cx.entity().downgrade();
        let non_git = p.git.is_none();
        let wt = p.git.as_ref().map(|g| g.worktrees.len()).unwrap_or(0);
        div()
            .id(gpui::SharedString::from(format!("proj-{}", p.id)))
            .group("row")
            .flex()
            .items_center()
            .gap(px(7.0))
            .h(px(28.0))
            .pl(px(14.0))
            .pr(px(8.0))
            .mx(px(6.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
            .on_drag(DragProject(p.id.clone()), move |d, _, _, cx| begin_drag(&ws_weak, &d.0, cx))
            .when(self.dragging.as_ref() == Some(&p.id), lifted)
            // Other projects slide out of the way as this one is dragged over them.
            .on_drag_move(cx.listener(move |ws, ev: &DragMoveEvent<DragProject>, _w, cx| {
                let from = ev.drag(cx).0.clone();
                if from == target || !ev.bounds.contains(&ev.event.position) || !ws.pointer_moved(ev.event.position) {
                    return;
                }
                let projects = &mut ws.store.data.projects;
                let at = |id: &Id| projects.iter().position(|p| p.id == *id);
                if let (Some(from), Some(to)) = (at(&from), at(&target)) {
                    move_to(projects, from, to);
                    ws.last_swap = Instant::now();
                    ws.store.mark_dirty();
                    cx.notify();
                }
            }))
            // A session dropped here is filed under this project.
            .drag_over::<DragSession>(|s, _, _, _| s.bg(hsla(theme::ROW_ACTIVE)))
            .on_drop(cx.listener(move |ws, d: &DragSession, _w, cx| {
                if ws.store.session(&d.0).is_some_and(|s| s.kind == SessionKind::Claude) {
                    if let Some(s) = ws.store.session_mut(&d.0) {
                        s.project_id = Some(drop_target.clone());
                    }
                    if let Some(p) = ws.store.data.projects.iter_mut().find(|p| p.id == drop_target) {
                        p.collapsed = false;
                    }
                    ws.store.mark_dirty();
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleProject(toggle_id.clone()), window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |ws, ev: &MouseDownEvent, _window, cx| {
                    if let Some(p) = ws.store.project(&menu_id).cloned() {
                        let items = ws.project_menu(&p);
                        ws.open_menu(ev.position, items, cx);
                    }
                }),
            )
            .child(div().w(px(10.0)).flex_none().text_size(px(9.0)).text_color(hsla(theme::TEXT_FAINT)).child(if expanded { "▾" } else { "▸" }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(hsla(theme::TEXT))
                    .child(p.name.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(88.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(11.0))
                    .text_color(hsla(theme::TEXT_FAINT))
                    .child(if non_git {
                        format!("{session_count}")
                    } else if wt > 0 {
                        format!("{branch} +{wt}")
                    } else {
                        branch
                    }),
            )
            .child(
                icon_button(gpui::SharedString::from(format!("proj-new-{}", p.id)), "+")
                    .invisible()
                    .group_hover("row", |s| s.visible())
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        ws.act(Act::NewClaude { cwd: new_id_.clone() }, window, cx);
                    })),
            )
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
                let mark = if active.as_ref() == Some(&a.id) { "✓  " } else { "    " };
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
            .child(div().flex_none().text_size(px(9.0)).text_color(hsla(theme::TEXT_FAINT)).child("▾"))
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

        let (ctx_fraction, ctx_caption) = match self.selected_context() {
            Some((f, tokens, true)) => (Some(f), format!("{}k tokens", tokens / 1000)),
            Some((f, tokens, false)) => (Some(f), format!("~{}k tokens (estimate)", tokens / 1000)),
            None => (None, "no session".to_string()),
        };

        // Fills with time left; goes red in the last fifth. Expired means the next message re-reads the whole context at full price.
        let cache = match self.selected_cache() {
            Some((_, 0, _)) => meter("CACHE", Some(0.0), "expired".into(), None),
            Some((f, left, ttl)) => {
                let tone = hsla(if f < 0.2 { theme::DANGER } else { theme::ACCENT });
                let window = if ttl >= 3600 { "1h" } else { "5m" };
                let left = if left < 60 { "<1m".to_string() } else { countdown(left) };
                meter("CACHE", Some(f), format!("{left} left of {window}"), Some(tone))
            }
            None => meter("CACHE", None, "no session".into(), None),
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
            .child(divider())
            .child(pair(cache, meter("CONTEXT", ctx_fraction, ctx_caption, None)))
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
