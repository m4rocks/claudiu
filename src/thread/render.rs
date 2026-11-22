//! How a thread is drawn: a readable conversation column, active plan/permission prompts, an elevated composer dock
//! with a unified model + draggable effort picker (aggregating every enabled harness) and a permission selector dialog.

use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, Div, Focusable, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, linear_color_stop, linear_gradient, list, prelude::FluentBuilder, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Textarea;
use gpui_kit::component::text::{TextView, TextViewState};
use std::collections::HashMap;
use gpui::{AppContext, Entity};

use super::{ThreadEvent, ThreadPicker, ThreadView};
use crate::harness::CatalogModel;
use crate::harness::thread::{Entry, SubagentReport, ToolCall, ToolContent, ToolStatus, diff_lines, parse_subagent_report};
use crate::panels::{composer, logos, picker, scale};
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{EffortPick, Trigger, effort_slider, icon, icon_button, key_badge, spinner};
use crate::visual::ui::Scope;

/// Lines of a tool's output shown while it is folded (Ctrl+O shows all).
const FOLDED_LINES: usize = 6;
/// Shared max width for conversation entries, plan/question cards, and the bottom composer so edges align cleanly.
const COLUMN_MAX_W: f32 = 840.0;

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(theme::PANEL_BG))
            .text_color(hsla(theme::TEXT))
            .text_size(scale::px(13.5))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|v, _: &gpui::MouseDownEvent, window, cx| {
                    if v.picker != Some(ThreadPicker::Model) && v.review.is_none() {
                        v.focus_composer(window, cx);
                    }
                }),
            );
        if let Some(handover) = &self.handover {
            return root.child(waiting(&handover.message));
        }
        if let Some(review) = self.render_review(cx) {
            return root.child(review);
        }
        let conversation = list(self.list.clone(), cx.processor(|v: &mut Self, ix: usize, window, cx| v.render_entry(ix, window, cx)))
            .size_full()
            .pt(px(14.0));
        // The conversation ends above the message box rather than under it, so everything written before a question
        // can still be scrolled to and read while it waits. A short fade softens the edge.
        let panel_bg = hsla(theme::PANEL_BG);
        let fade = linear_gradient(180.0, linear_color_stop(panel_bg.opacity(0.0), 0.0), linear_color_stop(panel_bg, 1.0));
        let conversation = div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(conversation)
            .child(div().absolute().bottom_0().left_0().right_0().h(px(18.0)).bg(fade));
        let conversation = div().flex_1().min_h_0().flex().flex_col().child(conversation).children(self.render_activity());
        let bottom_dock = div()
            .flex_none()
            .flex()
            .flex_col()
            .children(self.render_plan())
            .children(self.render_background(cx))
            .child(self.render_composer(window, cx));
        root.child(conversation)
            .child(bottom_dock)
            .children(self.render_picker_overlay(window, cx))
    }
}

/// The user's message as markdown that keeps each line they typed. Markdown would join lines with a single break
/// between them into one, so every line before another ends in a hard break (two trailing spaces, which unlike a
/// backslash leave nothing behind when the next line starts a list or a fence), except inside a fenced code block,
/// where lines already stay as they are.
fn keep_line_breaks(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut in_fence = false;
    let mut out = String::with_capacity(text.len() + lines.len());
    for (i, line) in lines.iter().enumerate() {
        let fence = line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~");
        if fence {
            in_fence = !in_fence;
        }
        out.push_str(line);
        let Some(next) = lines.get(i + 1) else { break };
        let breaks = !in_fence && !fence && !line.trim().is_empty() && !next.trim().is_empty();
        out.push_str(if breaks { "  \n" } else { "\n" });
    }
    out
}

/// Entry `ix`'s text as markdown, parsed once and kept in `cache` (`ThreadView::markdown`) so it keeps its height
/// as it scrolls.
pub(super) fn markdown_of(cache: &mut HashMap<usize, Entity<TextViewState>>, ix: usize, text: &str, cx: &mut Context<ThreadView>) -> TextView {
    let state = cache.entry(ix).or_insert_with(|| cx.new(|cx| TextViewState::markdown(text, cx))).clone();
    // Unchanged text costs a comparison; a reply still streaming in is appended to what is parsed.
    state.update(cx, |s, cx| s.set_text(text, cx));
    crate::github::ui::styled_markdown(TextView::new(&state))
}

