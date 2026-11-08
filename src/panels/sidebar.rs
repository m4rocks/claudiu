//! Left sidebar: one list of Claude Code sessions (running first, then recent, then older), a project filter, and
//! the account usage meters.

use rune::tok::motion;
use rune::{Color, Cursor, El, Phase, PointerButton, ScrollSpec, Transition, Tx, col, el, row, text, text_input};

use crate::app::{Act, Activity, MenuAnchor, MenuItem, SideDrag, Workspace};
use crate::panels::icons;
use crate::panels::status_icon::{SessionDot, status_icon};
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{ago, avatar, clawd, countdown, font, icon, icon_button, key_hint, label, meter, section_label, shortcut, tick};
use crate::store::{ExternalState, Id, Project, SessionKind, SessionRecord, now};

pub const SIDEBAR_W: f32 = 284.0;
pub const SIDEBAR_MIN: f32 = 220.0;
pub const SIDEBAR_MAX: f32 = 520.0;
/// Entries shown under RECENT before the rest go behind OLDER.
const RECENT_LIMIT: usize = 12;

/// Heights of the list's rows. A running session's row has three lines: project, title and branch.
const ROW_H: f32 = 25.0;
const RUNNING_ROW_H: f32 = 60.0;
const HEADER_H: f32 = 20.0;
const NOTE_H: f32 = 24.0;
const SPACER_H: f32 = 10.0;
const LIST_GAP: f32 = 2.0;
/// Height of a running session's row plus the gap between rows: how far a dragged row moves to pass one.
const ROW_PITCH: f32 = RUNNING_ROW_H + LIST_GAP;

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
    stale: bool,
    archived: bool,
    /// Which sidebar section renders the row; keeps element keys unique when a session shows in several.
    scope: &'static str,
}

/// True when `new` holds the same items as `prev` in a different order (not an add or remove).
#[cfg(test)]
fn is_reorder(prev: &[Id], new: &[Id]) -> bool {
    prev != new && prev.len() == new.len() && new.iter().all(|i| prev.contains(i))
}

/// Moves `v[from]` into the slot of `v[to]`, shifting the items in between.
fn move_to<T>(v: &mut [T], from: usize, to: usize) {
    if from < to {
        v[from..=to].rotate_left(1);
    } else {
        v[to..=from].rotate_right(1);
    }
}

/// Where a running row dragged `dy` points from slot `start` lands, among `count` rows.
fn drag_target(start: usize, dy: f32, count: usize) -> usize {
    let moved = (dy / ROW_PITCH).round() as isize;
    (start as isize + moved).clamp(0, count.saturating_sub(1) as isize) as usize
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
    #[cfg(test)]
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

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Show in Explorer" }
}

/// Run `act` from a click, then hand its effects to the frame.
fn act(ws: &mut Workspace, a: Act, tx: &mut Tx) {
    ws.act(a);
    ws.after_input(tx);
}

impl Workspace {
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

