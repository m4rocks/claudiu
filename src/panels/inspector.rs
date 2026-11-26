//! The right sidebar: the shown session's actions and status, or (one click away) the project's GitHub view.
//! It sits under the top bar so the window buttons stay in the corner.

use std::path::PathBuf;

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use std::time::Duration;

use gpui::{Animation, AnimationExt};

use crate::app::{Act, GitOp, MenuItem, SideDrag, Workspace};
use crate::panels::sidebar::{DragGhost, Slide, is_reorder, lifted, move_to, slid};
use crate::panels::widgets::Trigger;
use crate::store::SideCard;
use gpui::{AppContext, DragMoveEvent, prelude::FluentBuilder};
use crate::platform::Editor;
use crate::github::model::State;
use crate::store::{Attached, Id, Project, SessionKind, SessionRecord};
use crate::panels::theme::{self, theme};
use crate::app::SubagentState;
use crate::panels::logos;
use crate::panels::widgets::{icon, subagent_state_icon};
use gpui_kit::assets::IconName;
use crate::panels::ui::button::icon_button;
use crate::panels::ui::label::section_label;
use crate::panels::ui::progress::meter;

/// What the top bar and the right sidebar describe: the shown session and the folder and project it is in.
pub struct Shown {
    pub rec: Option<SessionRecord>,
    pub cwd: Option<PathBuf>,
    pub project: Option<Project>,
}

impl Workspace {
    pub fn shown(&self) -> Shown {
        let rec = self.active_id().and_then(|id| self.store.session(id)).cloned();
        let project = rec
            .as_ref()
            .and_then(|r| r.project_id.clone())
            .and_then(|p| self.store.project(&p))
            .cloned()
            .or_else(|| rec.is_none().then(|| self.home_selected_project().cloned()).flatten());
        let cwd = rec
            .as_ref()
            .map(|r| r.cwd.clone())
            .or_else(|| project.is_some().then(|| self.home_target_cwd()));
        Shown { rec, cwd, project }
    }

    pub fn render_right(&mut self, window_w: f32, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.right_reveal.step(!self.store.data.layout.right_hidden && !self.editing(), window);
        if open <= 0.0 {
            return None;
        }
        // The view the state asks for; the one drawn changes only after the old one has faded out.
        let desired = match self.right_gh.as_ref().filter(|id| self.gh_pane(id).is_some()) {
            Some(id) => format!("gh:{id}"),
            None => "menu".to_string(),
        };
        const OUT_MS: f32 = 100.0;
        const IN_MS: f32 = 140.0;
        let mut fade = 1.0;
        if self.right_view != desired {
            let t0 = *self.right_out.get_or_insert_with(std::time::Instant::now);
            let e = t0.elapsed().as_secs_f32() * 1000.0;
            if e >= OUT_MS || open < 1.0 {
                self.right_view = desired.clone();
                self.right_out = None;
                self.right_in = std::time::Instant::now();
            } else {
                fade = 1.0 - e / OUT_MS;
                window.request_animation_frame();
            }
        }
        if self.right_view == desired {
            // Fade in once the width has finished moving.
            let e = self.right_in.elapsed().as_secs_f32() * 1000.0 - crate::panels::widgets::Reveal::MS;
            if e < IN_MS {
                fade = (e / IN_MS).max(0.0);
                window.request_animation_frame();
            }
        }
        let shown_view = self.right_view.clone();
        let target = self.right_width_of(shown_view != "menu", window_w);
        if self.right_drag.is_some() {
            self.right_w.snap(target);
        }
        let width = self.right_w.to(target, window);
        let github = shown_view
            .strip_prefix("gh:")
            .and_then(|id| self.gh_pane(id))
            .map(|p| (p.view.clone(), p.project.clone()));
        let body = match github {
            Some((view, project)) => {
                view.update(cx, |v, _| v.width = target);
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .flex_none()
                            .h(px(32.0))
                            .px(px(6.0))
                            .border_b_1()
                            .border_color(theme().border)
                            .child(
                                icon_button("right-back", IconName::ArrowLeft)
                                    .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                                        ws.right_gh = None;
                                        ws.focus_active(window, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(div().text_size(crate::panels::scale::px(12.5)).font_weight(FontWeight::MEDIUM).child(format!("GitHub · {project}"))),
                    )
                    .child(div().flex_1().min_h_0().child(view))
                    .into_any_element()
            }
            None => self.render_right_menu(window, cx).into_any_element(),
        };
        // A different view (menu, GitHub project) fades in.
        let body = div().flex_none().w(px(target)).h_full().opacity(fade).child(body);
        // Both views can be resized by their left edge; a small grip sits in the gutter.
        let handle_center = theme::PANEL_GAP / 2.0 + 3.0;
        let handle = Some(
            div()
                .id("right-resize")
                .group("right-resize")
                .absolute()
                .top_0()
                .left(px(-handle_center))
                .w(px(6.0))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_col_resize()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                        ws.right_drag = Some(SideDrag { x: f32::from(ev.position.x), width });
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w(px(2.0))
                        .h(px(28.0))
                        .rounded_full()
                        .bg(theme().resize_handle)
                        .group_hover("right-resize", |s| s.bg(theme().resize_handle_hover)),
                ),
        );
        Some(
            div()
                .flex()
                .flex_none()
                .w(px((width + theme::PANEL_GAP) * open))
                .h_full()
                .overflow_hidden()
                .justify_end()
                .child(div().flex_none().w(px(theme::PANEL_GAP)))
                .child(
            div()
                .id("right-sidebar")
                .relative()
                .flex_none()
                .w(px(width))
                .h_full()
                // The menu's cards sit on the window's background; GitHub is one view, so it keeps the panel around it.
                .child(div().size_full().when(shown_view != "menu", |d| {
                    d.rounded(px(theme::PANEL_RADIUS)).overflow_hidden().border_1().border_color(theme().panel_border).bg(theme().sidebar)
                }).child(body))
                .children(handle),
                )
                .into_any_element(),
        )
    }

