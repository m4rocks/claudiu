//! The right sidebar: the shown session's actions and status, or (one click away) the project's GitHub view.
//! It sits under the top bar so the window buttons stay in the corner.

use std::path::PathBuf;

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::app::{Act, GitOp, MenuItem, SideDrag, Workspace};
use crate::platform::Editor;
use crate::store::{Attached, Id, Project, SessionKind, SessionRecord};
use crate::theme::{self, hsla};
use crate::widgets::{countdown, icon, icon_button, meter, section_label, tick};
use gpui_kit::assets::IconName;

/// What the top bar and the right sidebar describe: the shown session (or chat) and the folder and project it is in.
pub struct Shown {
    pub rec: Option<SessionRecord>,
    pub cwd: Option<PathBuf>,
    pub project: Option<Project>,
    /// The chat's title, when a chat is shown.
    pub chat_title: Option<String>,
}

impl Workspace {
    pub fn shown(&self) -> Shown {
        let id = self.active_id();
        let rec = id.and_then(|id| self.store.session(id)).cloned();
        let chat = id.and_then(|id| self.chat_pane(id).map(|p| (p.cwd.clone(), self.chat_title(id).unwrap_or_default())));
        let project = rec
            .as_ref()
            .and_then(|r| r.project_id.clone())
            .or_else(|| chat.as_ref().and_then(|(cwd, _)| self.store.project_for_cwd(cwd)))
            .and_then(|p| self.store.project(&p))
            .cloned();
        let cwd = rec.as_ref().map(|r| r.cwd.clone()).or_else(|| chat.as_ref().filter(|(cwd, _)| crate::chat::store::is_tied(cwd)).map(|(cwd, _)| cwd.clone()));
        Shown { rec, cwd, project, chat_title: chat.map(|(_, title)| title) }
    }

