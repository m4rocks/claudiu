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
use crate::panels::widgets::{Trigger, button};
use crate::platform::Editor;
use crate::store::{Attached, Id, Project, SessionKind, SessionRecord};
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{countdown, icon, icon_button, meter, section_label, tick};
use gpui_kit::assets::IconName;

/// What the top bar and the right sidebar describe: the shown session and the folder and project it is in.
pub struct Shown {
    pub rec: Option<SessionRecord>,
    pub cwd: Option<PathBuf>,
    pub project: Option<Project>,
}

impl Workspace {
    pub fn shown(&self) -> Shown {
        let rec = self.active_id().and_then(|id| self.store.session(id)).cloned();
        let project = rec.as_ref().and_then(|r| r.project_id.clone()).and_then(|p| self.store.project(&p)).cloned();
        let cwd = rec.as_ref().map(|r| r.cwd.clone());
        Shown { rec, cwd, project }
    }

    pub fn render_right(&mut self, window_w: f32, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.right_reveal.step(!self.store.data.layout.right_hidden, window);
        if open <= 0.0 {
            return None;
        }
        // The view the state asks for; the one drawn changes only after the old one has faded out.
        let desired = match self.right_gh.as_ref().filter(|id| self.gh_pane(id).is_some()) {
            Some(id) => format!("gh:{id}"),
            None if self.visual_shown() => "visual".to_string(),
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
                            .border_color(hsla(theme::BORDER))
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
            None if shown_view == "visual" => self.render_visual_view(window, cx).into_any_element(),
            None => self.render_right_menu(window, cx).into_any_element(),
        };
        // A different view (menu, GitHub project, Visual) fades in.
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
                        .bg(hsla(0x2b2b30))
                        .group_hover("right-resize", |s| s.bg(hsla(0x4a4a52))),
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
                .child(
                    div()
                        .size_full()
                        .rounded(px(theme::PANEL_RADIUS))
                        .overflow_hidden()
                        .border_1()
                        .border_color(hsla(theme::PANEL_BORDER))
                        .bg(hsla(theme::SIDEBAR_BG))
                        .child(body),
                )
                .children(handle),
                )
                .into_any_element(),
        )
    }

    /// What the model drew for the shown session, newest card first; a card still being written says so.
    fn render_visual_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cards: Vec<crate::visual::Card> = self.visual_cards().iter().rev().cloned().collect();
        let header = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .flex_none()
            .h(px(32.0))
            .px(px(6.0))
            .border_b_1()
            .border_color(hsla(theme::BORDER))
            .child(icon_button("visual-back", IconName::ArrowLeft).on_click(cx.listener(|ws, _: &ClickEvent, _w, cx| ws.hide_visual(false, cx))))
            .child(div().flex_1().text_size(crate::panels::scale::px(12.5)).font_weight(FontWeight::MEDIUM).child(format!("Visual · {}", cards.len())))
            .child(icon_button("visual-clear", IconName::X).on_click(cx.listener(|ws, _: &ClickEvent, _w, cx| ws.hide_visual(true, cx))));
        let mut list = div().flex().flex_col().w_full();
        for (n, card) in cards.iter().enumerate() {
            let mut one = div().flex().flex_col().gap(px(8.0)).p(px(14.0));
            if n > 0 {
                one = one.border_t_1().border_color(hsla(theme::BORDER));
            }
            if !card.title.is_empty() || !card.done {
                let mut head = div().flex().items_center().gap(px(8.0)).text_size(crate::panels::scale::px(12.5));
                head = head.child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).child(card.title.clone()));
                if !card.done {
                    head = head.child(div().flex_none().text_color(hsla(theme::TEXT_FAINT)).child("drawing…"));
                }
                one = one.child(head);
            }
            one = one.child(div().text_size(crate::panels::scale::px(13.0)).child(crate::visual::ui::markdown(format!("visual-{}", card.call), &card.content)));
            list = list.child(crate::panels::widgets::fade_in(one, format!("visual-card-{}", card.call), 220, 0.0));
        }
        div().flex().flex_col().size_full().child(header).child(self.visual_scroll.wire(div().id("visual-scroll").flex_1().min_h_0(), window).child(list))
    }

    fn render_right_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shown = self.shown();
        let id = self.active_id().cloned();
        let running = id.as_ref().is_some_and(|id| self.is_running(id, cx));
        let actions = self.action_rows(&shown, id.as_ref(), running, cx);
        let attached = id.as_deref().map(|i| self.attached_of(i)).unwrap_or_default();
        let github = (!attached.is_empty()).then(|| self.attached_rows(id.as_ref(), attached, cx));
        let links = id.as_deref().map(|i| self.links_of(i)).unwrap_or_default();
        let linked = (!links.is_empty()).then(|| self.linked_rows(id.as_ref(), links, cx));
        let away = id.as_deref().and_then(|i| self.session_away(i, cx));
        let status = self.status_rows(&shown, id.as_ref(), running, cx);
        let drawn = self.visual_cards().len();
        let visual = (drawn > 0).then(|| {
            div().flex().flex_col().gap(px(1.0)).child(removable_row(
                cx,
                self.leaving.contains("visual-open"),
                "visual-open".to_string(),
                IconName::Eye,
                format!("Visual · {drawn}"),
                |ws, _window, cx| ws.show_visual(cx),
                |ws| {
                    if let Some(id) = ws.active_id().cloned() {
                        ws.visuals.remove(&id);
                    }
                },
            ))
        });
        self.menu_scroll
            .wire(div().id("right-menu").size_full(), window)
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(6.0))
            .children(away.map(|(from, to_name)| {
                // Fades in when the session turns out to be on another account.
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .child(div().text_size(crate::panels::scale::px(12.0)).text_color(hsla(theme::WARN)).child(format!("This session runs on {from}.")))
                    .child(button("handoff-session", format!("Hand off to {to_name}"), true).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                        ws.act(Act::Handoff, window, cx)
                    })))
                    .with_animation("handoff-fade", Animation::new(Duration::from_millis(220)).with_easing(gpui::ease_out_quint()), |d, t| d.opacity(t));
                group("ACCOUNT", body)
            }))
            .child(group("ACTIONS", actions))
            .children(visual.map(|v| group("VISUAL", v)))
            .children(github.map(|g| group("GITHUB", g)))
            .children(linked.map(|l| group("LINKED", l)))
            .child(group("STATUS", status))
    }

    /// Open in the editor, the folder, git, the shell and the GitHub view.
    fn action_rows(&self, shown: &Shown, id: Option<&Id>, running: bool, cx: &mut Context<Self>) -> Div {
        let claude = shown.rec.as_ref().is_some_and(|r| r.kind == SessionKind::Claude);
        let target = shown.project.as_ref().map(|p| p.path.clone()).or_else(|| shown.cwd.clone());
        let repo = shown.project.as_ref().filter(|p| p.git.is_some()).and(shown.cwd.clone());
        let busy = self.git_busy;

        let mut actions = div().flex().flex_col().gap(px(1.0));
        if let Some(target) = target {
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
                        .child(icon_button("open-in-pick", IconName::ChevronDown).dropdown(
                            cx,
                            move |ws, at, _w, cx| {
                                let current = ws.preferred_editor();
                                let items = ws
                                    .editors
                                    .iter()
                                    .filter(|(_, found)| *found)
                                    .map(|(e, _)| MenuItem::new(format!("{}{}", tick(current == Some(*e)), e.label()), Act::OpenEditor(*e, pick_target.clone())))
                                    .collect();
                                ws.open_dropdown(at, items, cx);
                            },
                        )),
                );
            }
            actions = actions.child(
                row("reveal", IconName::FolderOpen, "Open folder")
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Reveal(target.clone()), window, cx))),
            );
        }
        if let Some(repo) = repo {
            // One git operation at a time: while one runs, both rows are disabled.
            let git_row = |cx: &mut Context<Self>, key: &'static str, glyph: IconName, label: &'static str, doing: &'static str, op: GitOp, make: Box<dyn Fn(PathBuf) -> Act>| {
                if busy.is_some() {
                    dim_row(key, glyph, if busy == Some(op) { doing } else { label })
                } else {
                    let repo = repo.clone();
                    row(key, glyph, label).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(make(repo.clone()), window, cx)))
                }
            };
            // The commit message is written through the running session's mod, so it needs one.
            let commit = match id.filter(|_| running && claude) {
                Some(id) => {
                    let id = id.clone();
                    git_row(cx, "commit-push", IconName::GitCommitHorizontal, "Commit & Push", "Committing…", GitOp::CommitPush, Box::new(move |cwd| Act::CommitPush(id.clone(), cwd)))
                }
                None => dim_row("commit-push", IconName::GitCommitHorizontal, "Commit & Push · needs a running session"),
            };
            actions = actions.child(commit).child(git_row(cx, "pull", IconName::ArrowDownToLine, "Pull", "Pulling…", GitOp::Pull, Box::new(Act::Pull)));
        }
        if let (Some(id), true, true) = (id.cloned(), claude, running) {
            let open = self.shells.get(&id).is_some_and(|p| p.open);
            actions = actions.child(
                row("shell", IconName::Terminal, if open { "Hide shell" } else { "Show shell" })
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleShell(id.clone()), window, cx))),
            );
        }
        let cwd = self.active_cwd();
        actions.child(
            row("github", IconName::GitPullRequest, "GitHub")
                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::OpenGithub { cwd: cwd.clone() }, window, cx))),
        )
    }

    /// Issues and pull requests attached to the session: click to open, × to take off.
    fn attached_rows(&self, id: Option<&Id>, attached: Vec<Attached>, cx: &mut Context<Self>) -> Div {
        let mut rows = div().flex().flex_col().gap(px(1.0));
        for a in attached {
            let (pull, number) = (a.pull, a.number);
            let owner = id.cloned();
            let title = a.title.map_or_else(|| format!("#{number}"), |t| format!("#{number}  {t}"));
            let key = format!("attached-{pull}-{number}");
            rows = rows.child(removable_row(
                cx,
                self.leaving.contains(&key),
                key,
                if pull { IconName::GitPullRequest } else { IconName::CircleDot },
                title,
                move |ws, window, cx| ws.open_attached(pull, number, window, cx),
                move |ws| {
                    if let Some(id) = &owner {
                        ws.detach(id, pull, number);
                    }
                },
            ));
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
                IconName::Brackets,
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

    /// Context and cache meters, and the two settings Claude Code keeps (dropdowns while the session runs).
    fn status_rows(&self, shown: &Shown, id: Option<&Id>, running: bool, cx: &mut Context<Self>) -> Div {
        let (ctx_fraction, ctx_caption) = match self.selected_context() {
            Some((f, tokens, true, Some(full))) => (Some(f), format!("{}k of {}k tokens", tokens / 1000, full / 1000)),
            Some((f, tokens, true, None)) => (Some(f), format!("{}k tokens", tokens / 1000)),
            Some((f, tokens, false, _)) => (Some(f), format!("~{}k tokens (estimate)", tokens / 1000)),
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

        // The figures come from the snapshot the session's mod reports.
        let meta = shown.rec.as_ref().filter(|r| r.kind == SessionKind::Claude).and_then(|r| r.claude.as_ref());
        let (compact, advisor, active, known) = match meta {
            Some(m) => (m.auto_compact, m.advisor.clone(), m.advisor_active.clone(), true),
            None => (None, None, None, false),
        };
        let advisor = advisor.map(|a| a.to_lowercase());
        let active = active.map(|a| a.to_lowercase());
        let compact_text = compact.map_or("—", |v| if v { "On" } else { "Off" }).to_string();
        let advisor_text = if known { advisor_display(advisor.as_deref(), active.as_deref()) } else { "—".into() };
        let settable = id.filter(|_| meta.is_some() && running).cloned();

        let compact_row = match settable.clone() {
            Some(sid) => pick_row(cx, "set-auto-compact", "Auto-compact", compact_text, move || auto_compact_menu(&sid, compact)).into_any_element(),
            None => kv("Auto-compact", compact_text).into_any_element(),
        };
        let advisor_row = match settable {
            Some(sid) => pick_row(cx, "set-advisor", "Advisor", advisor_text, move || advisor_menu(&sid, advisor.as_deref())).into_any_element(),
            None => kv("Advisor", advisor_text).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(8.0))
            .child(meter("CONTEXT", ctx_fraction, ctx_caption, None))
            .child(cache)
            .child(div().flex().flex_col().gap(px(2.0)).child(compact_row).child(advisor_row))
    }
}

fn auto_compact_menu(sid: &Id, current: Option<bool>) -> Vec<MenuItem> {
    [(true, "On"), (false, "Off")].into_iter().map(|(on, name)| MenuItem::new(format!("{}{name}", tick(current == Some(on))), Act::SetAutoCompact(sid.clone(), on))).collect()
}

/// Off, the two aliases, and "Other…" (ticked for any other model) which asks for a name.
fn advisor_menu(sid: &Id, current: Option<&str>) -> Vec<MenuItem> {
    const ALIASES: [(&str, &str); 2] = [("opus", "Opus"), ("sonnet", "Sonnet")];
    let current = current.and_then(advisor_alias);
    let other = current.is_some_and(|a| !ALIASES.iter().any(|(k, _)| *k == a));
    let mut items = vec![MenuItem::new(format!("{}Off", tick(current.is_none())), Act::SetAdvisor(sid.clone(), "off".into()))];
    items.extend(ALIASES.iter().map(|(alias, name)| MenuItem::new(format!("{}{name}", tick(current == Some(*alias))), Act::SetAdvisor(sid.clone(), (*alias).into()))));
    items.push(MenuItem::new(format!("{}Other…", tick(other)), Act::AskAdvisor(sid.clone())));
    items
}

/// Claude Code may store either an alias (`opus`) or the resolved model id (`claude-opus-5-5`); the menu and the
/// value both speak aliases, so map the id back to the family it belongs to.
fn advisor_alias(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("off") {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    if lower == "opus" || lower.contains("opus") {
        Some("opus")
    } else if lower == "sonnet" || lower.contains("sonnet") {
        Some("sonnet")
    } else {
        Some(value)
    }
}

/// `Opus`, or `Opus → Sonnet` while the saved setting waits for `/clear` or `/compact`.
fn advisor_display(saved: Option<&str>, active: Option<&str>) -> String {
    let Some(saved) = saved.and_then(advisor_alias) else { return "Off".into() };
    let active = active.and_then(advisor_alias).unwrap_or(saved);
    if active == saved { advisor_name(active) } else { format!("{} → {}", advisor_name(active), advisor_name(saved)) }
}

fn advisor_name(alias: &str) -> String {
    match alias {
        "opus" => "Opus".into(),
        "sonnet" => "Sonnet".into(),
        other => other.to_string(),
    }
}

fn group(label: &str, body: impl IntoElement) -> Div {
    div().flex().flex_col().gap(px(4.0)).child(div().px(px(8.0)).child(section_label(label.to_string()))).child(body)
}

fn base_row(id: impl Into<SharedString>, glyph: IconName, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(px(26.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(6.0))
        .text_size(crate::panels::scale::px(12.5))
        .child(div().flex_none().text_color(hsla(theme::TEXT_DIM)).child(icon(glyph, 14.0)))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label.into()))
}

fn row(id: impl Into<SharedString>, glyph: IconName, label: impl Into<SharedString>) -> Stateful<Div> {
    base_row(id, glyph, label).text_color(hsla(theme::TEXT)).cursor_pointer().hover(|s| s.bg(hsla(theme::ROW_HOVER)))
}

/// The row as it looks while its action is unavailable: no hover, no pointer (give it no click handler either).
fn dim_row(id: impl Into<SharedString>, glyph: IconName, label: impl Into<SharedString>) -> Stateful<Div> {
    base_row(id, glyph, label).text_color(hsla(theme::TEXT_FAINT)).cursor_default()
}

/// A list row that does something when clicked and has a × that removes the entry.
/// It grows in when first drawn; × shrinks it away (`leaving`) and removes the entry once it is gone, so the rows below slide.
fn removable_row(
    cx: &mut Context<Workspace>,
    leaving: bool,
    key: String,
    glyph: IconName,
    title: String,
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
fn pick_row(cx: &mut Context<Workspace>, id: &'static str, label: &str, value: String, menu: impl Fn() -> Vec<MenuItem> + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        // Text lines up with the rows above; the hover background reaches out past it.
        .h(px(22.0))
        .mx(px(-6.0))
        .px(px(6.0))
        .rounded(px(5.0))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
        .text_size(crate::panels::scale::px(12.0))
        .child(div().flex_none().text_color(hsla(theme::TEXT_DIM)).child(label.to_string()))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .min_w_0()
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(value))
                .child(div().flex_none().text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::ChevronDown, 12.0))),
        )
        .dropdown(cx, move |ws, at, _w, cx| ws.open_dropdown(at, menu(), cx))
}