    fn render_right_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shown = self.shown();
        let id = self.active_id().cloned();
        let running = id.as_ref().is_some_and(|id| self.is_running(id, cx));
        let actions = self.action_rows(&shown, id.as_ref(), running, cx);
        let attached = id.as_deref().map(|i| self.attached_of(i)).unwrap_or_default();
        let github = (!attached.is_empty()).then(|| (attached.len(), self.attached_rows(id.as_ref(), attached, cx)));
        let links = id.as_deref().map(|i| self.links_of(i)).unwrap_or_default();
        let linked = (!links.is_empty()).then(|| (links.len(), self.linked_rows(id.as_ref(), links, cx)));
        let children = id.as_deref().map(|i| self.subagent_list(i, cx)).unwrap_or_default();
        let subagents = id.as_ref().filter(|_| !children.is_empty()).map(|parent| (children.len(), self.subagent_rows(parent, children, cx)));
        let status = self.status_rows(&shown, id.as_ref(), running, cx);
        if self.dragging_card.is_some() && !cx.has_active_drag() {
            self.dragging_card = None;
        }
        let mut cards: Vec<(SideCard, Div)> = [
            (SideCard::Actions, Some(group("ACTIONS", None, actions))),
            (SideCard::Github, github.map(|(n, g)| group("GITHUB", Some(n), g))),
            (SideCard::Subagents, subagents.map(|(n, s)| group("SUBAGENTS", Some(n), s))),
            (SideCard::Linked, linked.map(|(n, l)| group("LINKED", Some(n), l))),
            (SideCard::Status, Some(group("STATUS", None, status))),
            (SideCard::Empty, Some(card().min_h(px(72.0)).flex_grow_1())),
        ]
        .into_iter()
        .filter_map(|(kind, card)| Some((kind, card?)))
        .collect();
        let order = self.store.data.layout.right_card_order();
        cards.sort_by_key(|(kind, _)| order.iter().position(|k| k == kind));
        let visible_kinds: Vec<SideCard> = cards.iter().map(|(kind, _)| *kind).collect();
        if is_reorder(&self.prev_right_cards, &visible_kinds) {
            self.card_slide_gen += 1;
            let bounds = self.card_bounds.borrow();
            let base_y = self
                .prev_right_cards
                .first()
                .and_then(|k| bounds.get(k))
                .map(|(top, _)| *top)
                .unwrap_or(0.0);
            let mut y = base_y;
            for kind in &visible_kinds {
                let (old_top, height) = bounds.get(kind).copied().unwrap_or((y, 0.0));
                let delta = old_top - y;
                if delta.abs() > 0.5 && self.dragging_card != Some(*kind) {
                    self.card_slides.insert(*kind, Slide { from: delta, epoch: self.card_slide_gen });
                }
                y += height + theme::PANEL_GAP;
            }
        }
        self.prev_right_cards = visible_kinds;
        let cards: Vec<AnyElement> = cards
            .into_iter()
            .map(|(kind, card)| {
                let slide = self.card_slides.get(&kind).copied();
                let card_bounds = self.card_bounds.clone();
                let probe = gpui::canvas(
                    move |bounds, _, _| {
                        card_bounds.borrow_mut().insert(kind, (f32::from(bounds.origin.y), f32::from(bounds.size.height)));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full();
                let el = self.draggable_card(kind, card, cx).relative().child(probe);
                slid(el, &format!("card-{kind:?}"), slide)
            })
            .collect();
        self.menu_scroll
            .wire(div().id("right-menu").size_full(), window)
            .flex()
            .flex_col()
            .gap(px(theme::PANEL_GAP))
            .children(cards)
    }

    /// A card you can drag over the others to move it; the order is saved as it changes.
    fn draggable_card(&self, kind: SideCard, card: Div, cx: &mut Context<Self>) -> Stateful<Div> {
        let ws = cx.entity().downgrade();
        card.id(SharedString::from(format!("right-card-{kind:?}")))
            .on_drag(DragCard(kind), move |_, _, _, cx| {
                ws.update(cx, |ws, cx| {
                    ws.dragging_card = Some(kind);
                    ws.drag_pos = None;
                    cx.notify();
                })
                .ok();
                cx.new(|_| DragGhost)
            })
            .on_drag_move(cx.listener(move |ws, ev: &DragMoveEvent<DragCard>, _w, cx| {
                let from = ev.drag(cx).0;
                if from == kind || !ev.bounds.contains(&ev.event.position) || !ws.pointer_moved(ev.event.position) {
                    return;
                }
                // Cards differ in height, so a short card dragged onto a tall one could still be over it after they
                // swap, and swap back. It moves only once the pointer is past the middle of the card it goes over, in
                // the direction it travels, which the card that moved out of the way never is.
                let order = ws.store.data.layout.right_card_order();
                let at = |card: SideCard| order.iter().position(|k| *k == card);
                let down = at(from) < at(kind);
                let middle = ev.bounds.center().y;
                if (down && ev.event.position.y < middle) || (!down && ev.event.position.y > middle) {
                    return;
                }
                ws.move_right_card(from, kind);
                ws.last_swap = std::time::Instant::now();
                cx.notify();
            }))
            .when(self.dragging_card == Some(kind), lifted)
    }

    /// Put card `from` where card `to` is, and save the order.
    fn move_right_card(&mut self, from: SideCard, to: SideCard) {
        let mut order = self.store.data.layout.right_card_order();
        let at = |card: SideCard| order.iter().position(|k| *k == card);
        if let (Some(from), Some(to)) = (at(from), at(to)) {
            move_to(&mut order, from, to);
            self.store.data.layout.right_cards = order;
            self.store.mark_dirty();
        }
    }

    /// Open in the editor, the folder, git, the shell and the GitHub view.
    fn action_rows(&self, shown: &Shown, id: Option<&Id>, running: bool, cx: &mut Context<Self>) -> Div {
        let claude = shown.rec.as_ref().is_some_and(|r| r.kind == SessionKind::Agent);
        let target = shown.project.as_ref().map(|p| p.path.clone()).or_else(|| shown.cwd.clone());
        let repo = shown.project.as_ref().filter(|p| p.git.is_some()).and(shown.cwd.clone());
        let busy = self.git_busy;

        let mut actions = div().flex().flex_col().gap(px(1.0));
        if let Some(target) = &target {
            // "Open in <editor>" plus a ▾ to pick another installed editor (remembered, see Act::OpenEditor).
            if let Some(editor) = self.preferred_editor() {
                let (t, pick_target) = (target.clone(), target.clone());
                actions = actions.child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            row("open-in", IconName::ExternalLink, format!("Open in {}", editor_name(editor)))
                                .flex_1()
                                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::OpenEditor(editor, t.clone()), window, cx))),
                        )
                        .child(icon_button("open-in-pick", IconName::ChevronDown).dropdown_right(cx, move |ws, at, window, cx| {
                            ws.open_editor_picker(pick_target.clone(), at, window, cx);
                        })),
                );
            }
        }
        let editor_available = self.editor_owner().is_some();
        actions = actions.child(if editor_available {
            row("editor-mode", IconName::FileCode, "Editor mode")
                .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleEditor, window, cx)))
        } else {
            dim_row("editor-mode", IconName::FileCode, "Editor mode")
        });
        if let Some(target) = target.clone() {
            actions = actions.child(
                row("reveal", IconName::FolderOpen, "Open folder")
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Reveal(target.clone()), window, cx))),
            );
        }
        if let Some(repo) = repo.clone() {
            // One git operation at a time: while one runs, both rows are disabled.
            let git_row = |cx: &mut Context<Self>, key: &'static str, glyph: IconName, label: &'static str, doing: &'static str, op: GitOp, make: Box<dyn Fn(PathBuf) -> Act>| {
                if busy.is_some() {
                    dim_row(key, glyph, if busy == Some(op) { doing } else { label })
                } else {
                    let repo = repo.clone();
                    row(key, glyph, label).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(make(repo.clone()), window, cx)))
                }
            };
            // Until it has been read, assume there is work.
            let nothing_to_do = self.pending.as_ref().is_some_and(|(dir, p)| *dir == repo && !p.any());
            let commit = if nothing_to_do && busy.is_none() {
                dim_row("commit-push", IconName::GitCommitHorizontal, "Commit & Push · nothing to commit or push")
            } else {
                git_row(cx, "commit-push", IconName::GitCommitHorizontal, "Commit & Push", "Committing…", GitOp::CommitPush, Box::new(Act::CommitPush))
            };
            actions = actions.child(commit).child(git_row(cx, "pull", IconName::ArrowDownToLine, "Pull", "Pulling…", GitOp::Pull, Box::new(Act::Pull)));
        }
        if let (Some(id), true, true) = (id.cloned(), claude, running) {
            let open = self.shells.get(&id).is_some_and(|p| p.open);
            actions = actions.child(
                row("shell", IconName::Terminal, if open { "Hide shell" } else { "Show shell" })
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleShell(id.clone()), window, cx))),
            );
        } else if id.is_none() {
            let open = self.shells.get(crate::app::HOME_SHELL).is_some_and(|p| p.open);
            actions = actions.child(
                row("shell", IconName::Terminal, if open { "Hide shell" } else { "Show shell" })
                    .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.toggle_shell_here(window, cx))),
            );
        }
        if let Some((id, thread)) = id.and_then(|id| {
            let view = self.live_view(id)?.thread()?;
            view.read(cx).can_find().then(|| (id.clone(), view.clone()))
        }) {
            actions = actions.child(row("find-in-session", IconName::Search, "Find in session")
                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                    ws.show_session(&id, window, cx);
                    thread.update(cx, |view, cx| view.open_find(window, cx));
                })));
        } else if id.is_none() {
            actions = actions.child(dim_row("find-in-session", IconName::Search, "Find in session"));
        }
        let gh_cwd = shown.cwd.clone();
        actions.child(if let Some(cwd) = gh_cwd {
            row("github", IconName::GitPullRequest, "GitHub")
                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::OpenGithub { cwd: cwd.clone() }, window, cx)))
        } else {
            dim_row("github", IconName::GitPullRequest, "GitHub")
        })
    }

    /// Issues and pull requests attached to the session: click to open, × to take off.
    fn attached_rows(&self, id: Option<&Id>, attached: Vec<Attached>, cx: &mut Context<Self>) -> Div {
        let mut rows = div().flex().flex_col().gap(px(1.0));
        for a in attached {
            let (pull, number) = (a.pull, a.number);
            let owner = id.cloned();
            let key = format!("attached-{}-{pull}-{number}", a.repo.as_deref().unwrap_or_default());
            // An item of another repository says which one.
            let name = format!("{}#{number}", a.repo.as_deref().unwrap_or_default());
            let title = match (a.title, a.state) {
                (None, _) => pending_title(&name, &key),
                (Some(t), Some(State::Closed)) => format!("{name}  {t} (closed)").into_any_element(),
                (Some(t), Some(State::Merged)) => format!("{name}  {t} (merged)").into_any_element(),
                (Some(t), _) => format!("{name}  {t}").into_any_element(),
            };
            let (open_repo, detach_repo) = (a.repo.clone(), a.repo);
            rows = rows.child(removable_row(
                cx,
                self.leaving.contains(&key),
                key,
                if pull { IconName::GitPullRequest } else { IconName::CircleDot },
                title,
                move |ws, window, cx| ws.open_attached(open_repo.as_deref(), pull, number, window, cx),
                move |ws| {
                    if let Some(id) = &owner {
                        ws.detach(id, detach_repo.as_deref(), pull, number);
                    }
                },
            ));
        }
        rows
    }

    /// The subagents of `parent`: the sessions it started through m4code, then the harness's own that its chat ran.
    fn subagent_list(&self, parent: &str, cx: &gpui::App) -> Vec<SubagentRow> {
        let mut rows: Vec<SubagentRow> = self
            .subagents_of(parent)
            .into_iter()
            .map(|(child, title)| SubagentRow {
                key: format!("subagent-{child}"),
                harness: self.store.session(&child).and_then(|r| r.agent.as_ref()).map(|m| m.harness).unwrap_or_default(),
                title,
                state: self.subagent_state(&child, cx),
                open: SubagentOpen::Session(child),
            })
            .collect();
        if let Some(view) = self.live_view(parent).and_then(|v| v.thread()) {
            let view = view.read(cx);
            for call in view.native_subagents() {
                let state = SubagentState::of_step(call.status, view.is_working());
                rows.push(SubagentRow {
                    key: format!("native-subagent-{}", call.id),
                    harness: crate::harness::subagents::harness_of_type(call.subagent.as_deref().unwrap_or_default(), view.harness),
                    title: call.title.clone(),
                    state,
                    open: SubagentOpen::Step(call.id.clone()),
                });
            }
        }
        rows
    }

    /// Subagents of `parent`, each with its provider's logo and where it stands: click to look at one. Those that
    /// finished fold into one row under the rest, so a long-running parent's list stays short.
    fn subagent_rows(&self, parent: &Id, children: Vec<SubagentRow>, cx: &mut Context<Self>) -> Div {
        let (finished, active): (Vec<_>, Vec<_>) = children.into_iter().partition(|child| child.state.finished());
        let mut rows = div().flex().flex_col().gap(px(1.0));
        for child in active {
            rows = rows.child(subagent_row(parent, child, cx));
        }
        if finished.is_empty() {
            return rows;
        }
        let open = self.finished_subagents_open.contains(parent);
        let toggle = parent.clone();
        rows = rows.child(
            base_row(format!("subagents-finished-{parent}"), if open { IconName::ChevronDown } else { IconName::ChevronRight }, format!("{} finished", finished.len()))
                .text_color(theme().subtle_foreground)
                .cursor_pointer()
                .hover(|s| s.bg(theme().row_hover).text_color(theme().muted_foreground))
                .on_click(cx.listener(move |ws, _: &ClickEvent, _, cx| {
                    if !ws.finished_subagents_open.remove(&toggle) {
                        ws.finished_subagents_open.insert(toggle.clone());
                    }
                    cx.notify();
                })),
        );
        if open {
            for child in finished {
                rows = rows.child(subagent_row(parent, child, cx));
            }
        }
        rows
    }

    /// Sessions linked with this one: click to go there, × to unlink.
    fn linked_rows(&self, id: Option<&Id>, links: Vec<(Id, String)>, cx: &mut Context<Self>) -> Div {
        let mut rows = div().flex().flex_col().gap(px(1.0));
        for (other, title) in links {
            let (me, go, gone) = (id.cloned(), other.clone(), other.clone());
            let key = format!("linked-{other}");
            rows = rows.child(removable_row(
                cx,
                self.leaving.contains(&key),
                key,
                IconName::Link,
                title,
                move |ws, window, cx| ws.act(Act::Select(go.clone()), window, cx),
                move |ws| {
                    if let Some(me) = &me {
                        ws.unlink(me, &gone);
                    }
                },
            ));
        }
        rows
    }

    /// Context meter across all harnesses, plus Claude Code's session settings when applicable.
    fn status_rows(&self, shown: &Shown, id: Option<&Id>, running: bool, cx: &mut Context<Self>) -> Div {
        let (ctx_fraction, ctx_caption) = match self.selected_context(cx) {
            Some((f, tokens, true, Some(full))) => (Some(f), format!("{}k of {}k tokens", tokens / 1000, full / 1000)),
            Some((f, tokens, true, None)) => (Some(f), format!("{}k tokens", tokens / 1000)),
            Some((f, tokens, false, _)) => (Some(f), format!("~{}k tokens (estimate)", tokens / 1000)),
            None => (None, "no session".to_string()),
        };

        // The figures come from the snapshot the session's mod reports.
        let meta = shown.rec.as_ref().filter(|r| r.kind == SessionKind::Agent).and_then(|r| r.agent.as_ref());
        let (compact, precompute) = meta.map_or((None, None), |m| (m.auto_compact_override, m.precompute_compact));
        let live = id.and_then(|id| self.live_view(id)).and_then(|view| view.thread());
        let effective_compact = live
            .map(|view| view.read(cx).compaction.enabled)
            .or_else(|| meta.map(|_| compact.unwrap_or(self.store.data.settings.auto_compact)));
        let mut compact_text = live.map_or_else(
            || effective_compact.map_or("—", |v| if v { "On" } else { "Off" }).to_string(),
            |view| view.read(cx).compaction_status(ctx_fraction.is_some()),
        );
        if compact.is_none() && effective_compact.is_some()
            && !matches!(compact_text.as_str(), "Unsupported" | "Unavailable" | "Waiting…")
            && live.is_none_or(|view| view.read(cx).harness.adapter().compact_command.is_some())
        {
            compact_text.push_str(" (global)");
        }
        // Where auto-compaction starts, marked on the meter while it would run.
        let compact_at = match live {
            Some(view) => {
                let v = view.read(cx);
                (v.compaction.enabled && !matches!(compact_text.as_str(), "Unsupported" | "Unavailable")).then(|| crate::harness::compaction::clamp_threshold(v.compaction.threshold_percent))
            }
            None => effective_compact.filter(|on| *on).map(|_| crate::harness::compaction::clamp_threshold(self.store.data.settings.auto_compact_threshold)),
        };
        let mark = compact_at.map(|at| (at as f32 / 100.0, format!("compacts at {at}%")));
        let settable = id.filter(|_| meta.is_some() && running).cloned();

        let compact_settable = settable.clone().filter(|_| live.is_some_and(|view| view.read(cx).harness.adapter().compact_command.is_some()));
        let compact_row = match compact_settable {
            Some(sid) => pick_row(cx, "set-auto-compact", "Auto-compact", compact_text, move || auto_compact_menu(&sid, compact)).into_any_element(),
            None => kv("Auto-compact", compact_text).into_any_element(),
        };
        // Only while Claude Code offers the setting; without its feature flag the setting does nothing.
        let precompute_row = precompute.map(|on| {
            let text = if on { "On" } else { "Off" }.to_string();
            match settable.clone() {
                Some(sid) => pick_row(cx, "set-precompute-compact", "Background compaction", text, move || precompute_compact_menu(&sid, on)).into_any_element(),
                None => kv("Background compaction", text).into_any_element(),
            }
        });
        let cache_row = meta.and_then(|m| m.cache_usage.summary()).map(|summary| kv("Prompt cache", summary).into_any_element());
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(8.0))
            .child(meter("CONTEXT", ctx_fraction, ctx_caption, None, mark))
            .child(div().flex().flex_col().gap(px(2.0)).children(cache_row).child(compact_row).children(precompute_row))
    }
}

