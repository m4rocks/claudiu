//! How a thread is drawn: the conversation as a list that keeps to its end while it grows, and under it the plan,
//! the open permission question, the composer and a status line.

use gpui::{AnyElement, ClickEvent, Context, Div, Focusable, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, list, prelude::FluentBuilder, px};
use gpui_kit::component::input::Textarea;

use super::{Phase, ThreadView};
use crate::harness::thread::{Entry, ToolCall, ToolContent, ToolStatus, diff_lines};
use crate::panels::scale;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, key_hint, spinner};
use crate::visual::ui::Scope;

/// Lines of a tool's output shown while it is folded (Ctrl+O shows all).
const FOLDED_LINES: usize = 6;

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().flex().flex_col().bg(hsla(theme::PANEL_BG)).text_color(hsla(theme::TEXT)).text_size(scale::px(13.5));
        if let Some(handover) = &self.handover {
            return root.child(waiting(&handover.message));
        }
        if self.phase == Phase::Starting {
            return root.child(waiting(&format!("Starting {}…", self.harness.name())));
        }
        if let Some(review) = self.render_review(cx) {
            return root.child(review);
        }
        let conversation = list(self.list.clone(), cx.processor(|v: &mut Self, ix: usize, window, cx| v.render_entry(ix, window, cx)))
            .flex_1()
            .w_full()
            .px(px(18.0))
            .pt(px(12.0));
        root.child(conversation).children(self.render_plan()).children(self.render_question(cx)).child(self.render_composer(window, cx))
    }
}

fn waiting(message: &str) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(8.0))
        .text_color(hsla(theme::TEXT_DIM))
        .child(spinner("thread-waiting", 14.0))
        .child(message.to_string())
}

