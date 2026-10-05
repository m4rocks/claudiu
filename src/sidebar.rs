//! Left sidebar: CURRENT SESSION, PROJECTS, other sessions, and the usage meters.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, pulsating_between, px,
};

use crate::app::{Act, Activity, MenuItem, Workspace};
use crate::store::{ExternalState, Id, Project, SessionKind, SessionRecord, now};
use crate::theme::{self, hsla};
use crate::widgets::{ago, button, clawd, countdown, divider, icon_button, meter, section_label};

const SIDEBAR_W: f32 = 284.0;
const PROJECT_SESSION_LIMIT: usize = 5;

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

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Show in Explorer" }
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
        let item = |label: &str, act: Act, danger: bool, sep: bool| MenuItem { label: label.into(), act, danger, separator_before: sep };
        if running {
            items.push(item("Switch to session", Act::Select(rec.id.clone()), false, false));
        } else if rec.kind == SessionKind::Claude {
            items.push(item("Resume session", Act::Resume(rec.id.clone()), false, false));
        } else {
            items.push(item("Open new shell here", Act::NewShell { cwd: rec.cwd.clone() }, false, false));
        }
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

    fn project_menu(&self, p: &Project) -> Vec<MenuItem> {
        let item = |label: String, act: Act, danger: bool, sep: bool| MenuItem { label, act, danger, separator_before: sep };
        let mut items = vec![
            item("New Claude session here".into(), Act::NewClaude { cwd: p.path.clone() }, false, false),
            item("New shell here".into(), Act::NewShell { cwd: p.path.clone() }, false, false),
        ];
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

    pub fn render_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let q = self.search.trim().to_lowercase();
        let searching = !q.is_empty();

        // ---- CURRENT SESSION
        let current: Vec<Row> = self
            .live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
            .filter_map(|l| self.store.session(&l.id))
            .filter(|r| self.matches(&q, r))
            .map(|r| self.row_for(r, true, true))
            .collect();
        let current_total = self.running_count(cx);

        let mut body = div().flex().flex_col().gap(px(2.0));

        body = body.child(self.section_header("CURRENT SESSION", Some(current_total.to_string()), None::<Stateful<Div>>));
        if current.is_empty() {
            body = body.child(
                div().px(px(18.0)).py(px(6.0)).text_size(px(12.0)).text_color(hsla(theme::TEXT_FAINT)).child(if searching {
                    "No running sessions match"
                } else {
                    "Nothing running. Start a Claude session or a shell."
                }),
            );
        }
        for row in current {
            body = body.child(self.render_row(row, cx));
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
            body = body.child(self.render_project_header(&p, sessions.len(), expanded, cx));
            if expanded {
                let limit = if searching || self.show_all.contains(&p.id) { usize::MAX } else { PROJECT_SESSION_LIMIT };
                let hidden_count = matching.len().saturating_sub(limit);
                for rec in matching.iter().take(limit) {
                    let running = self.is_running(&rec.id, cx);
                    body = body.child(self.render_row(self.row_for(rec, running, false), cx));
                }
                if hidden_count > 0 {
                    let pid = p.id.clone();
                    body = body.child(
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
                    body = body.child(div().ml(px(32.0)).py(px(3.0)).text_size(px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child("No sessions yet"));
                }
            }
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
                    body = body.child(self.render_row(row, cx));
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
            .child(self.render_sidebar_header(window, cx))
            .child(div().id("sidebar-scroll").flex_1().min_h_0().overflow_y_scroll().pb(px(10.0)).child(body))
            .child(self.render_footer())
    }

    fn render_sidebar_header(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cwd = self.active_cwd();
        let (cwd_a, cwd_b) = (cwd.clone(), cwd);
        let search_focused = self.search_focus.is_focused(window);
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(12.0))
            .pt(px(14.0))
            .pb(px(12.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(4.0))
                    .child(clawd(24.0))
                    .child(div().text_size(px(13.0)).font_weight(FontWeight::BOLD).text_color(hsla(theme::TEXT)).child("CLAUDIU")),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        button("new-claude", "+ Claude", true)
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::NewClaude { cwd: cwd_a.clone() }, window, cx))),
                    )
                    .child(
                        button("new-shell", "+ Shell", false)
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::NewShell { cwd: cwd_b.clone() }, window, cx))),
                    ),
            )
            .child(
                div()
                    .id("search")
                    .track_focus(&self.search_focus)
                    .h(px(28.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(6.0))
                    .bg(hsla(0x161616))
                    .border_1()
                    .border_color(hsla(if search_focused { 0x3a2a24 } else { 0x1c1c1c }))
                    .cursor_text()
                    .text_size(px(12.5))
                    .on_key_down(cx.listener(|ws, ev: &gpui::KeyDownEvent, window, cx| {
                        let ks = &ev.keystroke;
                        match ks.key.as_str() {
                            "escape" => {
                                ws.search.clear();
                                ws.focus_active(window, cx);
                            }
                            "enter" => ws.focus_active(window, cx),
                            _ => {
                                crate::widgets::edit_text(&mut ws.search, ks, cx);
                            }
                        }
                        cx.notify();
                    }))
                    .on_click(cx.listener(|ws, _: &ClickEvent, window, _cx| window.focus(&ws.search_focus)))
                    .child(div().text_color(hsla(theme::TEXT_FAINT)).child("⌕"))
                    .child(if self.search.is_empty() {
                        div().text_color(hsla(theme::TEXT_FAINT)).child("Search sessions and projects")
                    } else {
                        div().text_color(hsla(theme::TEXT)).child(self.search.clone())
                    })
                    .when(search_focused, |d| d.child(div().w(px(1.0)).h(px(14.0)).bg(hsla(theme::ACCENT)))),
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

    fn render_footer(&self) -> impl IntoElement + use<> {
        let t = now();
        let acct = &self.store.data.account;

        let limit = |label: &str, l: &Option<crate::store::RateLimit>| -> Div {
            match l {
                Some(l) => match (l.resets_at, l.utilization) {
                    (Some(reset), util) if reset > t => {
                        let age = t - l.seen_at;
                        let mut caption = format!("resets in {}", countdown(reset - t));
                        if age > 900 {
                            caption.push_str(&format!(" · seen {} ago", ago(l.seen_at, t)));
                        }
                        meter(label, util, caption, None)
                    }
                    (Some(_), _) => meter(label, None, "window has reset · usage unknown".into(), None),
                    (None, util) => meter(label, util, format!("seen {} ago", ago(l.seen_at, t)), None),
                },
                None => meter(label, None, "appears after your first message".into(), None),
            }
        };

        let (ctx_fraction, ctx_caption) = match self.selected_context() {
            Some((f, tokens, true)) => (Some(f), format!("{}k tokens", tokens / 1000)),
            Some((f, tokens, false)) => (Some(f), format!("~{}k tokens (estimate)", tokens / 1000)),
            None => (None, "no Claude session selected".to_string()),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .px(px(18.0))
            .pt(px(12.0))
            .pb(px(14.0))
            .border_t_1()
            .border_color(hsla(theme::BORDER))
            .child(div().flex().flex_col().gap(px(10.0)).child(div().text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(theme::TEXT_DIM)).child("ACCOUNT"))
                .child(limit("5 HOUR", &acct.five_hour))
                .child(limit("7 DAY", &acct.seven_day)))
            .child(divider())
            .child(div().flex().flex_col().gap(px(10.0)).child(div().text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(hsla(theme::TEXT_DIM)).child("SESSION"))
                .child(meter("CONTEXT", ctx_fraction, ctx_caption, None)))
    }
}