fn auto_compact_menu(sid: &Id, current: Option<bool>) -> Vec<MenuItem> {
    [(None, "Use global setting"), (Some(true), "On"), (Some(false), "Off")].into_iter().map(|(on, name)| MenuItem { checked: current == on, ..MenuItem::new(name, Act::SetAutoCompact(sid.clone(), on)) }).collect()
}

fn precompute_compact_menu(sid: &Id, current: bool) -> Vec<MenuItem> {
    [(true, "On"), (false, "Off")].into_iter().map(|(on, name)| MenuItem { checked: current == on, ..MenuItem::new(name, Act::SetPrecomputeCompact(sid.clone(), on)) }).collect()
}

/// One subagent in the sidebar: a session m4code started (`spawn_subagent`), or one of the harness's own, which lives
/// in a step of its parent's chat.
struct SubagentRow {
    key: String,
    harness: crate::harness::Harness,
    title: String,
    state: SubagentState,
    open: SubagentOpen,
}

enum SubagentOpen {
    Session(Id),
    /// The parent's tool call that ran it.
    Step(String),
}

/// A subagent's row: a session opens; a native one unfolds its step in the parent's chat and scrolls there.
fn subagent_row(parent: &Id, child: SubagentRow, cx: &mut Context<Workspace>) -> Stateful<Div> {
    let parent = parent.clone();
    let SubagentRow { key, harness, title, state, open } = child;
    lead_row(key, logos::draw(logos::for_harness(harness), 13.0, theme().muted_foreground).into_any_element(), title)
        .text_color(if state.finished() { theme().muted_foreground } else { theme().foreground })
        .cursor_pointer()
        .hover(|s| s.bg(theme().row_hover))
        .child(div().flex_none().child(subagent_state_icon(state, 13.0)))
        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| match &open {
            SubagentOpen::Session(child) => ws.act(Act::Select(child.clone()), window, cx),
            SubagentOpen::Step(call) => {
                if let Some(view) = ws.live_view(&parent).and_then(|v| v.thread()).cloned() {
                    view.update(cx, |view, cx| view.reveal_step(call, cx));
                }
            }
        }))
}