/// A message's first line with text, for a one-line row.
fn first_line(text: &str) -> String {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_string()
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

fn render_thought_body(text: &str) -> AnyElement {
    div()
        .ml(px(14.0))
        .pl(px(10.0))
        .pb(px(4.0))
        .pt(px(2.0))
        .border_l_1()
        .border_color(hsla(theme::BORDER))
        .italic()
        .text_size(scale::px(12.0))
        .text_color(hsla(theme::TEXT_FAINT))
        .child(text.to_string())
        .into_any_element()
}

fn indent_run_step(el: AnyElement) -> AnyElement {
    div().ml(px(14.0)).pl(px(10.0)).border_l_1().border_color(hsla(theme::BORDER)).child(el).into_any_element()
}

impl ThreadView {
    /// Combined model list: live models reported by this session's harness, plus cached models from every other
    /// enabled harness. Filters out synthetic `"default"` entries when real models exist.
    pub fn unified_models(&self) -> Vec<CatalogModel> {
        let mut out = Vec::new();
        if let Some(opt) = self.thread.option("model")
            && !opt.choices.is_empty()
        {
            let real: Vec<&(String, String)> = opt.choices.iter().filter(|(id, _)| id != "default").collect();
            let src: Vec<&(String, String)> = if real.is_empty() { opt.choices.iter().collect() } else { real };
            let live = src.into_iter().map(|(id, name)| CatalogModel { harness: self.harness, id: id.clone(), name: name.clone() }).collect();
            out.extend(crate::store::keep_picked(live, &self.picked, |m| &m.id));
            for m in &self.all_models {
                if m.harness != self.harness {
                    out.push(m.clone());
                }
            }
        } else {
            out.extend(self.all_models.iter().cloned());
        }
        out
    }

    fn active_model_label(&self) -> String {
        if let Some(pending) = &self.pending_model {
            return pending.name.clone();
        }
        if let Some(model) = self.thread.option("model") {
            if let Some((_, name)) = model.choices.iter().find(|(v, _)| *v == model.current && v != "default") {
                return name.clone();
            }
            if let Some((_, name)) = model.choices.iter().find(|(v, _)| v != "default").or_else(|| model.choices.first()) {
                return name.clone();
            }
        }
        self.all_models
            .iter()
            .find(|m| m.harness == self.harness)
            .map_or_else(|| self.harness.name().to_string(), |m| m.name.clone())
    }

    fn render_entry(&mut self, ix: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.thread.entries.get(ix) else { return div().into_any_element() };
        let body = match entry {
            Entry::User(text) if let Some(report) = parse_subagent_report(text) => {
                let run = tool_run(&self.groupable(), ix);
                match &run {
                    Some(run) if run.start == ix && run.len() > 1 => return self.wrap_entry(ix, self.render_tool_run(run.clone(), cx)),
                    Some(run) if !self.run_open(run) => return div().into_any_element(),
                    _ => {}
                }
                let el = self.render_subagent_report(ix, &report, cx);
                if run.is_some_and(|r| r.len() > 1) { indent_run_step(el) } else { el }
            }
            Entry::User(text) => div()
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(9.0))
                .bg(hsla(theme::ROW_ACTIVE))
                .border_1()
                .border_color(hsla(theme::PANEL_BORDER))
                .child(markdown_of(&mut self.markdown, ix, &keep_line_breaks(text), cx))
                .into_any_element(),
            Entry::Agent(text) => div().px(px(2.0)).child(markdown_of(&mut self.markdown, ix, text, cx)).into_any_element(),
            Entry::Thought(text) => {
                let run = tool_run(&self.groupable(), ix);
                match &run {
                    Some(run) if run.start == ix && run.len() > 1 => return self.wrap_entry(ix, self.render_tool_run(run.clone(), cx)),
                    Some(run) if !self.run_open(run) => return div().into_any_element(),
                    _ => {}
                }
                let in_group = run.is_some_and(|r| r.len() > 1);
                let el = self.render_thought(ix, text, cx);
                if in_group { indent_run_step(el) } else { el }
            }
            Entry::Tool(call) => match tool_run(&self.groupable(), ix) {
                // The run's first call draws the run's line; the rest only show while the run is open.
                Some(run) if run.start == ix => self.render_tool_run(run, cx),
                Some(run) if !self.run_open(&run) => return div().into_any_element(),
                Some(run) if run.len() > 1 => indent_run_step(self.render_tool(ix, call, cx)),
                _ => self.render_tool(ix, call, cx),
            },
            Entry::Notice { text, error } => div()
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(hsla(if *error { 0x251417 } else { 0x131315 }))
                .border_1()
                .border_color(hsla(if *error { 0x55242a } else { theme::PANEL_BORDER }))
                .text_size(scale::px(12.0))
                .text_color(hsla(if *error { theme::DANGER } else { theme::TEXT_DIM }))
                .child(text.clone())
                .into_any_element(),
        };
        self.wrap_entry(ix, body)
    }

    /// A subagent's report, which m4code sent this session as a message: drawn as a folded step among the tool calls,
    /// not as the user's words. Click the row to read the report; Open shows the child.
    fn render_subagent_report(&self, ix: usize, report: &SubagentReport, cx: &mut Context<Self>) -> AnyElement {
        let key = format!("report:{ix}");
        let expanded = self.detail || self.expanded_tools.contains(&key);
        let harness = crate::harness::Harness::ALL.into_iter().find(|h| h.key() == report.harness).unwrap_or_default();
        let failed = report.state == "failed";
        let child = report.id.to_string();
        let open = div()
            .id(SharedString::from(format!("report-open-{ix}")))
            .flex_none()
            .px(px(5.0))
            .py(px(1.0))
            .rounded(px(4.0))
            .bg(hsla(theme::SURFACE_ELEVATED))
            .border_1()
            .border_color(hsla(theme::PANEL_BORDER))
            .text_size(scale::px(11.0))
            .text_color(hsla(theme::TEXT_DIM))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)).text_color(hsla(theme::TEXT)))
            .child("Open")
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                cx.emit(ThreadEvent::Select(child.clone()));
            }));
        let header = div()
            .id(SharedString::from(format!("report-row-{ix}")))
            .w_full()
            .px(px(4.0))
            .py(px(2.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
            .text_size(scale::px(12.0))
            .text_color(hsla(if failed { theme::DANGER } else if expanded { theme::TEXT_DIM } else { theme::TEXT_FAINT }))
            .child(logos::draw(logos::for_harness(harness), 12.0, hsla(if failed { theme::DANGER } else { theme::TEXT_FAINT })))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(report_line(report.title, report.state)))
            .child(open)
            .child(div().flex_none().text_color(hsla(theme::TEXT_FAINT)).child(icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                if !v.expanded_tools.remove(&key) {
                    v.expanded_tools.insert(key.clone());
                }
                v.remeasure(ix);
                v.focus_composer(window, cx);
                cx.notify();
            }));
        let mut block = div().flex().flex_col().child(header);
        if expanded {
            block = block.child(
                div().ml(px(14.0)).pl(px(10.0)).pb(px(6.0)).pt(px(2.0)).border_l_1().border_color(hsla(theme::BORDER))
                    .child(crate::github::ui::markdown(format!("thread-md-{ix}"), report.body)),
            );
        }
        block.into_any_element()
    }

    /// An entry in the conversation's centred column.
    /// What the agent is doing, at the foot of the conversation: the chat is where you look to see whether it works,
    /// and the message box stays yours.
    fn render_activity(&self) -> Option<AnyElement> {
        let (label, stoppable) = match self.starting_label() {
            Some(label) => (label, false),
            None if self.is_working() => ((if self.compaction.running { "Compacting…" } else { "Working…" }).to_string(), true),
            None => return None,
        };
        let row = div()
            .w_full()
            .max_w(px(COLUMN_MAX_W))
            .px(px(2.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(spinner("thread-activity", 12.0))
            .child(div().text_size(scale::px(12.5)).text_color(hsla(theme::TEXT_DIM)).child(label))
            .when(stoppable, |d| d.child(div().flex().gap(px(4.0)).items_center().child(key_badge("Esc")).child(div().text_size(scale::px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child("to stop"))));
        Some(div().flex_none().w_full().px(px(22.0)).pb(px(10.0)).flex().justify_center().child(row).into_any_element())
    }

    fn wrap_entry(&self, ix: usize, body: AnyElement) -> AnyElement {
        let is_last = ix + 1 == self.thread.entries.len();
        div()
            .w_full()
            .px(px(22.0))
            .pb(px(if is_last { 18.0 } else { 10.0 }))
            .flex()
            .justify_center()
            .child(div().w_full().max_w(px(COLUMN_MAX_W)).child(body))
            .into_any_element()
    }

    /// Compressed single-line thinking step; click to expand the full thought, just like a tool call.
    fn render_thought(&self, ix: usize, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let key = format!("thought:{ix}");
        let expanded = self.detail || self.expanded_tools.contains(&key);
        let preview = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_string();
        let header = div()
            .id(SharedString::from(format!("thought-row-{ix}")))
            .w_full()
            .px(px(4.0))
            .py(px(2.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
            .text_size(scale::px(12.0))
            .text_color(hsla(theme::TEXT_FAINT))
            .child(div().flex_none().italic().text_color(hsla(if expanded { theme::TEXT_DIM } else { theme::TEXT_FAINT })).child("Thought"))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().italic().child(preview))
            .child(div().flex_none().child(icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                if !v.expanded_tools.remove(&key) {
                    v.expanded_tools.insert(key.clone());
                }
                v.remeasure(ix);
                v.focus_composer(window, cx);
                cx.notify();
            }));
        let mut block = div().flex().flex_col().child(header);
        if expanded {
            block = block.child(render_thought_body(text));
        }
        block.into_any_element()
    }

    /// Compressed single-line tool call row; click to expand full output and diffs.
    fn render_tool(&self, ix: usize, call: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let failed = call.status == ToolStatus::Failed;
        let has_card = self.cards.contains_key(&call.id);
        let has_body = !call.content.is_empty() || has_card;
        let expanded = self.detail || has_card || self.expanded_tools.contains(&call.id);
        let toggle_id = call.id.clone();

        let mut header = div()
            .id(SharedString::from(format!("tool-row-{ix}")))
            .w_full()
            .px(px(4.0))
            .py(px(2.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .when(has_body, |d| d.cursor_pointer().hover(|s| s.bg(hsla(theme::SURFACE_HOVER))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(scale::px(12.0))
                    .text_color(hsla(if failed {
                        theme::DANGER
                    } else if expanded {
                        theme::TEXT_DIM
                    } else {
                        theme::TEXT_FAINT
                    }))
                    .child(title(call)),
            );
        for (n, (path, line)) in call.locations.iter().enumerate().take(2) {
            let (path, line) = (path.clone(), *line);
            let label = path.file_name().map_or_else(|| path.display().to_string(), |f| f.to_string_lossy().into_owned());
            header = header.child(
                div()
                    .id(SharedString::from(format!("tool-loc-{ix}-{n}")))
                    .flex_none()
                    .px(px(5.0))
                    .py(px(1.0))
                    .rounded(px(4.0))
                    .bg(hsla(theme::SURFACE_ELEVATED))
                    .border_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .text_size(scale::px(11.0))
                    .text_color(hsla(theme::TEXT_DIM))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)).text_color(hsla(theme::TEXT)))
                    .child(match line {
                        Some(l) => format!("{label}:{l}"),
                        None => label,
                    })
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        v.open_location(path.clone(), line, cx);
                    })),
            );
        }
        if has_body {
            header = header
                .child(
                    div()
                        .flex_none()
                        .text_color(hsla(theme::TEXT_FAINT))
                        .child(icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)),
                )
                .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                    if !v.expanded_tools.remove(&toggle_id) {
                        v.expanded_tools.insert(toggle_id.clone());
                    }
                    v.remeasure(ix);
                    cx.notify();
                }));
        }

        let mut block = div().flex().flex_col().child(header);
        if expanded && has_body {
            let mut body = div().ml(px(14.0)).pl(px(10.0)).pb(px(6.0)).pt(px(2.0)).border_l_1().border_color(hsla(theme::BORDER)).flex().flex_col().gap(px(6.0));
            for content in &call.content {
                body = body.child(match content {
                    ToolContent::Text(text) => output(text, self.detail),
                    ToolContent::Diff { path, old, new } => diff(path, old.as_deref(), new, self.detail),
                });
            }
            if let Some(card) = self.cards.get(&call.id) {
                let scope = Scope { call: card.call.clone(), ui: self.card_ui.clone() };
                body = body.child(div().mt(px(2.0)).child(crate::visual::ui::markdown(format!("thread-card-{}", card.call), &card.content, scope)));
            }
            block = block.child(body);
        }
        block.into_any_element()
    }

    /// The calls in this conversation that run one of the harness's own subagents, oldest first.
    pub fn native_subagents(&self) -> Vec<&ToolCall> {
        self.thread.entries.iter().filter_map(|e| match e {
            Entry::Tool(call) if call.subagent.is_some() => Some(call),
            _ => None,
        }).collect()
    }

    /// Show call `id`'s step: unfold it and the run of steps it is in, and scroll the conversation to it.
    pub fn reveal_step(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(ix) = self.thread.tool_at(id) else { return };
        self.expanded_tools.insert(id.to_string());
        let start = match tool_run(&self.groupable(), ix) {
            Some(run) => {
                self.expanded_tools.insert(format!("run:{}", run.start));
                run.start
            }
            None => ix,
        };
        self.remeasure(start);
        self.list.scroll_to_reveal_item(ix);
        cx.notify();
    }

    /// Per entry: whether it is a step that folds into a run with its neighbours: a thought, a tool call or a subagent's
    /// report. A call with an inline card stays on its own, so the card is never hidden.
    pub(super) fn groupable(&self) -> Vec<bool> {
        self.thread.entries.iter().map(|e| match e {
            Entry::Thought(_) => true,
            Entry::Tool(call) => !self.cards.contains_key(&call.id),
            Entry::User(text) => parse_subagent_report(text).is_some(),
            _ => false,
        }).collect()
    }

    /// A run is open while Ctrl+O shows everything or after a click on its line; one step alone is always open. Runs
    /// only grow at their end, so the first entry's place names a run for as long as it lasts.
    fn run_open(&self, run: &std::ops::Range<usize>) -> bool {
        run.len() == 1 || self.detail || self.expanded_tools.contains(&format!("run:{}", run.start))
    }

    /// The dimmed line of a run of steps: what they are, and the latest one. Click to show each step.
    fn render_tool_run(&self, run: std::ops::Range<usize>, cx: &mut Context<Self>) -> AnyElement {
        let steps = &self.thread.entries[run.clone()];
        let calls: Vec<&ToolCall> = steps.iter().filter_map(|e| if let Entry::Tool(c) = e { Some(c) } else { None }).collect();
        if let [Entry::Tool(call)] = steps {
            return self.render_tool(run.start, call, cx);
        }
        let reports: Vec<SubagentReport> = steps.iter().filter_map(|e| if let Entry::User(t) = e { parse_subagent_report(t) } else { None }).collect();
        let thoughts = steps.len() - calls.len() - reports.len();
        let latest = match steps.last() {
            Some(Entry::Tool(call)) => title(call),
            Some(Entry::Thought(text)) => text.lines().next().unwrap_or_default().to_string(),
            Some(Entry::User(text)) => parse_subagent_report(text).map(|r| report_line(r.title, r.state)).unwrap_or_default(),
            _ => String::new(),
        };
        let open = self.run_open(&run);
        let failed = calls.iter().any(|c| c.status == ToolStatus::Failed) || reports.iter().any(|r| r.state == "failed");
        let key = format!("run:{}", run.start);
        let start = run.start;
        let header = div()
            .id(SharedString::from(format!("tool-run-{start}")))
            .w_full()
            .px(px(4.0))
            .py(px(2.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
            .text_size(scale::px(12.0))
            .text_color(hsla(if failed { theme::DANGER } else { theme::TEXT_FAINT }))
            .child(div().flex_none().text_color(hsla(if failed { theme::DANGER } else { theme::TEXT_DIM })).child(run_label(thoughts, calls.len() + reports.len())))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(latest))
            .child(div().flex_none().child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                if !v.expanded_tools.remove(&key) {
                    v.expanded_tools.insert(key.clone());
                }
                v.remeasure(start);
                v.focus_composer(window, cx);
                cx.notify();
            }));
        if !open {
            return header.into_any_element();
        }
        let first = match &steps[0] {
            Entry::Tool(call) => self.render_tool(start, call, cx),
            Entry::Thought(text) => self.render_thought(start, text, cx),
            Entry::User(text) if let Some(report) = parse_subagent_report(text) => self.render_subagent_report(start, &report, cx),
            _ => return header.into_any_element(),
        };
        div().flex().flex_col().gap(px(4.0)).child(header).child(indent_run_step(first)).into_any_element()
    }

    fn render_plan(&self) -> Option<Div> {
        let working = self.working && self.thread.plan.iter().any(|p| p.status != "completed");
        if self.thread.plan.is_empty() || !(self.plan_open || working) {
            return None;
        }
        let mut plan = div()
            .w_full()
            .max_w(px(COLUMN_MAX_W))
            .px(px(14.0))
            .py(px(10.0))
            .rounded(px(8.0))
            .bg(hsla(theme::SURFACE_ELEVATED))
            .border_1()
            .border_color(hsla(theme::PANEL_BORDER))
            .flex()
            .flex_col()
            .gap(px(3.0));
        for item in &self.thread.plan {
            let (mark, color) = match item.status.as_str() {
                "completed" => ("☒", theme::TEXT_FAINT),
                "in_progress" => ("◼", theme::ACCENT),
                _ => ("☐", theme::TEXT_DIM),
            };
            plan = plan.child(div().flex().gap(px(8.0)).text_size(scale::px(12.5)).text_color(hsla(color)).child(mark).child(item.text.clone()));
        }
        Some(div().w_full().px(px(22.0)).mb(px(8.0)).flex().justify_center().child(plan))
    }

    /// Compact bar above the composer showing anything currently running in the background for this session
    /// (running child subagents, or tool calls still in progress outside an active turn).
    fn render_background(&self, cx: &mut Context<Self>) -> Option<Div> {
        let bg_tools: Vec<String> = if self.working {
            vec![]
        } else {
            self.thread
                .entries
                .iter()
                .filter_map(|e| match e {
                    Entry::Tool(call) if call.status == ToolStatus::Running => Some(title(call)),
                    _ => None,
                })
                .collect()
        };
        if self.background.is_empty() && bg_tools.is_empty() {
            return None;
        }
        let mut pills = div().flex().flex_wrap().items_center().gap(px(6.0)).flex_1().min_w_0();
        for (i, task) in self.background.iter().enumerate() {
            let child_id = task.id.clone();
            let waiting = task.waiting;
            pills = pills.child(
                div()
                    .id(SharedString::from(format!("thread-bg-task-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .px(px(7.0))
                    .py(px(2.0))
                    .rounded(px(5.0))
                    .bg(hsla(theme::PANEL_BG))
                    .border_1()
                    .border_color(hsla(if waiting { theme::FOCUS_BORDER } else { theme::PANEL_BORDER }))
                    .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                    .cursor_pointer()
                    .text_size(scale::px(11.5))
                    .text_color(hsla(theme::TEXT))
                    .child(if waiting {
                        div().text_color(hsla(theme::ACCENT)).font_weight(FontWeight::SEMIBOLD).child("?").into_any_element()
                    } else {
                        spinner(format!("thread-bg-spin-{i}"), 10.0).into_any_element()
                    })
                    .child(div().max_w(px(220.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(task.label.clone()))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(ThreadEvent::Select(child_id.clone())))),
            );
        }
        for (i, name) in bg_tools.into_iter().enumerate() {
            pills = pills.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .px(px(7.0))
                    .py(px(2.0))
                    .rounded(px(5.0))
                    .bg(hsla(theme::PANEL_BG))
                    .border_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .text_size(scale::px(11.5))
                    .text_color(hsla(theme::TEXT_DIM))
                    .child(spinner(format!("thread-bg-tool-{i}"), 10.0))
                    .child(div().max_w(px(220.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(name)),
            );
        }
        let bar = div()
            .w_full()
            .max_w(px(COLUMN_MAX_W))
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .bg(hsla(theme::SURFACE_ELEVATED))
            .border_1()
            .border_color(hsla(theme::PANEL_BORDER))
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(div().flex_none().text_size(scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child("Running in background"))
            .child(pills);
        Some(div().w_full().px(px(22.0)).mb(px(6.0)).flex().justify_center().child(bar))
    }

    fn render_question(&self, cx: &mut Context<Self>) -> Option<Div> {
        let q = self.question.as_ref()?;
        let mut options = div().flex().flex_col().gap(px(6.0)).w_full();
        for (i, opt) in q.options.iter().enumerate() {
            let lit = i == q.highlight;
            let row = div()
                .id(SharedString::from(format!("thread-perm-{i}")))
                .w_full()
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(7.0))
                .bg(hsla(if lit { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
                .border_1()
                .border_color(hsla(if lit { theme::FOCUS_BORDER } else { theme::PANEL_BORDER }))
                .cursor_pointer()
                .on_hover(cx.listener(move |v, hovered: &bool, _, cx| {
                    if *hovered {
                        v.highlight_option(i, cx);
                    }
                }))
                .flex()
                .items_start()
                .gap(px(10.0))
                .child(div().pt(px(1.0)).child(key_badge(format!("{}", i + 1))))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(scale::px(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(theme::TEXT))
                                .child(opt.name.clone()),
                        )
                        .children(opt.description.clone().map(|desc| {
                            div()
                                .text_size(scale::px(11.5))
                                .text_color(hsla(theme::TEXT_DIM))
                                .child(desc)
                        })),
                )
                .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.answer(Some(i), cx)));
            options = options.child(row);
        }
        // Inside the message box, above where you type: a long list of options scrolls on its own.
        Some(
            div()
                .w_full()
                .px(px(12.0))
                .pt(px(10.0))
                .pb(px(8.0))
                .border_b_1()
                .border_color(hsla(theme::BORDER))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(q.title.clone()))
                .child(div().id("thread-question-options").max_h(px(240.0)).overflow_y_scroll().child(options))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.0))
                        .items_center()
                        .text_color(hsla(theme::TEXT_FAINT))
                        .text_size(scale::px(11.5))
                        .child(key_badge("↑↓"))
                        .child(key_badge("Enter"))
                        .child("or")
                        .child(key_badge("1-9"))
                        .child("to choose")
                        .child(key_badge("Esc"))
                        .child("to decline")
                        .when(q.takes_own_answer, |d| d.child("· or type your own answer below")),
                ),
        )
    }

    /// Universal floating picker dialog over the chat view: unified model + draggable effort picker, or permission mode selector.
    fn render_picker_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let active = self.picker?;
        let accent = hsla(theme::ACCENT);

        let (id, title, body) = match active {
            ThreadPicker::Model => {
                let current_id = self.thread.option("model").map(|m| m.current.as_str());
                let models = self.filtered_models(cx);
                let hi = self.model_highlight.min(models.len().saturating_sub(1));
                let mut list = div().id("picker-model-list").max_h(px(176.0)).overflow_y_scroll().track_scroll(&self.model_scroll).flex().flex_col().gap(px(2.0));
                for (n, m) in models.iter().enumerate() {
                    let selected = match &self.pending_model {
                        Some(p) => p.harness == m.harness && p.id == m.id,
                        None => m.harness == self.harness && current_id == Some(m.id.as_str()),
                    };
                    let focused = n == hi;
                    let choice = m.clone();
                    let logo = logos::draw(logos::for_harness(m.harness), 11.5, hsla(if selected { theme::TEXT } else { theme::TEXT_FAINT })).into_any_element();
                    list = list.child(
                        picker::row(SharedString::from(format!("thread-model-{n}")), Some(logo), m.name.clone(), None, None, selected, false)
                            .when(focused && !selected, |d| d.bg(hsla(theme::SURFACE_HOVER)))
                            .border_1()
                            .border_color(if focused { hsla(theme::FOCUS_BORDER) } else { gpui::transparent_black() })
                            .debug_selector(move || format!("thread-model-{n}"))
                            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                                v.pick_model(choice.clone(), cx);
                                v.close_picker(window, cx);
                            })),
                    );
                }
                if models.is_empty() {
                    list = list.child(picker::empty_note("No models match."));
                }

                let (effort_values, effort_labels, current_effort_ix, option_id) = self.effort_choices();
                let values_rc = Rc::new(effort_values);
                let on_pick: EffortPick<Self> = Rc::new(move |v, ix, _, cx| {
                    let Some(val) = values_rc.get(ix).cloned() else { return };
                    v.pending_effort = Some(val.clone());
                    if let Some(id) = &option_id {
                        v.set_option(id.clone(), val, cx);
                    }
                    cx.notify();
                });

                let effort_section = div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(picker::section_header(
                        "EFFORT",
                        Some(div().text_size(scale::px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child("←/→ or drag").into_any_element()),
                    ))
                    .child(effort_slider(
                        "thread-effort",
                        &effort_labels,
                        Some(current_effort_ix),
                        accent,
                        self.effort_bounds.clone(),
                        self.effort_dragging.clone(),
                        on_pick,
                        cx,
                    ));

                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(picker::search_input(&self.model_search))
                    .child(list)
                    .child(picker::divider())
                    .child(effort_section);
                ("thread-model-dialog", "MODEL", body.into_any_element())
            }
            ThreadPicker::Permission => {
                let mut rows = div().id("picker-perm-list").max_h(px(340.0)).overflow_y_scroll().flex().flex_col().gap(px(2.0));
                if self.thread.modes.is_empty() {
                    rows = rows.child(picker::empty_note("No permission modes available."));
                } else {
                    let hi = self.mode_highlight.min(self.thread.modes.len().saturating_sub(1));
                    for (i, (id, name)) in self.thread.modes.iter().enumerate() {
                        let current = self.thread.mode.as_deref() == Some(id.as_str());
                        let focused = i == hi;
                        let mode_id = id.clone();
                        let display = picker::format_mode_name(id, name);
                        let lead = icon(IconName::Lock, 12.0).text_color(hsla(if current { theme::TEXT } else { theme::TEXT_FAINT })).into_any_element();
                        rows = rows.child(
                            picker::row(
                                SharedString::from(format!("perm-mode-{i}")),
                                Some(lead),
                                display,
                                Some(picker::permission_description(id).into()),
                                None,
                                current,
                                false,
                            )
                            .when(focused && !current, |d| d.bg(hsla(theme::SURFACE_HOVER)))
                            .border_1()
                            .border_color(if focused { hsla(theme::FOCUS_BORDER) } else { gpui::transparent_black() })
                            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                                v.set_mode(mode_id.clone(), cx);
                                v.close_picker(window, cx);
                            })),
                        );
                    }
                }
                ("thread-permission-dialog", "PERMISSION MODE", rows.into_any_element())
            }
        };

        Some(picker::dropdown_overlay(
            (id, title),
            self.picker_at,
            22,
            body,
            None,
            |v: &mut Self, window, cx| v.close_picker(window, cx),
            window,
            cx,
        ))
    }

    fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let mut inner = div().w_full().max_w(px(COLUMN_MAX_W)).flex().flex_col().gap(px(6.0));

        let commands = self.matching_commands(cx);
        if !commands.is_empty() {
            let highlight = self.command_highlight.min(commands.len() - 1);
            let mut menu = div()
                .id("thread-commands")
                .max_h(px(240.0))
                .overflow_y_scroll()
                .track_scroll(&self.command_scroll)
                .flex()
                .flex_col()
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(8.0))
                .bg(hsla(theme::SURFACE_ELEVATED))
                .border_1()
                .border_color(hsla(theme::PANEL_BORDER));
            for (n, c) in commands.into_iter().enumerate() {
                let name = c.name.clone();
                menu = menu.child(
                    div()
                        .id(SharedString::from(format!("thread-cmd-{n}")))
                        .flex()
                        .gap(px(10.0))
                        .px(px(6.0))
                        .py(px(3.0))
                        .rounded(px(5.0))
                        .cursor_pointer()
                        .when(n == highlight, |d| d.bg(hsla(theme::SURFACE_HOVER)))
                        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                        .child(div().flex_none().text_color(hsla(theme::TEXT)).child(format!("/{}", c.name)))
                        .child(div().flex_1().truncate().text_color(hsla(theme::TEXT_DIM)).child(c.description.clone()))
                        .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                            v.set_draft(&format!("/{name} "), window, cx);
                            cx.notify();
                        })),
                );
            }
            inner = inner.child(menu);
        }

        if let Some(target) = &self.pending_model {
            inner = inner.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.0))
                    .py(px(5.0))
                    .rounded(px(7.0))
                    .bg(hsla(theme::SURFACE_ELEVATED))
                    .border_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(scale::px(11.5))
                            .text_color(hsla(theme::TEXT))
                            .child(logos::draw(logos::for_harness(target.harness), 11.5, hsla(theme::TEXT_DIM)))
                            .child(format!("Next message switches to {}", target.name)),
                    )
                    .child(
                        div()
                            .id("cancel-handoff-stage")
                            .cursor_pointer()
                            .text_color(hsla(theme::TEXT_DIM))
                            .hover(|s| s.text_color(hsla(theme::TEXT)))
                            .child(icon(IconName::X, 12.0))
                            .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                                v.pending_model = None;
                                cx.notify();
                            })),
                    ),
            );
        }

        let focused = self.composer.focus_handle(cx).is_focused(window);
        let input = composer::input_area()
            .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| v.composer.update(cx, |c, cx| c.focus(window, cx))))
            .child(Textarea::new(&self.composer).appearance(false))
            .into_any_element();
        let dock = composer::render(self.composer_parts(focused, input, cx));

        div()
            .flex_none()
            .w_full()
            .px(px(22.0))
            .pb(px(12.0))
            .pt(px(4.0))
            .flex()
            .justify_center()
            .child(inner.children(self.render_waiting(cx)).child(dock))
    }

    /// The user's messages waiting for the turn to end, above the message box, each one editable or removable
    /// before it is sent.
    fn render_waiting(&self, cx: &mut Context<Self>) -> Option<Div> {
        let waiting = self.waiting_messages();
        let steering: Vec<String> = self.unread_steers().map(first_line).collect();
        if waiting.is_empty() && steering.is_empty() {
            return None;
        }
        let steering_rows = steering.into_iter().enumerate().map(|(i, line)| {
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(4.0))
                .child(spinner(SharedString::from(format!("steering-{i}")), 12.0))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(scale::px(12.5)).text_color(hsla(theme::TEXT_DIM)).child(line))
                .child(div().flex_none().text_size(scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child("Read after the current step"))
        });
        let waiting_empty = waiting.is_empty();
        let rows = waiting.into_iter().map(|(at, text)| {
            let first_line = first_line(text);
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(4.0))
                .child(icon(IconName::Clock, 12.0).text_color(hsla(theme::TEXT_FAINT)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(scale::px(12.5)).text_color(hsla(theme::TEXT_DIM)).child(first_line))
                .child(
                    icon_button(SharedString::from(format!("queued-edit-{at}")), IconName::Pencil)
                        .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| v.edit_queued(at, window, cx))),
                )
                .child(
                    icon_button(SharedString::from(format!("queued-remove-{at}")), IconName::X)
                        .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| drop(v.unqueue(at, cx)))),
                )
        });
        Some(
            div()
                .w_full()
                .py(px(4.0))
                .rounded(px(10.0))
                .bg(hsla(theme::SURFACE_ELEVATED))
                .border_1()
                .border_color(hsla(theme::PANEL_BORDER))
                .flex()
                .flex_col()
                .children(steering_rows)
                .when(!waiting_empty, |d| d.child(div().px(px(10.0)).pt(px(2.0)).text_size(scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child("Queued for after this turn")))
                .children(rows),
        )
    }

    /// Assemble the shared message box parts: model, permission mode, branch, review and send. What the agent is
    /// doing shows in the chat (`render_activity`) and what waits shows above the box (`render_waiting`).
    fn composer_parts(&self, focused: bool, input: AnyElement, cx: &mut Context<Self>) -> composer::Composer {
        let active_harness = self.pending_model.as_ref().map_or(self.harness, |m| m.harness);
        let (_, effort_labels, effort_ix, _) = self.effort_choices();
        let effort_badge = effort_labels.get(effort_ix).cloned();

        let model = composer::model_chip("thread-model", self.picker == Some(ThreadPicker::Model), &self.model_anchor, active_harness, self.active_model_label(), effort_badge)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    v.toggle_model_picker(window, cx);
                    cx.notify();
                }),
            )
            .into_any_element();

        let permission = self.thread.mode.as_deref().map(|mode_id| {
            let raw_name = self.thread.mode_name().unwrap_or(mode_id);
            composer::permission_chip("thread-mode", self.picker == Some(ThreadPicker::Permission), mode_id, raw_name)
                .dropdown_above(cx, |v, at, window, cx| {
                    v.picker = Some(ThreadPicker::Permission);
                    v.picker_at = at;
                    v.mode_highlight = v.thread.modes.iter().position(|(id, _)| v.thread.mode.as_deref() == Some(id.as_str())).unwrap_or(0);
                    v.focus_composer(window, cx);
                    cx.notify();
                })
                .into_any_element()
        });

        let git = self.git.as_ref().map(|git| {
            let tail = git.busy.then(|| spinner("thread-git-busy", 11.0).into_any_element());
            composer::git_chip("thread-git", git.open, git.primary.clone(), git.linked.clone(), tail)
                .dropdown_above(cx, |_, at, _, cx| cx.emit(ThreadEvent::OpenGitPicker(at)))
                .into_any_element()
        });

        let extra_chips: Vec<AnyElement> = vec![];
        let mut extra_actions: Vec<AnyElement> = vec![];
        // Outside a git repository there are no snapshots, and so no review at all.
        if !self.snapshots.is_empty() {
            let chip = if self.can_review() {
                composer::chip("thread-review-open", false)
                    .text_color(hsla(theme::TEXT_DIM))
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_review(cx)))
            } else {
                composer::disabled_chip("thread-review-open")
            };
            extra_actions.push(chip.child("Review changes").into_any_element());
        }
        let can_send = composer::can_send(self.composer.read(cx).value().as_ref());
        let send = composer::send_button("thread-send", can_send)
            .when(can_send, |d| d.on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.submit(window, cx))))
            .into_any_element();

        composer::Composer {
            lit: focused || self.question.is_some(),
            top: self.render_question(cx).map(IntoElement::into_any_element),
            input,
            model,
            permission,
            project: None,
            git,
            extra_chips,
            extra_actions,
            send,
        }
    }
}