impl ThreadView {
    fn render_entry(&mut self, ix: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.thread.entries.get(ix) else { return div().into_any_element() };
        let body = match entry {
            Entry::User(text) => div()
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(6.0))
                .bg(hsla(theme::ROW_ACTIVE))
                .child(div().flex().gap(px(8.0)).child(div().text_color(hsla(theme::ACCENT)).child(">")).child(div().flex_1().child(text.clone())))
                .into_any_element(),
            Entry::Agent(text) => crate::github::ui::markdown(format!("thread-md-{ix}"), text).into_any_element(),
            Entry::Thought(text) => {
                let shown = if self.detail { text.clone() } else { text.lines().next().unwrap_or_default().to_string() };
                div().italic().text_color(hsla(theme::TEXT_FAINT)).child(format!("✻ {shown}")).into_any_element()
            }
            Entry::Tool(call) => self.render_tool(ix, call, cx),
            Entry::Notice { text, error } => div().text_color(hsla(if *error { theme::DANGER } else { theme::TEXT_DIM })).child(text.clone()).into_any_element(),
        };
        div().w_full().pb(px(12.0)).child(body).into_any_element()
    }

    fn render_tool(&self, ix: usize, call: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let (mark, color) = match call.status {
            ToolStatus::Pending | ToolStatus::Running => ("●", theme::TEXT_DIM),
            ToolStatus::Done => ("●", theme::OK),
            ToolStatus::Failed => ("●", theme::DANGER),
        };
        let mut header = div().flex().items_center().gap(px(8.0)).child(div().text_color(hsla(color)).child(mark)).child(div().font_weight(FontWeight::SEMIBOLD).child(title(call)));
        for (n, (path, line)) in call.locations.iter().enumerate().take(3) {
            let (path, line) = (path.clone(), *line);
            let label = path.file_name().map_or_else(|| path.display().to_string(), |f| f.to_string_lossy().into_owned());
            header = header.child(
                div()
                    .id(SharedString::from(format!("tool-loc-{ix}-{n}")))
                    .text_color(hsla(theme::TEXT_DIM))
                    .cursor_pointer()
                    .hover(|s| s.text_color(hsla(theme::TEXT)))
                    .child(match line {
                        Some(l) => format!("{label}:{l}"),
                        None => label,
                    })
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.open_location(path.clone(), line, cx))),
            );
        }
        let mut block = div().flex().flex_col().gap(px(4.0)).child(header);
        for content in &call.content {
            block = block.child(match content {
                ToolContent::Text(text) => output(text, self.detail),
                ToolContent::Diff { path, old, new } => diff(path, old.as_deref(), new, self.detail),
            });
        }
        if let Some(card) = self.cards.get(&call.id) {
            let scope = Scope { call: card.call.clone(), ui: self.card_ui.clone() };
            block = block.child(div().mt(px(4.0)).child(crate::visual::ui::markdown(format!("thread-card-{}", card.call), &card.content, scope)));
        }
        block.into_any_element()
    }

    fn render_plan(&self) -> Option<Div> {
        let working = self.working && self.thread.plan.iter().any(|p| p.status != "completed");
        if self.thread.plan.is_empty() || !(self.plan_open || working) {
            return None;
        }
        let mut plan = div().mx(px(18.0)).mb(px(8.0)).px(px(12.0)).py(px(8.0)).rounded(px(6.0)).border_1().border_color(hsla(theme::PANEL_BORDER)).flex().flex_col().gap(px(2.0));
        for item in &self.thread.plan {
            let (mark, color) = match item.status.as_str() {
                "completed" => ("☒", theme::TEXT_FAINT),
                "in_progress" => ("◼", theme::ACCENT),
                _ => ("☐", theme::TEXT_DIM),
            };
            plan = plan.child(div().flex().gap(px(8.0)).text_color(hsla(color)).child(mark).child(item.text.clone()));
        }
        Some(plan)
    }

    fn render_question(&self, cx: &mut Context<Self>) -> Option<Div> {
        let q = self.question.as_ref()?;
        let mut options = div().flex().flex_wrap().gap(px(6.0));
        for (i, (_, name, kind)) in q.options.iter().enumerate() {
            let primary = kind == "allow_once";
            options = options.child(
                button(format!("thread-perm-{i}"), format!("{}  {name}", i + 1), primary).on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.answer(Some(i), cx))),
            );
        }
        Some(
            div()
                .mx(px(18.0))
                .mb(px(8.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(hsla(theme::ACCENT))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(q.title.clone()))
                .child(options)
                .child(div().flex().gap(px(6.0)).items_center().text_color(hsla(theme::TEXT_FAINT)).text_size(scale::px(11.5)).child(key_hint("1-9")).child("to choose").child(key_hint("Esc")).child("to decline")),
        )
    }

    fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let _ = window;
        let mut column = div().flex_none().px(px(18.0)).pb(px(10.0)).flex().flex_col().gap(px(6.0));
        let commands = self.matching_commands(cx);
        if !commands.is_empty() {
            let mut menu = div().flex().flex_col().px(px(8.0)).py(px(4.0)).rounded(px(6.0)).bg(hsla(theme::APP_BG)).border_1().border_color(hsla(theme::PANEL_BORDER));
            for (n, c) in commands.into_iter().enumerate() {
                let name = c.name.clone();
                menu = menu.child(
                    div()
                        .id(SharedString::from(format!("thread-cmd-{n}")))
                        .flex()
                        .gap(px(10.0))
                        .py(px(2.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
                        .child(div().text_color(hsla(theme::ACCENT)).child(format!("/{}", c.name)))
                        .child(div().flex_1().truncate().text_color(hsla(theme::TEXT_DIM)).child(c.description.clone()))
                        .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                            v.set_draft(&format!("/{name} "), window, cx);
                            cx.notify();
                        })),
                );
            }
            column = column.child(menu);
        }
        if self.model_menu
            && let Some(model) = self.thread.option("model")
        {
            let mut menu = div().flex().flex_col().px(px(8.0)).py(px(4.0)).rounded(px(6.0)).bg(hsla(theme::APP_BG)).border_1().border_color(hsla(theme::PANEL_BORDER));
            for (n, (value, name)) in model.choices.iter().enumerate() {
                let (id, value) = (model.id.clone(), value.clone());
                let current = value == model.current;
                menu = menu.child(
                    div()
                        .id(SharedString::from(format!("thread-model-{n}")))
                        .py(px(2.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
                        .text_color(hsla(if current { theme::ACCENT } else { theme::TEXT }))
                        .child(name.clone())
                        .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                            v.model_menu = false;
                            v.set_option(id.clone(), value.clone(), cx);
                            cx.notify();
                        })),
                );
            }
            column = column.child(menu);
        }
        let border = if self.composer.focus_handle(cx).is_focused(window) { theme::ACCENT } else { theme::PANEL_BORDER };
        column
            .child(
                div()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(hsla(border))
                    .bg(hsla(theme::APP_BG))
                    .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| v.composer.update(cx, |c, cx| c.focus(window, cx))))
                    .child(Textarea::new(&self.composer).appearance(false)),
            )
            .child(self.render_status(cx))
    }

    fn render_status(&self, cx: &mut Context<Self>) -> Div {
        let faint = |text: String| div().text_color(hsla(theme::TEXT_FAINT)).child(text);
        let mut line = div().flex().items_center().gap(px(12.0)).text_size(scale::px(11.5));
        if self.working {
            line = line.child(div().flex().items_center().gap(px(6.0)).child(spinner("thread-working", 11.0)).child(faint("Working".into()))).child(div().flex().gap(px(4.0)).items_center().child(key_hint("Esc")).child(faint("to interrupt".into())));
        }
        if let Some(mode) = self.thread.mode_name() {
            line = line.child(
                div()
                    .id("thread-mode")
                    .flex()
                    .gap(px(4.0))
                    .items_center()
                    .cursor_pointer()
                    .child(div().text_color(hsla(theme::TEXT_DIM)).child(mode.to_string()))
                    .child(key_hint("Shift+Tab"))
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                        if let Some(next) = v.thread.next_mode().map(String::from) {
                            v.set_mode(next, cx);
                        }
                    })),
            );
        }
        if let Some(model) = self.thread.option("model") {
            let name = model.choices.iter().find(|(v, _)| *v == model.current).map_or(model.current.clone(), |(_, n)| n.clone());
            line = line.child(
                div()
                    .id("thread-model")
                    .flex()
                    .gap(px(4.0))
                    .items_center()
                    .cursor_pointer()
                    .child(div().text_color(hsla(theme::TEXT_DIM)).child(name))
                    .child(key_hint(if cfg!(target_os = "macos") { "Option+P" } else { "Alt+P" }))
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                        v.model_menu = !v.model_menu;
                        cx.notify();
                    })),
            );
        }
        line = line.child(div().flex_1());
        if self.can_review() {
            line = line.child(
                div()
                    .id("thread-review-open")
                    .cursor_pointer()
                    .text_color(hsla(theme::TEXT_DIM))
                    .hover(|s| s.text_color(hsla(theme::TEXT)))
                    .child("Review changes")
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_review(cx))),
            );
        }
        if !self.queue.is_empty() {
            line = line.child(faint(format!("{} queued", self.queue.len())));
        }
        if let Some((used, size)) = self.thread.context.filter(|(_, size)| *size > 0) {
            line = line.child(faint(format!("{}% context", used * 100 / size)));
        }
        line
    }
}