/// Drag payload: a right sidebar card, dragged over another to move it there.
struct DragCard(SideCard);

/// A card of the right sidebar, which sits on the window's background: drawn like the chat's panel.
fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .p(px(4.0))
        .rounded(px(theme::PANEL_RADIUS))
        .border_1()
        .border_color(theme().panel_border)
        .bg(theme().background)
}

/// One section of the sidebar, as a card: its label, the number of entries for a list, then its rows.
fn group(label: &str, count: Option<usize>, body: impl IntoElement) -> Div {
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(8.0))
        .pt(px(4.0))
        .child(section_label(label.to_string()))
        .children(count.map(|n| section_label(n.to_string())));
    card().child(header).child(body)
}

/// How long the grey bar of an item still waiting for its title pulses. Then it rests, because an item GitHub never
/// answers for (no `gh`, signed out, not a GitHub folder) would otherwise keep redrawing the window forever.
const PENDING_PULSE: Duration = Duration::from_secs(10);
/// Pulses in `PENDING_PULSE`, so the bar ends the pulse where it began.
const PENDING_PULSES: f32 = 8.0;

/// The bar's opacity `t` (0 to 1) through `PENDING_PULSE`: full at both ends, dipping to 0.45 in between.
fn pending_opacity(t: f32) -> f32 {
    0.725 + 0.275 * (t * PENDING_PULSES * std::f32::consts::TAU).cos()
}