    pub fn render_right(&mut self, window_w: f32, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.store.data.layout.right_hidden {
            return None;
        }
        let width = self.right_width(window_w);
        let github = self.right_gh.as_ref().and_then(|id| self.gh_pane(id)).map(|p| (p.view.clone(), p.project.clone()));
        let body = match github {
            Some((view, project)) => {
                view.update(cx, |v, _| v.width = width);
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
                            .h(px(34.0))
                            .px(px(8.0))
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
                            .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).child(format!("GitHub · {project}"))),
                    )
                    .child(div().flex_1().min_h_0().child(view))
                    .into_any_element()
            }
            None => self.render_right_menu(window, cx).into_any_element(),
        };
        // Both views can be resized by their left edge.
        let handle = Some(
            div()
                .id("right-resize")
                .absolute()
                .top_0()
                .left(px(-3.0))
                .w(px(6.0))
                .h_full()
                .cursor_col_resize()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                        ws.right_drag = Some(SideDrag { x: f32::from(ev.position.x), width });
                        cx.notify();
                    }),
                ),
        );
        Some(
            div()
                .id("right-sidebar")
                .relative()
                .flex_none()
                .w(px(width))
                .h_full()
                .bg(hsla(theme::SIDEBAR_BG))
                .border_l_1()
                .border_color(hsla(theme::BORDER))
                .child(body)
                .children(handle)
                .into_any_element(),
        )
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
        let status = self.status_rows(&shown, id.as_ref(), running, cx);
        self.menu_scroll
            .wire(div().id("right-menu").size_full(), window)
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(8.0))
            .child(group("ACTIONS", actions))
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
                        .child(icon_button("open-in-pick", IconName::ChevronDown).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                                let current = ws.preferred_editor();
                                let items = ws
                                    .editors
                                    .iter()
                                    .filter(|(_, found)| *found)
                                    .map(|(e, _)| MenuItem::new(format!("{}{}", tick(current == Some(*e)), e.label()), Act::OpenEditor(*e, pick_target.clone())))
                                    .collect();
                                ws.open_menu(ev.position, items, cx);
                            }),
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
            let git_row = |cx: &mut Context<Self>, key: &'static str, glyph: IconName, label: &'static str, doing: &'static str, op: GitOp, make: fn(PathBuf) -> Act| {
                if busy.is_some() {
                    dim_row(key, glyph, if busy == Some(op) { doing } else { label })
                } else {
                    let repo = repo.clone();
                    row(key, glyph, label).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(make(repo.clone()), window, cx)))
                }
            };
            actions = actions
                .child(git_row(cx, "commit-push", IconName::GitCommitHorizontal, "Commit & Push", "Committing…", GitOp::CommitPush, Act::CommitPush))
                .child(git_row(cx, "pull", IconName::ArrowDownToLine, "Pull", "Pulling…", GitOp::Pull, Act::Pull));
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

    /// Issues and pull requests attached to the session or chat: click to open, × to take off.
    fn attached_rows(&self, id: Option<&Id>, attached: Vec<Attached>, cx: &mut Context<Self>) -> Div {
        let mut rows = div().flex().flex_col().gap(px(1.0));
        for a in attached {
            let (pull, number) = (a.pull, a.number);
            let owner = id.cloned();
            let title = a.title.map_or_else(|| format!("#{number}"), |t| format!("#{number}  {t}"));
            rows = rows.child(removable_row(
                cx,
                format!("attached-{pull}-{number}"),
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

    /// Sessions and chats linked with this one: click to go there, × to unlink.
    fn linked_rows(&self, id: Option<&Id>, links: Vec<(Id, String)>, cx: &mut Context<Self>) -> Div {
        let mut rows = div().flex().flex_col().gap(px(1.0));
        for (other, title) in links {
            let glyph = if self.chat_store.get(&other).is_some() { IconName::MessageSquare } else { IconName::Brackets };
            let (me, go, gone) = (id.cloned(), other.clone(), other.clone());
            rows = rows.child(removable_row(
                cx,
                format!("linked-{other}"),
                glyph,
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

        // A code session's figures come from its snapshot, a chat's from the mod inside its own claude.
        let meta = shown.rec.as_ref().filter(|r| r.kind == SessionKind::Claude).and_then(|r| r.claude.as_ref());
        let pane = id.and_then(|i| self.chat_pane(i));
        let (compact, advisor, known) = match (meta, pane) {
            (Some(m), _) => (m.auto_compact, m.advisor.clone(), true),
            (None, Some(p)) => (p.auto_compact, p.advisor.clone(), p.auto_compact.is_some() || p.advisor.is_some()),
            _ => (None, None, false),
        };
        let advisor = advisor.map(|a| a.to_lowercase());
        let compact_text = compact.map_or("—", |v| if v { "On" } else { "Off" }).to_string();
        let advisor_text = if known { advisor.clone().unwrap_or_else(|| "Off".into()) } else { "—".into() };
        let settable = id.filter(|_| (meta.is_some() && running) || pane.is_some()).cloned();

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
            .px(px(10.0))
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
    let other = current.is_some_and(|a| a != "off" && !ALIASES.iter().any(|(k, _)| *k == a));
    let mut items = vec![MenuItem::new(format!("{}Off", tick(current.is_none())), Act::SetAdvisor(sid.clone(), "off".into()))];
    items.extend(ALIASES.iter().map(|(alias, name)| MenuItem::new(format!("{}{name}", tick(current == Some(*alias))), Act::SetAdvisor(sid.clone(), (*alias).into()))));
    items.push(MenuItem::new(format!("{}Other…", tick(other)), Act::AskAdvisor(sid.clone())));
    items
}

fn group(label: &str, body: impl IntoElement) -> Div {
    div().flex().flex_col().gap(px(4.0)).child(div().px(px(10.0)).child(section_label(label.to_string()))).child(body)
}

fn base_row(id: impl Into<SharedString>, glyph: IconName, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id.into())
        .h(px(28.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .rounded(px(6.0))
        .text_size(px(12.5))
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
fn removable_row(
    cx: &mut Context<Workspace>,
    key: String,
    glyph: IconName,
    title: String,
    open: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    remove: impl Fn(&mut Workspace) + 'static,
) -> Stateful<Div> {
    let x = format!("remove-{key}");
    row(key, glyph, title)
        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| open(ws, window, cx)))
        .child(icon_button(x, IconName::X).on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
            cx.stop_propagation();
            remove(ws);
            cx.notify();
        })))
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
        .text_size(px(12.0))
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
        .on_mouse_down(MouseButton::Left, cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| ws.open_menu(ev.position, menu(), cx)))
}

fn kv(label: &str, value: String) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        .h(px(22.0))
        .text_size(px(12.0))
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
        assert_eq!(labels(&advisor_menu(&sid, Some("claude-opus-5-5"))), ["    Off", "    Opus", "    Sonnet", "✓  Other…"]);
    }
}