pub fn short_effort_label(name: &str) -> String {
    match name.trim().to_lowercase().as_str() {
        "low" => "Low".into(),
        "medium" | "med" => "Med".into(),
        "high" => "High".into(),
        "xhigh" | "extra high" | "extra_high" => "XHigh".into(),
        "max" | "maximum" => "Max".into(),
        other => {
            let mut c = other.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        }
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
    let mut block = mono().pl(px(14.0)).text_color(hsla(theme::TEXT_DIM)).flex().flex_col();
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
    let mut block = mono().rounded(px(6.0)).border_1().border_color(hsla(theme::PANEL_BORDER)).flex().flex_col().overflow_hidden();
    block = block.child(div().px(px(8.0)).py(px(3.0)).bg(hsla(theme::APP_BG)).text_color(hsla(theme::TEXT_DIM)).child(path.display().to_string()));
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

/// A report's line among the steps, such as `Codex subagent reported · done`, from its `state` key.
fn report_line(title: &str, state: &str) -> String {
    let state = match state {
        "completed" => "done",
        "interrupted" => "interrupted",
        _ => "failed",
    };
    format!("{title} reported · {state}")
}

/// What a run's line says it holds.
fn run_label(thoughts: usize, calls: usize) -> String {
    let calls = match calls {
        0 => None,
        1 => Some("1 tool call".to_string()),
        n => Some(format!("{n} tool calls")),
    };
    match (thoughts, calls) {
        (0, Some(calls)) => calls,
        (_, Some(calls)) => format!("Thought, {calls}"),
        (_, None) => "Thought".to_string(),
    }
}

/// The run of foldable steps (`groupable`) that entry `ix` belongs to, or None when it is not one.
fn tool_run(groupable: &[bool], ix: usize) -> Option<std::ops::Range<usize>> {
    if !groupable.get(ix).copied().unwrap_or(false) {
        return None;
    }
    let start = (0..ix).rev().take_while(|&i| groupable[i]).last().unwrap_or(ix);
    let end = (ix..groupable.len()).take_while(|&i| groupable[i]).last().map_or(ix + 1, |i| i + 1);
    Some(start..end)
}

#[cfg(test)]
mod tests {
    use super::{keep_line_breaks, report_line, run_label, tool_run};

    #[test]
    fn a_users_lines_stay_lines_but_code_and_paragraphs_do_not_change() {
        assert_eq!(keep_line_breaks("one\ntwo\nthree"), "one  \ntwo  \nthree");
        assert_eq!(keep_line_breaks("para\n\nnext"), "para\n\nnext", "a blank line already splits them");
        assert_eq!(keep_line_breaks("look:\n```\na\nb\n```\nafter"), "look:  \n```\na\nb\n```\nafter");
        assert_eq!(keep_line_breaks("single"), "single");
    }

    #[test]
    fn a_runs_line_says_what_it_holds() {
        assert_eq!(run_label(0, 3), "3 tool calls");
        assert_eq!(run_label(2, 1), "Thought, 1 tool call");
        assert_eq!(run_label(2, 0), "Thought");
    }

    #[test]
    fn a_reports_line_names_the_subagent_and_how_it_ended() {
        assert_eq!(report_line("Codex subagent", "completed"), "Codex subagent reported · done");
        assert_eq!(report_line("Fix login", "interrupted"), "Fix login reported · interrupted");
        assert_eq!(report_line("Fix login", "failed"), "Fix login reported · failed");
    }

    #[test]
    fn consecutive_tool_calls_share_one_run() {
        // agent, tool, tool, tool, agent, tool, tool-with-card, tool
        let groupable = [false, true, true, true, false, true, false, true];
        assert_eq!(tool_run(&groupable, 1), Some(1..4));
        assert_eq!(tool_run(&groupable, 3), Some(1..4), "a later call of the run finds the same run");
        assert_eq!(tool_run(&groupable, 5), Some(5..6), "a call with a card ends the run before it");
        assert_eq!(tool_run(&groupable, 7), Some(7..8));
        assert_eq!(tool_run(&groupable, 0), None, "not a tool call");
        assert_eq!(tool_run(&groupable, 6), None, "a call with a card stands on its own");
    }
}