/// `#12` with a grey bar where the title will be, until GitHub answers.
fn pending_title(name: &str, key: &str) -> AnyElement {
    let bar = div().flex_none().h(px(8.0)).w(px(96.0)).rounded(px(4.0)).bg(theme().muted).with_animation(
        SharedString::from(format!("pending-{key}")),
        Animation::new(PENDING_PULSE).with_easing(pending_opacity),
        |d, opacity| d.opacity(opacity),
    );
    div().flex().items_center().gap(px(8.0)).child(name.to_string()).child(bar).into_any_element()
}

fn base_row(id: impl Into<SharedString>, glyph: IconName, label: impl IntoElement) -> Stateful<Div> {
    lead_row(id, div().text_color(theme().muted_foreground).child(icon(glyph, 14.0)).into_any_element(), label)
}

/// A row that starts with `lead`, such as a provider's logo, instead of an icon.
fn lead_row(id: impl Into<SharedString>, lead: AnyElement, label: impl IntoElement) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(px(26.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(6.0))
        .text_size(crate::panels::scale::px(12.5))
        .child(div().flex_none().child(lead))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().line_height(gpui::relative(1.35)).child(label))
}

fn row(id: impl Into<SharedString>, glyph: IconName, label: impl IntoElement) -> gpui_kit::base::Button {
    crate::panels::ui::button::focusable(id.into())
        .h(px(26.0))
        .px(px(8.0))
        .justify_start()
        .gap(px(8.0))
        .rounded(px(6.0))
        .text_size(crate::panels::scale::px(12.5))
        .text_color(theme().foreground)
        .cursor_pointer()
        .hover(|s| s.bg(theme().row_hover))
        .child(div().flex_none().text_color(theme().muted_foreground).child(icon(glyph, 14.0)))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().line_height(gpui::relative(1.35)).child(label))
}

