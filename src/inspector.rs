//! The right sidebar: the shown session's actions and status, or (one click away) the project's GitHub view.
//! It sits under the top bar so the window buttons stay in the corner.

use std::path::PathBuf;

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, SharedString, Stateful,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use crate::app::{Act, GitOp, MenuItem, RightDrag, Workspace};
use crate::platform::Editor;
use crate::store::{Project, SessionKind, SessionRecord};
use crate::theme::{self, hsla};
use crate::widgets::{countdown, icon, icon_button, meter, section_label};
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

    pub fn render_right(&mut self, window_w: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
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
            None => self.render_right_menu(cx).into_any_element(),
        };
        // The wide GitHub view can be resized by its left edge; the menu has one width.
        let handle = self.right_gh.is_some().then(|| {
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
                        ws.right_drag = Some(RightDrag { x: f32::from(ev.position.x), width });
                        cx.notify();
                    }),
                )
        });
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

    fn render_right_menu(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shown = self.shown();
        let id = self.active_id().cloned();
        let claude = shown.rec.as_ref().filter(|r| r.kind == SessionKind::Claude);
        let target = shown.project.as_ref().map(|p| p.path.clone()).or_else(|| shown.cwd.clone());
        let repo = shown.project.as_ref().filter(|p| p.git.is_some()).and(shown.cwd.clone());
        let busy = self.git_busy;
        let running = id.as_ref().is_some_and(|id| self.is_running(id, cx));

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
                        .child(
                            icon_button("open-in-pick", IconName::ChevronDown).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                                    let current = ws.preferred_editor();
                                    let items = ws
                                        .editors
                                        .iter()
                                        .filter(|(_, found)| *found)
                                        .map(|(e, _)| MenuItem {
                                            label: format!("{}{}", if current == Some(*e) { "✓  " } else { "    " }, e.label()),
                                            hint: None,
                                            disabled: false,
                                            act: Act::OpenEditor(*e, pick_target.clone()),
                                            danger: false,
                                            separator_before: false,
                                        })
                                        .collect();
                                    ws.open_menu(ev.position, items, cx);
                                }),
                            ),
                        ),
                );
            }
            actions = actions.child(
                row("reveal", IconName::FolderOpen, "Open folder")
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Reveal(target.clone()), window, cx))),
            );
        }
        if let Some(repo) = repo {
            let pull_repo = repo.clone();
            // One git operation at a time: while one runs, both rows are disabled.
            actions = actions
                .child(if busy.is_some() {
                    dim_row("commit-push", IconName::GitCommitHorizontal, if busy == Some(GitOp::CommitPush) { "Committing…" } else { "Commit & Push" })
                } else {
                    row("commit-push", IconName::GitCommitHorizontal, "Commit & Push")
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::CommitPush(repo.clone()), window, cx)))
                })
                .child(if busy.is_some() {
                    dim_row("pull", IconName::ArrowDownToLine, if busy == Some(GitOp::Pull) { "Pulling…" } else { "Pull" })
                } else {
                    row("pull", IconName::ArrowDownToLine, "Pull")
                        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::Pull(pull_repo.clone()), window, cx)))
                });
        }
        if let (Some(id), true, true) = (id.clone(), claude.is_some(), running) {
            let open = self.shells.get(&id).is_some_and(|p| p.open);
            actions = actions.child(
                row("shell", IconName::Terminal, if open { "Hide shell" } else { "Show shell" })
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleShell(id.clone()), window, cx))),
            );
        }
        let cwd = self.active_cwd();
        actions = actions.child(
            row("github", IconName::GitPullRequest, "GitHub")
                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::OpenGithub { cwd: cwd.clone() }, window, cx))),
        );

        // Issues and pull requests Claude attached to this session.
        let attached = claude.and_then(|r| r.claude.as_ref()).map(|m| m.attached.clone()).unwrap_or_default();
        let any_attached = !attached.is_empty();
        let mut github = div().flex().flex_col().gap(px(1.0));
        for a in attached {
            let (pull, number) = (a.pull, a.number);
            let (open_id, rec_id) = (format!("attached-{}-{number}", a.pull), id.clone());
            let title = match &a.title {
                Some(t) => format!("#{number}  {t}"),
                None => format!("#{number}"),
            };
            github = github.child(
                row(open_id, if pull { IconName::GitPullRequest } else { IconName::CircleDot }, title)
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.open_attached(pull, number, window, cx)))
                    .child(icon_button(format!("detach-{pull}-{number}"), IconName::X).on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
                        cx.stop_propagation();
                        if let Some(meta) = rec_id.as_ref().and_then(|id| ws.store.session_mut(id)).and_then(|r| r.claude.as_mut()) {
                            meta.attached.retain(|x| !(x.pull == pull && x.number == number));
                            ws.store.mark_dirty();
                        }
                        cx.notify();
                    }))),
            );
        }

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
        let meta = claude.and_then(|r| r.claude.as_ref());
        let on_off = |v: Option<bool>| v.map_or("—", |v| if v { "On" } else { "Off" }).to_string();
        let status = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .px(px(10.0))
            .child(meter("CONTEXT", ctx_fraction, ctx_caption, None))
            .child(cache)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(kv("Auto-compact", on_off(meta.and_then(|m| m.auto_compact))))
                    .child(kv("Advisor", meta.map_or("—".into(), |m| m.advisor.clone().unwrap_or_else(|| "Off".into()))))
                    .child(kv("Last command", meta.and_then(|m| m.last_command.clone()).map_or("—".into(), |c| format!("/{c}")))),
            );

        div()
            .id("right-menu")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(8.0))
            .child(group("ACTIONS", actions))
            .when(any_attached, |d| d.child(group("GITHUB", github)))
            .child(group("STATUS", status))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// gpui panics when a style is set twice (a hover on a row that already has one), so just building both is the check.
    #[test]
    fn both_row_kinds_can_be_built() {
        row("a", IconName::Terminal, "A");
        dim_row("b", IconName::Terminal, "B");
    }
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