/// A tool's title, or its kind while the agent hasn't named it.
fn title(call: &ToolCall) -> String {
    if !call.title.is_empty() {
        return call.title.clone();
    }
    if call.kind.is_empty() { "Tool".into() } else { call.kind.clone() }
}

fn mono() -> Div {
    div().font_family(crate::platform::monospace_font()).text_size(scale::px(12.5))
}

fn output(text: &str, full: bool) -> AnyElement {
    let text = text.trim_matches('\n').trim_start_matches("```console\n").trim_end_matches("```").trim_end();
    let lines: Vec<&str> = text.lines().collect();
    let shown = if full { lines.len() } else { lines.len().min(FOLDED_LINES) };
    let mut block = mono().pl(px(18.0)).text_color(hsla(theme::TEXT_DIM)).flex().flex_col();
    for l in &lines[..shown] {
        block = block.child(div().whitespace_nowrap().child(l.to_string()));
    }
    if shown < lines.len() {
        block = block.child(div().text_color(hsla(theme::TEXT_FAINT)).child(format!("… {} more lines (Ctrl+O)", lines.len() - shown)));
    }
    block.into_any_element()
}

fn diff(path: &std::path::Path, old: Option<&str>, new: &str, full: bool) -> AnyElement {
    let lines = diff_lines(old.unwrap_or_default(), new);
    let shown = if full { lines.len() } else { lines.len().min(FOLDED_LINES * 3) };
    let mut block = mono().ml(px(18.0)).rounded(px(4.0)).border_1().border_color(hsla(theme::PANEL_BORDER)).flex().flex_col().overflow_hidden();
    block = block.child(div().px(px(8.0)).py(px(2.0)).bg(hsla(theme::APP_BG)).text_color(hsla(theme::TEXT_DIM)).child(path.display().to_string()));
    for (sign, text) in &lines[..shown] {
        let (bg, fg) = match sign {
            '+' => (0x16301e, theme::OK),
            '-' => (0x3a1a1d, theme::DANGER),
            _ => (theme::PANEL_BG, theme::TEXT_DIM),
        };
        block = block.child(div().px(px(8.0)).bg(hsla(bg)).text_color(hsla(fg)).whitespace_nowrap().child(format!("{sign} {text}")));
    }
    if shown < lines.len() {
        block = block.child(div().px(px(8.0)).text_color(hsla(theme::TEXT_FAINT)).child(format!("… {} more lines (Ctrl+O)", lines.len() - shown)));
    }
    block.when(old.is_none(), |b| b.border_color(hsla(theme::OK))).into_any_element()
}