fn kv(label: &str, value: String) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        .h(px(22.0))
        .text_size(crate::panels::scale::px(12.0))
        .child(div().flex_none().text_color(hsla(theme::TEXT_DIM)).child(label.to_string()))
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(value))
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
    }

    fn labels(items: &[MenuItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn the_setting_menus_tick_the_current_value() {
        let sid = "s".to_string();
        assert_eq!(labels(&auto_compact_menu(&sid, Some(false))), ["    On", "✓  Off"]);
        assert_eq!(labels(&auto_compact_menu(&sid, None)), ["    On", "    Off"]);
        assert_eq!(labels(&advisor_menu(&sid, None)), ["✓  Off", "    Opus", "    Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("sonnet"))), ["    Off", "    Opus", "✓  Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("claude-opus-5-5"))), ["    Off", "✓  Opus", "    Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("claude-sonnet-4-5-20250929"))), ["    Off", "    Opus", "✓  Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("off"))), ["✓  Off", "    Opus", "    Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("my-model"))), ["    Off", "    Opus", "    Sonnet", "✓  Other…"]);
        assert_eq!(advisor_display(None, None), "Off");
        assert_eq!(advisor_display(Some("sonnet"), Some("sonnet")), "Sonnet");
        assert_eq!(advisor_display(Some("sonnet"), Some("claude-opus-5-5")), "Opus → Sonnet");
        assert_eq!(advisor_display(Some("opus"), None), "Opus");
    }
}