    fn row_for(&self, rec: &SessionRecord, running: bool) -> Row {
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
            right: if !running { project.clone() } else { ago(rec.last_active, now()) },
            project,
            branch,
            active: self.selected.as_deref() == Some(rec.id.as_str()),
            dim: !running,
            running,
            stale,
            archived: rec.hidden,
            scope: "cur",
        }
    }

    fn session_menu(&self, rec: &SessionRecord, running: bool) -> Vec<MenuItem> {
        let mut items = vec![];
        let item = |label: &str, act: Act, danger: bool, sep: bool| MenuItem { danger, separator_before: sep, ..MenuItem::new(label, act) };
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

    pub(crate) fn project_menu(&self, p: &Project) -> Vec<MenuItem> {
        let item = |label: String, act: Act, danger: bool, sep: bool| MenuItem { danger, separator_before: sep, ..MenuItem::new(label, act) };
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

    /// Code sessions shown in the list: matching the search, and in the project filter when there is one.
    fn listed(&self, q: &str, filter: Option<&Id>, rec: &SessionRecord) -> bool {
        self.matches(q, rec) && filter.is_none_or(|f| rec.project_id.as_ref() == Some(f))
    }

    /// The project the list is filtered to, if any (a removed project filters nothing).
    pub fn project_filter(&self) -> Option<&Project> {
        self.store.data.layout.project_filter.as_ref().and_then(|id| self.store.project(id))
    }

    /// One list of Claude Code sessions: what is running first (in the order you arranged them), then the most
    /// recent, with the rest behind "Older". The project filter narrows all of it. Rows behind a closed "Older" are
    /// counted, not built.
    fn list_items(&self) -> Vec<Item> {
        let q = self.search.text.trim().to_lowercase();
        let searching = !q.is_empty();
        let filter = self.project_filter().map(|p| p.id.clone());

        // ---- RUNNING
        let numbered = self.running_ids();
        let running: Vec<Row> = numbered
            .iter()
            .filter_map(|id| self.store.session(id))
            .filter(|r| self.listed(&q, filter.as_ref(), r))
            .map(|r| {
                let mut row = self.row_for(r, true);
                // Numbered among all running sessions, so a search or project filter doesn't renumber them.
                if self.shortcuts_shown
                    && let Some(n) = numbered.iter().position(|id| *id == r.id).filter(|&i| i < 9)
                {
                    row.right = shortcut(&(n + 1).to_string());
                }
                row
            })
            .collect();

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
            let mut row = self.row_for(rec, false);
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
        items
    }

    /// The running rows shown, in order: what a dragged row moves among.
    fn shown_running(&self) -> Vec<Id> {
        self.list_items()
            .into_iter()
            .filter_map(|i| match i {
                Item::Session { row, reorder: true } => Some(row.id),
                _ => None,
            })
            .collect()
    }

    /// A running row was dragged `dy` points from where it started: move it to the slot under the pointer, among the
    /// rows shown. `start` is its place when the drag began.
    fn drag_row(&mut self, id: &str, start: usize, dy: f32) {
        let shown = self.shown_running();
        let Some(now) = shown.iter().position(|s| s == id) else { return };
        let to = drag_target(start, dy, shown.len());
        if to == now {
            return;
        }
        let at = |ws: &Workspace, id: &str| ws.live.iter().position(|l| l.id == id);
        if let (Some(from), Some(to)) = (at(self, id), at(self, &shown[to])) {
            move_to(&mut self.live, from, to);
        }
    }

    /// The sidebar: header, the session list and the usage footer, sliding in and out from the left.
    pub fn render_sidebar(&self) -> El {
        let visible = self.store.data.layout.sidebar_visible;
        let width = self.left_width(self.window_size.0);
        let spin = self.spin_visible();
        let items = self.list_items();
        let mut list = col().key("sidebar-list").w_full().gap(LIST_GAP).pb(8.0);
        for item in items {
            list = list.child(self.list_item(item, spin));
        }
        let scroll = col().key("sidebar-scroll").w_full().grow(1.0).shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list);
        let panel = col()
            .key("sidebar-panel")
            .w(width)
            .h_full()
            .radius(theme::PANEL_RADIUS)
            .clip_rounded(theme::PANEL_RADIUS)
            .border(1.0, rgb(theme::PANEL_BORDER))
            .fill(rgb(theme::SIDEBAR_BG))
            .child(self.sidebar_header())
            .child(scroll)
            .child(self.sidebar_footer());
        // The right edge is the drag handle: it fills the gutter with a small visible grip.
        let handle = row()
            .key("left-resize")
            .pin(None, Some(0.0), Some(-(theme::PANEL_GAP / 2.0 + 3.0)), Some(0.0))
            .w(6.0)
            .center()
            .cursor(Cursor::ResizeColumn)
            .hover_group()
            .on_drag(|ws: &mut Workspace, d: &mut rune::Drag, tx: &mut Tx| {
                match d.phase {
                    Phase::Down => ws.left_drag = Some(SideDrag { size: ws.left_width(ws.window_size.0) }),
                    Phase::Move => ws.drag_left(d.dx),
                    Phase::Up => ws.left_drag = None,
                }
                ws.after_input(tx);
            })
            .child(el().w(2.0).h(28.0).radius(1.0).fill(rgb(0x2b2b30)).group_color(rgb(0x4a4a52)));
        let mut outer = row()
            .key("sidebar")
            .h_full()
            .w(if visible { width + theme::PANEL_GAP } else { 0.0 })
            .clip_rounded(0.0)
            .motion(motion::GLIDE)
            .child(col().key("sidebar-inner").w(width).h_full().child(panel).child(handle).opacity(if visible { 1.0 } else { 0.0 }).motion(motion::GLIDE));
        if self.left_drag.is_some() {
            outer = outer.instant_changes();
        }
        outer
    }

    fn list_item(&self, item: Item, spin: bool) -> El {
        match item {
            Item::Header(label, count) => section_header(label, count),
            Item::Note(t) => row().key(format!("note-{t}")).h(NOTE_H).center_y().px(14.0).child(label(t, 12.0, theme::TEXT_FAINT)),
            Item::Spacer => el().key("spacer").h(SPACER_H),
            Item::Session { row, reorder } => self.session_row(row, reorder, spin),
            Item::Older { count, open } => row()
                .key("older-header")
                .h(HEADER_H)
                .center_y()
                .gap(5.0)
                .px(14.0)
                .cursor(Cursor::Pointer)
                .role("button")
                .on_press_tx(|ws: &mut Workspace, tx: &mut Tx| act(ws, Act::ToggleOlder, tx))
                .child(icon(&icons::CHEVRON_RIGHT, 12.0, theme::TEXT_FAINT).rotate(if open { 90.0 } else { 0.0 }).motion(motion::SNAP))
                .child(section_label("OLDER"))
                .child(label(count.to_string(), 10.5, theme::TEXT_FAINT)),
        }
    }

    fn session_row(&self, row_: Row, reorder: bool, spin: bool) -> El {
        let key = format!("row-{}-{}", row_.scope, row_.id);
        let dragging = self.dragging.as_ref().filter(|d| d.id == row_.id);
        let right_text = if row_.archived { "archived".to_string() } else if row_.stale { "stale".to_string() } else { row_.right.clone() };
        let right_color = if row_.stale { theme::WARN } else { theme::TEXT_FAINT };
        let title_color = if row_.dim { theme::TEXT_DIM } else { theme::TEXT };
        let dot = status_icon(&format!("dot-{}-{}", row_.scope, row_.id), row_.dot, spin);
        let title = text(row_.title.clone()).font(font(13.0, 400.0)).color(rgb(title_color)).truncate().grow(1.0).shrink();
        // The × or archive button sits over the row's right end, shown while the pointer is on the row; what is
        // there otherwise (the age) fades out under it.
        let (corner_icon, corner_act) = if row_.running {
            (&icons::X, Act::CloseLive(row_.id.clone()))
        } else if row_.archived {
            (&icons::ARCHIVE_RESTORE, Act::Archive(row_.id.clone(), false))
        } else {
            (&icons::ARCHIVE, Act::Archive(row_.id.clone(), true))
        };
        let corner = icon_button(corner_icon)
            .key(format!("{key}-corner"))
            .group_opacity(0.0, 1.0)
            .help(if row_.running { "End session" } else if row_.archived { "Unarchive" } else { "Archive" })
            .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, corner_act.clone(), tx));
        let right = label(right_text, 11.0, right_color).group_opacity(1.0, 0.0).max_w(96.0).truncate();
        let content = if row_.running {
            // Project and age, then the title with the status icon, then the branch.
            let mut c = col()
                .grow(1.0)
                .shrink()
                .gap(2.0)
                .child(row().w_full().gap(6.0).child(label(row_.project.clone(), 11.0, theme::TEXT_FAINT).truncate().grow(1.0).shrink()).child(right))
                .child(row().w_full().center_y().gap(7.0).child(title).child(dot));
            if let Some(b) = row_.branch.clone() {
                c = c.child(row().gap(4.0).center_y().child(icon(&icons::GIT_BRANCH, 11.0, theme::TEXT_FAINT)).child(label(b, 11.0, theme::TEXT_FAINT).truncate().shrink()));
            }
            c
        } else {
            row().grow(1.0).shrink().center_y().gap(7.0).child(dot).child(title).child(right.mr(22.0))
        };
        let corner_slot = row().pin(None, Some(0.0), Some(4.0), if row_.running { None } else { Some(0.0) }).center_y().mt(if row_.running { 4.0 } else { 0.0 }).child(corner);
        let mut r = row()
            .key(key)
            .h(if row_.running { RUNNING_ROW_H } else { ROW_H })
            .center_y()
            .gap(7.0)
            .pl(10.0)
            .pr(6.0)
            .mx(5.0)
            .radius(6.0)
            .cursor(Cursor::Pointer)
            .role("button")
            .hover_group()
            .hover(Color::WHITE.alpha(0.035))
            .motion(motion::REFLOW)
            .child(content)
            .child(corner_slot);
        if row_.active {
            r = r.fill(rgb(theme::ROW_ACTIVE));
        }
        let menu_id = row_.id.clone();
        r = r.on_secondary_press(move |ws: &mut Workspace, p: rune::PointerPress, tx: &mut Tx| {
            if let Some(rec) = ws.store.session(&menu_id).cloned() {
                let running = ws.is_running(&menu_id);
                let items = ws.session_menu(&rec, running);
                ws.open_menu(MenuAnchor::Point(p.x, p.y), 0.0, items);
            }
            ws.after_input(tx);
        });
        if !reorder {
            let id = row_.id.clone();
            return r.on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, Act::Select(id.clone()), tx));
        }
        if let Some(d) = dragging {
            // Lifted: it follows the pointer while the others make room.
            r = r.fill(rgb(theme::ROW_ACTIVE)).border(1.0, rgb(theme::TEXT_FAINT)).offset(0.0, d.dy).shadow(6.0, 18.0, Color::BLACK.alpha(0.45)).raise();
        }
        let id = row_.id.clone();
        r.pointer_buttons([PointerButton::Primary, PointerButton::Middle]).on_drag(move |ws: &mut Workspace, d: &mut rune::Drag, tx: &mut Tx| {
            let middle = tx.button() == PointerButton::Middle;
            match d.phase {
                Phase::Down if !middle => {
                    d.data = ws.shown_running().iter().position(|s| *s == id).unwrap_or(0) as f32;
                }
                Phase::Move if !middle && d.dy.abs() > 4.0 => {
                    ws.drag_row(&id, d.data as usize, d.dy);
                    // The row is drawn at its new slot; the offset keeps it under the pointer.
                    let shown = ws.shown_running();
                    let slot = shown.iter().position(|s| *s == id).unwrap_or(0) as f32;
                    ws.dragging = Some(crate::app::RowDrag { id: id.clone(), dy: d.dy - (slot - d.data) * ROW_PITCH });
                }
                Phase::Up => {
                    let was_drag = ws.dragging.take().is_some();
                    if middle {
                        // A middle click ends a running session, like its ×.
                        ws.act(Act::CloseLive(id.clone()));
                    } else if !was_drag && !d.cancelled() {
                        ws.act(Act::Select(id.clone()));
                    }
                }
                _ => {}
            }
            ws.after_input(tx);
        })
    }

    /// "+ New" with no project chosen: pick where it starts.
    fn new_session_menu(&self) -> Vec<MenuItem> {
        let mut items = vec![MenuItem { disabled: true, ..MenuItem::new("Start the session in…", Act::Dismiss) }];
        items.extend(self.store.data.projects.iter().map(|p| MenuItem {
            detail: Some(crate::panels::widgets::short_path(&p.path)),
            icon: Some(&icons::FOLDER),
            ..MenuItem::new(p.name.clone(), Act::NewClaude { cwd: p.path.clone() })
        }));
        items.push(MenuItem { separator_before: true, icon: Some(&icons::PLUS), ..MenuItem::new("Add project…", Act::ImportProject) });
        items
    }

    fn sidebar_header(&self) -> El {
        // With a project chosen a new session starts there (and the button says so); with "All projects" it asks.
        let filter = self.project_filter().map(|p| p.name.clone());
        let hints = self.shortcuts_shown;
        let label_text = match filter {
            Some(name) => format!("New in {}", if name.chars().count() > 12 { format!("{}…", name.chars().take(11).collect::<String>()) } else { name }),
            None => "New".to_string(),
        };
        let mut title = row()
            .center_y()
            .gap(8.0)
            .px(2.0)
            .child(clawd(24.0))
            .child(text("CLAUDIU").font(font(13.0, 700.0).tracking(0.6)).color(rgb(theme::TEXT)).grow(1.0));
        if hints {
            title = title.child(key_hint(",").key("hint-settings"));
        }
        title = title.child(icon_button(&icons::SETTINGS).key("settings").help("Settings").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| act(ws, Act::OpenSettings, tx)));
        let new = crate::panels::widgets::button_leading(&icons::PLUS, label_text, true).key("new-claude").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
            match ws.project_filter().map(|p| p.path.clone()) {
                Some(cwd) => ws.act(Act::NewClaude { cwd }),
                None if ws.store.data.projects.is_empty() => {
                    let cwd = ws.active_cwd();
                    ws.act(Act::NewClaude { cwd })
                }
                None => {
                    let items = ws.new_session_menu();
                    ws.open_menu(MenuAnchor::Key("new-claude".into()), 240.0, items);
                }
            }
            ws.after_input(tx);
        });
        let mut new_row = row().center_y().gap(6.0).child(new);
        if hints {
            new_row = new_row.child(key_hint("Shift+N").key("hint-new"));
        }
        let search_field = text_input(&self.search, font(12.5, 400.0))
            .key("search")
            .single_line()
            .placeholder("Search sessions and projects", rgb(theme::TEXT_FAINT))
            .color(rgb(theme::TEXT))
            .caret(rgb(theme::ACCENT), rgb(theme::SELECTION))
            .grow(1.0)
            .shrink()
            .on_edit(|ws: &mut Workspace| &mut ws.search)
            .on_submit(|ws: &mut Workspace, tx: &mut Tx| {
                ws.focus_active();
                ws.after_input(tx);
            })
            .on_input_key(|ws: &mut Workspace, k: &rune::KeyEvent, tx: &mut Tx| {
                // Escape empties the search, then gives the keyboard back to the session.
                if k.key != rune::Key::Escape {
                    return false;
                }
                if ws.search.text.is_empty() {
                    ws.focus_active();
                } else {
                    ws.search.clear();
                }
                ws.after_input(tx);
                true
            });
        let mut search = row()
            .key("search-box")
            .h(26.0)
            .px(8.0)
            .gap(6.0)
            .center_y()
            .radius(6.0)
            .fill(rgb(0x141414))
            .border(1.0, rgb(if self.search.focused { 0x3a3a40 } else { 0x1f1f22 }))
            .motion(motion::SNAP)
            .child(icon(&icons::SEARCH, 13.0, theme::TEXT_FAINT))
            .child(search_field);
        if hints {
            search = search.child(key_hint("Shift+F").key("hint-search"));
        }
        col().gap(8.0).px(8.0).pt(8.0).pb(8.0).child(title).child(new_row).child(self.project_filter_chip()).child(search)
    }

    /// The project filter: a chip that lists the projects, and (with a project chosen) a ⋯ button for its actions.
    fn project_filter_chip(&self) -> El {
        let current = self.project_filter().cloned();
        let name = current.as_ref().map_or("All projects".to_string(), |p| p.name.clone());
        let chip = row()
            .key("project-filter")
            .grow(1.0)
            .shrink()
            .h(24.0)
            .px(6.0)
            .gap(6.0)
            .center_y()
            .radius(6.0)
            .fill(rgb(0x141414))
            .hover(Color::WHITE.alpha(0.03))
            .cursor(Cursor::Pointer)
            .role("button")
            .child(icon(&icons::FOLDER_OPEN, 13.0, if current.is_some() { theme::TEXT } else { theme::TEXT_DIM }))
            .child(label(name, 12.0, if current.is_some() { theme::TEXT } else { theme::TEXT_DIM }).truncate().grow(1.0).shrink())
            .child(icon(&icons::CHEVRON_DOWN, 12.0, theme::TEXT_FAINT))
            .on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                let chosen = ws.project_filter().map(|p| p.id.clone());
                let mut items = vec![MenuItem::new(format!("{}All projects", tick(chosen.is_none())), Act::FilterProject(None))];
                items.extend(ws.store.data.projects.iter().map(|p| MenuItem::new(format!("{}{}", tick(chosen.as_ref() == Some(&p.id)), p.name), Act::FilterProject(Some(p.id.clone())))));
                items.push(MenuItem { separator_before: true, ..MenuItem::new("Add project…", Act::ImportProject) });
                ws.open_menu(MenuAnchor::Key("project-filter".into()), 0.0, items);
                ws.after_input(tx);
            });
        let mut r = row().center_y().gap(4.0).child(chip);
        if let Some(p) = current {
            r = r.child(icon_button(&icons::ELLIPSIS).key("project-more").help("Project actions").on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                let items = ws.project_menu(&p);
                ws.open_menu(MenuAnchor::Key("project-more".into()), 220.0, items);
                ws.after_input(tx);
            }));
        }
        r
    }

    /// Switch account, add one, or sign in again / remove the active alt (shown greyed out for the main account).
    fn account_menu(&self) -> Vec<MenuItem> {
        let active = self.store.active_account().map(|a| a.id.clone());
        let accounts = &self.store.data.accounts;
        let mut items: Vec<MenuItem> = accounts
            .iter()
            .map(|a| {
                let mark = tick(active.as_ref() == Some(&a.id));
                MenuItem { hint: Some(account_hint(a)), ..MenuItem::new(format!("{mark}{}", a.name()), Act::SwitchAccount(a.id.clone())) }
            })
            .collect();
        items.push(MenuItem { separator_before: true, ..MenuItem::new("Add account…", Act::AddAccount) });
        let id = active.unwrap_or_default();
        let main = self.store.active_account().is_none_or(|a| a.is_main());
        items.push(MenuItem { disabled: main, ..MenuItem::new("Sign in to this account again…", Act::SignIn(id.clone())) });
        items.push(MenuItem { disabled: main, danger: true, ..MenuItem::new("Remove this account…", Act::RemoveAccount(id)) });
        items
    }

    fn account_switcher(&self) -> El {
        let acct = self.store.active_account();
        let name = acct.map_or_else(|| "Main account".to_string(), |a| a.name());
        // `Pro · Alt`, so the row says which kind of account is in use.
        let plan = acct.map(account_hint).unwrap_or_default();
        row()
            .key("account-switcher")
            .h(26.0)
            .mx(-5.0)
            .px(5.0)
            .gap(8.0)
            .center_y()
            .radius(6.0)
            .cursor(Cursor::Pointer)
            .role("button")
            .hover(Color::WHITE.alpha(0.035))
            .on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                let items = ws.account_menu();
                ws.open_menu(MenuAnchor::Key("account-switcher".into()), 0.0, items);
                ws.after_input(tx);
            })
            .child(avatar(&name, 18.0))
            .child(label(name, 12.5, theme::TEXT).truncate().grow(1.0).shrink())
            .child(label(plan, 10.5, theme::TEXT_FAINT))
            .child(icon(&icons::CHEVRON_DOWN, 12.0, theme::TEXT_FAINT))
    }

    fn sidebar_footer(&self) -> El {
        let t = now();
        let usage = self.store.active_account().map(|a| a.usage.clone()).unwrap_or_default();
        // Captions are short: two meters share a row.
        let limit = |key: &str, label: &str, l: &Option<crate::store::RateLimit>| -> El {
            match l {
                Some(l) => match (l.resets_at, l.utilization) {
                    (Some(reset), util) if reset > t => {
                        let mut caption = format!("resets {}", countdown(reset - t));
                        if t - l.seen_at > 900 {
                            caption.push_str(&format!(" · {} ago", ago(l.seen_at, t)));
                        }
                        meter(key, label, util, caption, None)
                    }
                    (Some(_), _) => meter(key, label, None, "reset · unknown".into(), None),
                    (None, util) => meter(key, label, util, format!("seen {} ago", ago(l.seen_at, t)), None),
                },
                None => meter(key, label, None, "after first message".into(), None),
            }
        };
        col()
            .gap(8.0)
            .px(14.0)
            .pt(6.0)
            .pb(8.0)
            .border_sides([1.0, 0.0, 0.0, 0.0], rgb(theme::BORDER))
            .child(self.account_switcher())
            .child(
                row()
                    .gap(12.0)
                    .child(limit("meter-5h", "5 HOUR", &usage.five_hour).grow(1.0).shrink())
                    .child(limit("meter-7d", "7 DAY", &usage.seven_day).grow(1.0).shrink()),
            )
    }
}

fn section_header(label_text: &str, count: Option<String>) -> El {
    let mut left = row().center_y().gap(6.0).child(section_label(label_text.to_string()));
    if let Some(c) = count {
        // The count rolls to its new value.
        left = left.child(label(c, 10.5, theme::TEXT_FAINT).key(format!("count-{label_text}")).transition(Transition::opacity()));
    }
    row().key(format!("header-{label_text}")).h(HEADER_H).center_y().justify_between().pl(14.0).pr(10.0).child(left)
}

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
    use super::{Item, ROW_H, RUNNING_ROW_H, Row, SessionDot, drag_target, is_reorder, move_to};

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
    fn a_dragged_row_passes_a_neighbour_halfway_over_it_and_stays_in_the_list() {
        let pitch = RUNNING_ROW_H + super::LIST_GAP;
        assert_eq!(drag_target(1, pitch * 0.4, 4), 1, "not yet halfway");
        assert_eq!(drag_target(1, pitch * 0.6, 4), 2);
        assert_eq!(drag_target(1, -pitch * 0.6, 4), 0);
        assert_eq!(drag_target(1, pitch * 9.0, 4), 3, "past the end lands last");
        assert_eq!(drag_target(0, -pitch * 9.0, 4), 0);
    }
}