/// The row as it looks while its action is unavailable: no hover, no pointer (give it no click handler either).
fn dim_row(id: impl Into<SharedString>, glyph: IconName, label: impl IntoElement) -> gpui_kit::base::Button {
    gpui_kit::base::Button::new(id.into())
        .disabled(true)
        .h(px(26.0))
        .px(px(8.0))
        .justify_start()
        .gap(px(8.0))
        .rounded(px(6.0))
        .text_size(crate::panels::scale::px(12.5))
        .text_color(theme().subtle_foreground)
        .cursor_default()
        .child(div().flex_none().text_color(theme().muted_foreground).child(icon(glyph, 14.0)))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().line_height(gpui::relative(1.35)).child(label))
}

/// A list row that does something when clicked and has a × that removes the entry.
/// It grows in when first drawn; × shrinks it away (`leaving`) and removes the entry once it is gone, so the rows below slide.
fn removable_row(
    cx: &mut Context<Workspace>,
    leaving: bool,
    key: String,
    glyph: IconName,
    title: impl IntoElement,
    open: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    remove: impl Fn(&mut Workspace) + 'static,
) -> impl IntoElement {
    use gpui::{Animation, AnimationExt};
    const MS: u64 = 180;
    let x = format!("remove-{key}");
    let remove = std::rc::Rc::new(remove);
    let gone_key = key.clone();
    let anim_key = SharedString::from(format!("{}-{key}", if leaving { "leave" } else { "enter" }));
    let row = row(key, glyph, title)
        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| open(ws, window, cx)))
        .child(icon_button(x, IconName::X).on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
            cx.stop_propagation();
            if !ws.leaving.insert(gone_key.clone()) {
                return;
            }
            let (remove, gone_key) = (remove.clone(), gone_key.clone());
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(MS)).await;
                let _ = this.update(cx, |ws, cx| {
                    ws.leaving.remove(&gone_key);
                    remove(ws);
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
        })));
    div().flex_none().overflow_hidden().child(row).with_animation(
        anim_key,
        Animation::new(std::time::Duration::from_millis(MS)).with_easing(gpui::ease_out_quint()),
        move |d, t| {
            let open = if leaving { 1.0 - t } else { t };
            d.h(px(26.0 * open)).opacity(open)
        },
    )
}

/// A setting row that opens a menu (built when it is clicked): the value with a chevron, lit on hover.
fn pick_row(cx: &mut Context<Workspace>, id: &'static str, label: &str, value: String, menu: impl Fn() -> Vec<MenuItem> + 'static) -> gpui_kit::base::Button {
    crate::panels::ui::button::button(id, None, "", crate::panels::ui::button::Variant::Ghost, crate::panels::ui::button::Size::Xs)
        .w_full().h_auto().mx(px(-6.0)).px(px(6.0))
        .child(crate::panels::ui::item::setting(label, div().flex().items_center().gap(px(4.0))
            .child(value)
            .child(icon(IconName::ChevronDown, 12.0))))
        .dropdown(cx, move |ws, at, window, cx| ws.open_dropdown(at, menu(), window, cx))
}

fn kv(label: &str, value: String) -> Div {
    crate::panels::ui::item::setting(label, value)
}

fn editor_name(editor: Editor) -> &'static str {
    match editor {
        Editor::VsCode => "VS Code",
        Editor::Zed => "Zed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gpui panics when a style is set twice (a hover on a row that already has one), so just building both is the check.
    #[test]
    fn both_row_kinds_can_be_built() {
        row("a", IconName::Terminal, "A");
        dim_row("b", IconName::Terminal, "B");
        crate::panels::ui::button::icon_button("c", IconName::FileCode);
        crate::panels::ui::button::icon_button_disabled("d", IconName::FileCode);
    }

    #[test]
    fn the_pending_bar_pulses_and_rests_at_full_opacity() {
        assert!((pending_opacity(0.0) - 1.0).abs() < 1e-5);
        assert!((pending_opacity(1.0) - 1.0).abs() < 1e-5, "it stays this way once the animation is over");
        assert!((pending_opacity(0.5 / PENDING_PULSES) - 0.45).abs() < 1e-5, "the dip comes mid-pulse");
        assert!((0..=1000).all(|i| (0.45 - 1e-5..=1.0 + 1e-5).contains(&pending_opacity(i as f32 / 1000.0))));
    }

    fn labels(items: &[MenuItem]) -> Vec<(&str, bool)> {
        items.iter().map(|i| (i.label.as_str(), i.checked)).collect()
    }

    #[test]
    fn the_setting_menus_tick_the_current_value() {
        let sid = "s".to_string();
        assert_eq!(labels(&auto_compact_menu(&sid, Some(false))), [("Use global setting", false), ("On", false), ("Off", true)]);
        assert_eq!(labels(&auto_compact_menu(&sid, None)), [("Use global setting", true), ("On", false), ("Off", false)]);
        assert_eq!(labels(&precompute_compact_menu(&sid, true)), [("On", true), ("Off", false)]);
    }
}
