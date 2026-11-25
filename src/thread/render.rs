//! How a thread is drawn: a readable conversation column, active plan/permission prompts, an elevated composer dock
//! with a unified model + draggable effort picker (aggregating every enabled harness) and a permission selector dialog.

use std::rc::Rc;

use base64::Engine;

use gpui::{
    AnyElement, ClickEvent, Context, Div, Focusable, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Window, anchored, deferred, div, linear_color_stop, linear_gradient, list, prelude::FluentBuilder, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Paste, Textarea};
use gpui_kit::component::text::{TextView, TextViewState};
use std::collections::HashMap;
use gpui::{AppContext, Entity};

use super::{ThreadEvent, ThreadPicker, ThreadView, elements};
use crate::harness::CatalogModel;
use crate::harness::permission::Permission;
use super::timeline::{self, RunKind};
use crate::harness::thread::{Entry, SubagentReport, ToolCall, ToolContent, ToolStatus, parse_subagent_report};
use crate::panels::{attachments, comment_box, composer, logos, picker, scale};
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{EffortPick, Trigger, button, button_colored, button_disabled, button_disabled_leading, effort_slider, icon, icon_button, key_badge, spinner};
use crate::visual::ui::Scope;

/// Lines of a tool's output shown while it is folded (Ctrl+O shows all).
const FOLDED_LINES: usize = 6;
/// Shared max width for conversation entries, plan/question cards, and the bottom composer so edges align cleanly.
pub(super) const COLUMN_MAX_W: f32 = 840.0;

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
                    if v.picker != Some(ThreadPicker::Model) && v.review.is_none() && v.screenshot.is_none() && v.find.is_none() && v.selection_comment.is_none() {
                        v.focus_composer(window, cx);
                    }
                }),
            );
        if let Some(review) = self.render_review(window, cx) {
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
            .on_mouse_down(MouseButton::Left, cx.listener(|v, _: &MouseDownEvent, _, cx| {
                let mut changed = v.chat_menu.take().is_some();
                if v.selection_comment.is_none() && v.comment_anchor.take().is_some() {
                    changed = true;
                }
                if changed {
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(|v, ev: &MouseDownEvent, window, cx| {
                let quote = v.selected_quote(cx);
                let anchor = gpui_kit::base::TextSelection::selection_edges(window, cx)
                    .map(|(_, end)| gpui::point(end.right() + px(8.0), end.top()))
                    .unwrap_or(ev.position);
                v.chat_menu = Some(super::ChatMenu { at: ev.position, quote, anchor });
                cx.notify();
            }))
            .on_scroll_wheel(cx.listener(|v, _: &gpui::ScrollWheelEvent, _, cx| {
                let mut changed = v.chat_menu.take().is_some();
                if v.selection_comment.is_none() && v.comment_anchor.take().is_some() {
                    changed = true;
                }
                if changed {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|v, event: &gpui::MouseUpEvent, window, cx| {
                    if v.selection_comment.is_none() {
                        let quote = v.selected_quote(cx);
                        let anchor = gpui_kit::base::TextSelection::selection_edges(window, cx)
                            .map(|(_, end)| gpui::point(end.right() + px(8.0), end.top()))
                            .unwrap_or_else(|| event.position + gpui::point(px(8.0), px(8.0)));
                        v.comment_anchor = (!quote.is_empty()).then_some((anchor, quote));
                    }
                    if v.picker != Some(ThreadPicker::Model)
                        && v.review.is_none() && v.find.is_none()
                        && v.selection_comment.is_none() && v.comment_anchor.is_none()
                    {
                        v.focus_composer(window, cx);
                    }
                    cx.notify();
                }),
            )
            .child(conversation)
            .child(self.chat_glide.overlay(&self.list, px(64.0), self.picker.is_none() && self.screenshot.is_none()))
            .child(div().absolute().bottom_0().left_0().right_0().h(px(18.0)).bg(fade))
            .child(gpui_kit::base::Scrollbar::vertical(&self.list).mode(gpui_kit::base::ScrollbarMode::Always));
        let show_jump = self.show_jump_to_latest();
        self.jump_visible.set(show_jump);
        let conversation = conversation.when(show_jump, |view| {
            view.child(div().absolute().bottom(px(22.0)).right(px(24.0)).child(
                button_colored("jump-to-latest", Some(IconName::ArrowDownToLine), "Jump to latest", hsla(theme::ACCENT), hsla(theme::PANEL_BG), hsla(0xdcdcdc), None, true).debug_selector(|| "jump-to-latest".into()).on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                    v.chat_glide.cancel();
                    v.list.set_follow_mode(gpui::FollowMode::Tail);
                    v.jump_visible.set(false);
                    cx.notify();
                })),
            ))
        });
        let conversation = div().flex_1().min_h_0().flex().flex_col().child(conversation).children(self.render_activity());
        let bottom_dock = div()
            .flex_none()
            .flex()
            .flex_col()
            .children(self.render_session_notices())
            .children(self.render_plan())
            .children(self.render_background(window, cx))
            .child(self.render_composer(window, cx));
        root.children(self.render_find(cx))
            .child(conversation)
            .child(bottom_dock)
            .children(self.render_selection_comment(cx))
            .children(self.render_chat_menu(window, cx))
            .children(self.render_picker_overlay(window, cx))
            .children(self.render_screenshot(cx))
    }
}

impl ThreadView {
    fn render_selection_comment(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (at, _) = self.comment_anchor.as_ref()?;
        let content = if let Some(comment) = &self.selection_comment {
            div().id("selection-comment-input").w(px(320.0)).occlude()
                .on_key_down(cx.listener(|v, ev: &gpui::KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "escape" {
                        v.dismiss_selection_comment(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_mouse_down_out(cx.listener(|v, _: &MouseDownEvent, _, cx| {
                    if v.selection_comment.as_ref().is_some_and(|comment| comment.input.read(cx).value().trim().is_empty()) {
                        v.selection_comment = None;
                        v.comment_anchor = None;
                        cx.notify();
                    }
                }))
                .child(comment_box::comment_box(&comment.input, "adds it to your message", "sends it"))
                .into_any_element()
        } else {
            div().occlude().child(
                comment_box::comment_trigger("comment-on-selection", true)
                    .debug_selector(|| "comment-on-selection".into())
                    .on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.start_selection_comment(window, cx))),
            )
            .into_any_element()
        };
        Some(deferred(anchored().position(*at).snap_to_window_with_margin(px(8.0)).child(content))
            .with_priority(20).into_any_element())
    }

    fn render_chat_menu(&self, _window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu_state = self.chat_menu.as_ref()?;
        let at = menu_state.at;
        let quote = menu_state.quote.clone();
        let has_selection = !quote.is_empty();
        let selection_anchor = menu_state.anchor;
        let copy_quote = quote.clone();
        let comment_quote = quote;
        let menu_item = |id: &'static str, glyph: IconName, label: &'static str, enabled: bool| {
            crate::panels::widgets::focusable(id)
                .disabled(!enabled)
                .w_full()
                .h(px(28.0))
                .px(px(10.0))
                .rounded(px(5.0))
                .justify_start()
                .gap(px(8.0))
                .text_size(scale::px(12.5))
                .text_color(hsla(if enabled { theme::TEXT } else { theme::TEXT_FAINT }))
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(hsla(theme::ROW_HOVER))))
                .child(icon(glyph, 13.0).text_color(hsla(if enabled { theme::TEXT_DIM } else { theme::TEXT_FAINT })))
                .child(label)
        };
        let menu = div()
            .id("chat-context-menu")
            .occlude()
            .w(px(180.0))
            .p(px(4.0))
            .rounded(px(8.0))
            .bg(hsla(theme::SURFACE_ELEVATED))
            .border_1()
            .border_color(hsla(theme::PANEL_BORDER))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                gpui_kit::base::GlobalState::suppress_text_selection(cx);
            })
            .on_mouse_down_out(cx.listener(|v, _: &MouseDownEvent, _, cx| {
                v.chat_menu = None;
                cx.notify();
            }))
            .child(menu_item("chat-ctx-copy", IconName::Copy, "Copy", has_selection).when(has_selection, |b| {
                b.on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_quote.clone()));
                    v.chat_menu = None;
                    cx.notify();
                }))
            }))
            .child(menu_item("chat-ctx-comment", IconName::MessageSquare, "Add comment", has_selection)
                .debug_selector(|| "chat-ctx-comment".into())
                .when(has_selection, |b| {
                b.on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                    v.chat_menu = None;
                    v.comment_anchor = Some((selection_anchor, comment_quote.clone()));
                    v.start_selection_comment(window, cx);
                }))
            }))
            .child(menu_item("chat-ctx-paste", IconName::Clipboard, "Paste", true).on_click(cx.listener(|v, _: &ClickEvent, window, cx| {
                v.chat_menu = None;
                if !(v.attachments_enabled() && v.attachments.paste(cx))
                    && let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                {
                    v.paste_text(&text, window, cx);
                } else {
                    v.focus_composer(window, cx);
                }
                cx.notify();
            })));
        Some(deferred(anchored().position(at).snap_to_window_with_margin(px(8.0)).child(menu)).with_priority(25).into_any_element())
    }
}

/// The user's message as markdown that keeps each line they typed. Markdown would join lines with a single break
/// between them into one, so every line before another ends in a hard break (two trailing spaces, which unlike a
/// backslash leave nothing behind when the next line starts a list or a fence), except inside a fenced code block,
/// where lines already stay as they are.
pub(super) fn keep_line_breaks(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    // The open fence's marker character and length: only a run of that character at least as long closes it.
    let mut open: Option<(char, usize)> = None;
    let mut out = String::with_capacity(text.len() + lines.len());
    for (i, line) in lines.iter().enumerate() {
        let marker = fence_marker(line);
        let fence = match (open, marker) {
            (None, Some(m)) => {
                open = Some(m);
                true
            }
            (Some((c, n)), Some((mc, mn))) if mc == c && mn >= n => {
                open = None;
                true
            }
            _ => false,
        };
        let in_fence = open.is_some();
        out.push_str(line);
        let Some(next) = lines.get(i + 1) else { break };
        let breaks = !in_fence && !fence && !line.trim().is_empty() && !next.trim().is_empty();
        out.push_str(if breaks { "  \n" } else { "\n" });
    }
    out
}

/// Ensure entry `ix`'s cached `TextViewState` exists, starts parsing `text` immediately, and remeasures row `ix`
/// whenever a background parse (streaming append or >4 KB message) commits new rendered text.
pub(super) fn ensure_markdown_state(
    cache: &mut HashMap<usize, Entity<TextViewState>>,
    ix: usize,
    text: &str,
    cx: &mut Context<ThreadView>,
) -> Entity<TextViewState> {
    let state = cache
        .entry(ix)
        .or_insert_with(|| {
            let entity = cx.new(|cx| TextViewState::markdown(text, cx));
            let source_key = |state: &Entity<TextViewState>, app: &gpui::App| {
                let rendered = state.read(app).rendered_text();
                let source = rendered.source();
                (source.as_ptr() as usize, source.len())
            };
            let mut seen = source_key(&entity, cx);
            cx.observe(&entity, move |view, state, cx| {
                let current = source_key(&state, cx);
                if current != seen
                    && view.markdown.get(&ix).is_some_and(|cached| cached.entity_id() == state.entity_id())
                {
                    seen = current;
                    view.remeasure(ix);
                    cx.notify();
                }
            })
            .detach();
            entity
        })
        .clone();
    // Unchanged text skips `update` altogether so scrolling never triggers entity writes.
    if state.read(cx).rendered_text().source() != text {
        state.update(cx, |s, cx| s.set_text(text, cx));
    }
    state
}

/// Entry `ix`'s text as markdown, parsed once and kept in `cache` (`ThreadView::markdown`) so it keeps its height
/// as it scrolls.
pub(super) fn markdown_of(cache: &mut HashMap<usize, Entity<TextViewState>>, ix: usize, text: &str, cx: &mut Context<ThreadView>) -> TextView {
    let state = ensure_markdown_state(cache, ix, text, cx);
    crate::github::ui::styled_markdown(TextView::new(&state))
}

fn chat_links(view: TextView, row: usize, cx: &mut Context<ThreadView>) -> TextView {
    let thread = cx.entity().downgrade();
    view.on_link_click(move |target, _, _, app| {
        if let Ok(url) = url::Url::parse(target)
            && matches!(url.scheme(), "http" | "https") && url.has_host()
        {
            app.open_url(url.as_str());
        } else if let Some((path, line)) = timeline::file_link(target) {
            let _ = thread.update(app, |view, cx| view.open_location_at(row, path, line, cx));
        }
    })
}

fn chat_markdown(cache: &mut HashMap<usize, Entity<TextViewState>>, ix: usize, text: &str, cx: &mut Context<ThreadView>) -> TextView {
    let view = markdown_of(cache, ix, text, cx);
    chat_links(view, ix, cx)
}

/// A line that opens or closes a fenced code block: its marker character and how many of it there are.
fn fence_marker(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let c = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = trimmed.chars().take_while(|x| *x == c).count();
    (n >= 3).then_some((c, n))
}

/// A message's first line with text, for a one-line row.
fn first_line(text: &str) -> String {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_string()
}

fn indent_run_step(el: AnyElement) -> AnyElement {
    elements::trace_content(el)
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
        if let Some(initial) = &self.initial_model {
            if let Some(model) = self.thread.option("model")
                && let Some((_, name)) = model.choices.iter().find(|(v, _)| v == initial)
            {
                return name.clone();
            }
            if let Some(found) = self.all_models.iter().find(|m| m.harness == self.harness && m.id == *initial) {
                return found.name.clone();
            }
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
                let run = self.tool_run(ix);
                match &run {
                    Some(run) if run.start == ix && run.len() > 1 => return self.wrap_entry(ix, self.render_tool_run(run.clone(), cx)),
                    Some(run) if !self.run_open(run) => return div().into_any_element(),
                    _ => {}
                }
                let el = self.render_subagent_report(ix, &report, cx);
                if run.is_some_and(|r| r.len() > 1) { indent_run_step(el) } else { el }
            }
            Entry::User(text) => {
                let md_view = if self.user_md_len.get(&ix) == Some(&text.len())
                    && let Some(state) = self.markdown.get(&ix)
                {
                    chat_links(crate::github::ui::styled_markdown(TextView::new(state)), ix, cx)
                } else {
                    self.user_md_len.insert(ix, text.len());
                    let formatted = keep_line_breaks(text);
                    chat_markdown(&mut self.markdown, ix, &formatted, cx)
                };
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .px(px(14.0))
                    .py(px(10.0))
                    .rounded(px(9.0))
                    .bg(hsla(theme::ROW_ACTIVE))
                    .border_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .child(div().w_full().min_w_0().child(md_view))
                    .into_any_element()
            }
            Entry::Agent(text) => {
                let cached = self.agent_cache.entry(ix).or_insert_with(|| super::AgentCache {
                    text_len: text.len(),
                    citations: elements::extract_citations(text),
                });
                if cached.text_len != text.len() {
                    cached.text_len = text.len();
                    cached.citations = elements::extract_citations(text);
                }
                let citations = elements::inline_citations_row(ix, cached.citations.clone());
                let mut view = chat_markdown(&mut self.markdown, ix, text, cx);
                if self.inline_visuals {
                    view = crate::visual::ui::transcript(view, Scope { call: format!("entry-{ix}"), ui: self.card_ui.clone() });
                }
                div().px(px(2.0)).flex().flex_col().gap(px(6.0)).child(view).children(citations).into_any_element()
            }
            Entry::WorkspaceBoundary(_) => return div().into_any_element(),
            Entry::Thought(text) => {
                let run = self.tool_run(ix);
                match &run {
                    Some(run) if run.start == ix && run.len() > 1 => return self.wrap_entry(ix, self.render_tool_run(run.clone(), cx)),
                    Some(run) if !self.run_open(run) => return div().into_any_element(),
                    _ => {}
                }
                let in_group = run.is_some_and(|r| r.len() > 1);
                let el = self.render_thought(ix, text, cx);
                if in_group { indent_run_step(el) } else { el }
            }
            Entry::Tool(call) => match self.tool_run(ix) {
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
        let status = match report.state {
            "completed" => elements::TraceStatus::Done,
            "interrupted" => elements::TraceStatus::Ended,
            _ => elements::TraceStatus::Error,
        };
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
        let header = elements::trace_line(
            SharedString::from(format!("report-row-{ix}")),
            status,
            logos::draw(logos::for_harness(harness), 12.0, hsla(theme::TEXT_DIM)).into_any_element(),
            report.title.to_string(),
            None,
            vec![elements::trace_badge(
                match status {
                    elements::TraceStatus::Done => "Completed",
                    elements::TraceStatus::Ended => "Interrupted",
                    _ => "Failed",
                },
                if status == elements::TraceStatus::Error { theme::DANGER } else { theme::TEXT_DIM },
                theme::SURFACE_ELEVATED,
                theme::PANEL_BORDER,
            ), open.into_any_element()],
            true,
            expanded,
        )
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
            let view = chat_links(crate::github::ui::markdown(format!("thread-md-{ix}"), report.body), ix, cx);
            block = block.child(elements::trace_content(view.into_any_element()));
        }
        block.into_any_element()
    }

    /// Handoff, compaction, or active working status fixed above the composer for the entire duration of the turn.
    fn render_activity(&self) -> Option<AnyElement> {
        let (label, stoppable) = match self.starting_label() {
            Some(label) => (label, false),
            None if self.is_working() && self.compaction.running => ("Compacting…".to_string(), true),
            None if self.is_working() || self.has_active_turn() => ("Working…".to_string(), true),
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
            .child(div().w_full().max_w(px(COLUMN_MAX_W))
                .when(self.find_query_for_row(ix).is_some(), |row| row.rounded(px(5.0)).bg(hsla(theme::SURFACE_ELEVATED)))
                .child(body))
            .into_any_element()
    }

    /// Reasoning step: collapsed by default (with a live spinner while streaming, then `"Thought for Ns"` once done).
    fn render_thought(&self, ix: usize, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let key = format!("thought:{ix}");
        let timing = self.thought_timing.get(&ix);
        let streaming = self.is_working()
            && ix + 1 == self.thread.entries.len()
            && timing.is_some_and(|(_, done)| done.is_none());
        let duration = timing.and_then(|(_, done)| *done);
        let expanded = (self.detail && !self.collapsed_thoughts.contains(&ix))
            || self.expanded_tools.contains(&key);
        let preview = first_line(text);
        let header = elements::reasoning_header(ix, preview, streaming, duration, expanded).on_click(cx.listener(
            move |v, _: &ClickEvent, window, cx| {
                if v.detail {
                    if !v.collapsed_thoughts.remove(&ix) {
                        v.expanded_tools.remove(&key);
                        v.collapsed_thoughts.insert(ix);
                    }
                } else if !v.expanded_tools.remove(&key) {
                    v.expanded_tools.insert(key.clone());
                }
                v.remeasure(ix);
                v.focus_composer(window, cx);
                cx.notify();
            },
        ));
        let mut block = div().flex().flex_col().child(header);
        if expanded && !text.trim().is_empty() {
            block = block.child(elements::reasoning_body(text));
        }
        block.into_any_element()
    }

    /// Tool call row built on `Trace` + `CodeBlock`: kind icon, status badge, file location chips, and collapsible output.
    fn render_tool(&self, ix: usize, call: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let details = &call.details;
        let has_details = call.content.iter().any(|item| !matches!(item, ToolContent::Diff { .. }))
            || details.command.is_some() || details.labeled_output().next().is_some();
        let expanded = (self.detail && !self.collapsed_shell_calls.contains(&call.id))
            || self.expanded_tools.contains(&call.id);
        let toggle_id = call.id.clone();

        let mut location_chips = Vec::new();
        for (n, (path, line)) in call.locations.iter().enumerate().take(2) {
            let (path, line) = (path.clone(), *line);
            let label = relative_to_cwd(timeline::workspace_at(&self.thread.entries, ix, &self.cwd), &path);
            let end_line = (n == 0).then_some(details.end_line).flatten();
            location_chips.push(
                div()
                    .id(SharedString::from(format!("tool-loc-{ix}-{n}")))
                    .debug_selector(move || format!("tool-loc-{ix}-{n}"))
                    .flex_none()
                    .max_w(px(240.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
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
                    .child(match (line, end_line) {
                        (Some(start), Some(end)) if end > start => format!("{label}:L{start}-L{end}"),
                        (Some(line), _) => format!("{label}:L{line}"),
                        _ => label,
                    })
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        v.open_location_at(ix, path.clone(), line, cx);
                    }))
                    .into_any_element(),
            );
        }

        if let Some(ms) = details.elapsed_ms {
            location_chips.push(div().text_color(hsla(theme::TEXT_FAINT))
                .child(elements::format_duration(std::time::Duration::from_millis(ms))).into_any_element());
        }
        if let Some(code) = details.exit_code {
            let color = if code == 0 { theme::OK } else { theme::DANGER };
            location_chips.push(div().text_color(hsla(color)).child(format!("Exit {code}")).into_any_element());
        }
        let mut header = elements::tool_header(ix, call, title_in_cwd(call, &self.cwd), location_chips, has_details, expanded);
        if has_details {
            header = header.on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                if v.detail {
                    if !v.collapsed_shell_calls.remove(&toggle_id) {
                        v.expanded_tools.remove(&toggle_id);
                        v.collapsed_shell_calls.insert(toggle_id.clone());
                    }
                } else if !v.expanded_tools.remove(&toggle_id) {
                    v.expanded_tools.insert(toggle_id.clone());
                }
                v.remeasure(ix);
                cx.notify();
            }));
        }

        let mut block = div().flex().flex_col().child(header);
        if expanded && has_details {
            let mut body = div().py(px(4.0)).flex().flex_col().gap(px(6.0));
            if expanded {
                let query = self.find_query_for_row(ix);
                if let Some(command) = &details.command {
                    let command_for_shell = command.clone();
                    body = body.child(elements::tool_output_block(None, command, FOLDED_LINES, self.detail, query))
                        .child(button(SharedString::from(format!("tool-shell-{ix}")), "Copy and open shell", false)
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(ThreadEvent::OpenToolShell(command_for_shell.clone())))));
                }
                for (label, text) in details.labeled_output() {
                    body = body.child(elements::tool_output_block(Some(label), text, FOLDED_LINES, self.detail, query));
                }
                body = body.children(call.content.iter().enumerate().filter_map(|(index, content)| match content {
                    ToolContent::Text(text) if !text.is_empty() && call.shows_text_content() =>
                        Some(elements::tool_output_block(None, text, FOLDED_LINES, self.detail, query)),
                    ToolContent::Image { mime, data } => self.render_tool_image(ix, (&call.id, index), (mime, data), cx),
                    _ => None,
                }));
            }
            block = block.child(elements::trace_content(body.into_any_element()));
        }
        block.into_any_element()
    }

    fn render_tool_image(&self, row: usize, image_key: (&str, usize), encoded: (&str, &str), cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = (image_key.0.to_string(), image_key.1);
        let mut images = self.tool_images.borrow_mut();
        let image = if let Some(image) = images.get(&key) {
            image.clone()
        } else {
            let format = match encoded.0 {
                "image/png" => gpui::ImageFormat::Png,
                "image/jpeg" => gpui::ImageFormat::Jpeg,
                "image/webp" => gpui::ImageFormat::Webp,
                _ => return None,
            };
            let data = base64::engine::general_purpose::STANDARD.decode(encoded.1).ok()?;
            let reader = image::ImageReader::new(std::io::Cursor::new(&data)).with_guessed_format().ok()?;
            let (width, height) = reader.into_dimensions().ok()?;
            if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 16_000_000 {
                return None;
            }
            let image = std::sync::Arc::new(gpui::Image::from_bytes(format, data));
            images.insert(key, image.clone());
            image
        };
        Some(div()
            .id(SharedString::from(format!("tool-screenshot-{row}-{}", image_key.1)))
            .cursor_pointer()
            .child(gpui::img(image.clone()).w(px(240.0)).h(px(140.0)).object_fit(gpui::ObjectFit::Contain))
            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                v.screenshot = Some(image.clone());
                v.screenshot_focus.focus(window, cx);
                cx.notify();
            }))
            .into_any_element())
    }

    fn render_screenshot(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let image = self.screenshot.clone()?;
        Some(div().id("thread-screenshot-preview").track_focus(&self.screenshot_focus).absolute().inset_0().occlude()
            .bg(hsla(theme::APP_BG).opacity(0.95)).flex().flex_col().items_center().p(px(16.0)).gap(px(10.0))
            .child(button("thread-screenshot-close", "Close screenshot", false)
                .on_click(cx.listener(|v, _: &ClickEvent, window, cx| {
                    v.screenshot = None;
                    v.focus_composer(window, cx);
                    cx.notify();
                })))
            .child(div().flex_1().w_full().min_h_0().child(gpui::img(image).size_full().object_fit(gpui::ObjectFit::Contain)))
            .into_any_element())
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
        self.reveal_timeline_row(ix, cx);
    }

    pub(super) fn reveal_timeline_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        match self.thread.entries.get(ix) {
            Some(Entry::Tool(call)) => {
                self.collapsed_shell_calls.remove(&call.id);
                self.expanded_tools.insert(call.id.clone());
            }
            Some(Entry::Thought(_)) => {
                self.collapsed_thoughts.remove(&ix);
                self.expanded_tools.insert(format!("thought:{ix}"));
            }
            Some(Entry::User(text)) if parse_subagent_report(text).is_some() => {
                self.expanded_tools.insert(format!("report:{ix}"));
            }
            _ => {}
        }
        let start = match self.tool_run(ix) {
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

    pub(super) fn run_kind(&self, ix: usize) -> Option<RunKind> {
        timeline::run_kind(self.thread.entries.get(ix)?)
    }

    pub(super) fn group_start_before(&self, ix: usize) -> usize {
        ix.checked_sub(1).and_then(|row| self.tool_run(row)).map_or(ix, |run| run.start)
    }

    fn tool_run(&self, ix: usize) -> Option<std::ops::Range<usize>> {
        let len = self.thread.entries.len();
        let mut cache = self.run_cache.borrow_mut();
        if cache.entries != len || cache.ranges.len() != len {
            let mut ranges = vec![None; len];
            let mut start = 0;
            while start < len {
                if self.run_kind(start).is_some() {
                    let mut end = start + 1;
                    while end < len && self.run_kind(end).is_some() {
                        end += 1;
                    }
                    for slot in &mut ranges[start..end] {
                        *slot = Some(start..end);
                    }
                    start = end;
                } else {
                    start += 1;
                }
            }
            *cache = super::RunCache { entries: len, ranges };
        }
        cache.ranges.get(ix).cloned().flatten()
    }

    /// A run is open while Ctrl+O shows everything or after a click on its line; one step alone is always open. Runs
    /// only grow at their end, so the first entry's place names a run for as long as it lasts.
    fn run_open(&self, run: &std::ops::Range<usize>) -> bool {
        run.len() == 1 || self.detail || self.expanded_tools.contains(&format!("run:{}", run.start))
    }

    /// Chain of Thought: collapsible summary line for a run of agent steps (thoughts, tool calls, subagent reports).
    fn render_tool_run(&self, run: std::ops::Range<usize>, cx: &mut Context<Self>) -> AnyElement {
        let steps = &self.thread.entries[run.clone()];
        let calls: Vec<&ToolCall> = steps.iter().filter_map(|e| if let Entry::Tool(c) = e { Some(c) } else { None }).collect();
        if let [Entry::Tool(call)] = steps {
            return self.render_tool(run.start, call, cx);
        }
        let reports: Vec<SubagentReport> = steps.iter().filter_map(|e| if let Entry::User(t) = e { parse_subagent_report(t) } else { None }).collect();
        let thoughts = steps.len() - calls.len() - reports.len();
        let latest = match steps.last() {
            Some(Entry::Tool(call)) => title_in_cwd(call, &self.cwd),
            Some(Entry::Thought(text)) => text.clone(),
            Some(Entry::User(text)) => parse_subagent_report(text).map(|r| report_line(r.title, r.state)).unwrap_or_default(),
            _ => String::new(),
        };
        let open = self.run_open(&run);
        let running = calls.iter().any(|c| c.status == ToolStatus::Running)
            || (self.is_working() && run.end == self.thread.entries.len() && matches!(steps.last(), Some(Entry::Thought(_))));
        let failed = calls.iter().any(|c| c.status == ToolStatus::Failed) || reports.iter().any(|r| r.state == "failed");
        let key = format!("run:{}", run.start);
        let start = run.start;
        let preview = first_line(&latest);
        let header = elements::chain_of_thought_header(
            start,
            if run.clone().all(|row| self.run_kind(row) == Some(RunKind::Exploration)) {
                timeline::exploration_label(calls.iter().copied())
            } else {
                run_label(thoughts, calls.len() + reports.len())
            },
            preview,
            steps.len(),
            running,
            failed,
            open,
        )
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
    fn render_background(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
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
        if self.background.is_empty() && self.provider_shells.is_empty() && self.background_tasks.is_empty() && bg_tools.is_empty() {
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
        for (i, task) in self.background_tasks.iter().enumerate() {
            let task_id = task.id.clone();
            let has_log = task.log_path.is_some();
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
                    .child(spinner(format!("thread-bg-async-task-{i}"), 10.0))
                    .child(div().max_w(px(220.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(task.label.clone()))
                    .when(has_log, |pill| {
                        pill.child(
                            button(SharedString::from(format!("bg-task-output-{i}")), "Output", false)
                                .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.open_background_task_output(&task_id, cx))),
                        )
                    }),
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
        for shell in &self.provider_shells {
            let read_id = shell.id.clone();
            let stop_id = shell.id.clone();
            let stopping = shell.stopping;
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
                    .child(spinner(format!("provider-shell-{}", shell.id), 10.0))
                    .child(div().max_w(px(220.0)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(shell.label.clone()))
                    .child(button(SharedString::from(format!("shell-output-{}", shell.id)), "Output", false)
                        .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.background_shell_action(&read_id, false, cx))))
                    .child(if stopping {
                        button_disabled(SharedString::from(format!("shell-stop-{}", shell.id)), "Stopping…")
                    } else {
                        button(SharedString::from(format!("shell-stop-{}", shell.id)), "Stop", false)
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.background_shell_action(&stop_id, true, cx)))
                    }),
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
        let output = self.shell_output.as_ref().map(|output| {
            self.shell_output_scroll.wire(div().id("thread-shell-output").w_full().max_h(px(180.0)), window)
                .rounded(px(6.0)).bg(hsla(theme::SURFACE_ELEVATED)).p(px(8.0))
                .child(div().flex().justify_end()
                    .child(button("thread-shell-output-close", "Close output", false).on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                        v.shell_output = None;
                        cx.notify();
                    }))))
                .child(div().font_family(crate::platform::monospace_font()).text_size(scale::px(11.5)).child(output.text.clone()))
        });
        Some(div().w_full().px(px(22.0)).mb(px(6.0)).flex().justify_center()
            .child(div().w_full().max_w(px(COLUMN_MAX_W)).flex().flex_col().gap(px(4.0)).child(bar).children(output)))
    }

    fn render_question(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let q = self.active_question()?;
        let question_id = q.id;
        let multi = q.multi_select;
        let selected_count = q.selected.len();
        let mut options = div().flex().flex_col().gap(px(6.0)).w_full();
        for (i, opt) in q.options.iter().enumerate() {
            let lit = i == q.highlight;
            let checked = multi && q.selected.contains(&i);
            let row = div()
                .id(SharedString::from(format!("thread-perm-{question_id}-{i}")))
                .w_full()
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(7.0))
                .bg(hsla(if lit || checked { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
                .border_1()
                .border_color(hsla(if lit || checked { theme::FOCUS_BORDER } else { theme::PANEL_BORDER }))
                .cursor_pointer()
                .on_hover(cx.listener(move |v, hovered: &bool, _, cx| {
                    if *hovered {
                        v.highlight_option(i, cx);
                    }
                }))
                .flex()
                .items_start()
                .gap(px(10.0))
                .when(multi, |row| {
                    row.child(
                        div()
                            .mt(px(2.0))
                            .flex_none()
                            .size(px(15.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(hsla(if checked { theme::ACCENT } else { theme::PANEL_BORDER }))
                            .bg(hsla(if checked { theme::ACCENT } else { theme::PANEL_BG }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(checked, |box_el| box_el.child(icon(IconName::Check, 11.0).text_color(hsla(theme::PANEL_BG)))),
                    )
                })
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
                .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                    if multi {
                        v.toggle_question_option(question_id, i, cx);
                    } else {
                        v.answer_question(question_id, Some(i), cx);
                    }
                }));
            options = options.child(row);
        }
        let pending_count = self.pending_question_count();
        let footer = if multi {
            let submit_label = if selected_count > 0 { format!("Submit ({selected_count})") } else { "Submit".into() };
            let submit_btn = if selected_count > 0 {
                button_colored(
                    "thread-question-submit",
                    Some(IconName::Check),
                    &submit_label,
                    hsla(theme::ACCENT),
                    hsla(theme::PANEL_BG),
                    hsla(0xdcdcdc),
                    None,
                    true,
                )
                .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                    v.submit_multi_question(question_id, cx);
                }))
                .h(px(24.0))
                .px(px(10.0))
                .into_any_element()
            } else {
                button_disabled_leading("thread-question-submit", Some(IconName::Check), &submit_label)
                    .h(px(24.0))
                    .px(px(10.0))
                    .into_any_element()
            };
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.0))
                        .items_center()
                        .text_color(hsla(theme::TEXT_FAINT))
                        .text_size(scale::px(11.5))
                        .child(key_badge("↑↓"))
                        .child(key_badge("Space"))
                        .child("or")
                        .child(key_badge("1-9"))
                        .child("to toggle")
                        .child(key_badge("Enter"))
                        .child("to submit")
                        .child(key_badge("Esc"))
                        .child("to decline")
                        .when(q.takes_own_answer, |d| d.child("· or type below")),
                )
                .child(submit_btn)
        } else if q.options.is_empty() {
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.0))
                .items_center()
                .text_color(hsla(theme::TEXT_FAINT))
                .text_size(scale::px(11.5))
                .child(key_badge("Esc"))
                .child("to dismiss")
                .child("· type your answer below")
        } else {
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
                .when(q.takes_own_answer, |d| d.child("· or type your own answer below"))
        };
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
                .when(pending_count > 1, |card| {
                    card.child(
                        div()
                            .text_size(scale::px(11.5))
                            .text_color(hsla(theme::TEXT_DIM))
                            .child(format!("{} more requests", pending_count - 1)),
                    )
                })
                .when(!q.options.is_empty(), |card| {
                    card.child(self.question_scroll.wire(div().id("thread-question-options").max_h(px(240.0)), window).child(options))
                })
                .child(footer),
        )
    }

    /// Universal floating picker dialog over the chat view: unified model + draggable effort picker, or permission mode selector.
    fn render_picker_overlay(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let active = self.picker?;
        let accent = hsla(theme::ACCENT);

        let (id, title, body) = match active {
            ThreadPicker::Model => {
                let current_id = self.thread.option("model").map(|m| m.current.as_str());
                let models = self.filtered_models(cx);
                let hi = self.model_highlight.min(models.len().saturating_sub(1));
                self.model_rows.borrow_mut().truncate(models.len());
                let mut list = self.model_scroll.wire(div().id("picker-model-list").role(gpui::Role::ListBox).aria_label("Models").flex_1().min_h_0().max_h(px(210.0)), window).flex().flex_col().gap(px(2.0));
                let mut previous = None;
                for (n, m) in models.iter().enumerate() {
                    let heading = (previous != Some(m.harness)).then(|| picker::section_header(m.harness.name(), None));
                    previous = Some(m.harness);
                    let selected = match &self.pending_model {
                        Some(p) => p.harness == m.harness && p.id == m.id,
                        None => m.harness == self.harness && current_id == Some(m.id.as_str()),
                    };
                    let focused = n == hi;
                    let choice = m.clone();
                    let logo = logos::draw(logos::for_harness(m.harness), 11.5, hsla(if selected { theme::TEXT } else { theme::TEXT_FAINT })).into_any_element();
                    list = list.child(div().flex_none().flex().flex_col().children(heading).child(
                        picker::row(SharedString::from(format!("thread-model-{n}")), Some(logo), m.name.clone(), None, None, selected, false)
                            .relative()
                            .role(gpui::Role::ListBoxOption)
                            .aria_selected(selected)
                            .when(focused && !selected, |d| d.bg(hsla(theme::SURFACE_HOVER)))
                            .border_1()
                            .border_color(if focused { hsla(theme::FOCUS_BORDER) } else { gpui::transparent_black() })
                            .debug_selector(move || format!("thread-model-{n}"))
                            .child(picker::track_row(self.model_rows.clone(), n))
                            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                                v.pick_model(choice.clone(), cx);
                                v.close_picker(window, cx);
                            })),
                    ));
                }
                if models.is_empty() {
                    list = list.child(crate::panels::widgets::empty_panel(IconName::Search, "No matching models"));
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
                    .flex_1()
                    .min_h_0()
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
                let mut rows = self.permission_scroll.wire(div().id("picker-perm-list").max_h(px(340.0)), window).flex().flex_col().gap(px(2.0));
                let offered = self.permissions();
                for (i, mode) in Permission::ALL.into_iter().enumerate() {
                    let available = offered.contains(&mode);
                    let current = self.permission() == Some(mode);
                    let focused = available && i == self.mode_highlight;
                    let lead = icon(IconName::Lock, 12.0).text_color(hsla(if current { theme::TEXT } else { theme::TEXT_FAINT })).into_any_element();
                    let mut row = picker::row(SharedString::from(format!("perm-mode-{i}")), Some(lead), mode.label(), Some(mode.detail(available)), None, current, !available)
                        .debug_selector(move || format!("perm-mode-{i}"))
                        .when(focused && !current, |d| d.bg(hsla(theme::SURFACE_HOVER)))
                        .border_1()
                        .border_color(if focused { hsla(theme::FOCUS_BORDER) } else { gpui::transparent_black() });
                    if available {
                        row = row.on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                            v.set_permission(mode, cx);
                            v.close_picker(window, cx);
                        }));
                    }
                    rows = rows.child(row);
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

    fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut inner = div().w_full().max_w(px(COLUMN_MAX_W)).flex().flex_col().gap(px(6.0));

        let commands = self.matching_commands(cx);
        if !commands.is_empty() {
            let highlight = self.command_highlight.min(commands.len() - 1);
            let mut menu = self.command_scroll.wire(div().id("thread-commands").max_h(px(240.0)), window)
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
                            v.replace_active_command(&name, true, window, cx);
                            v.focus_composer(window, cx);
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
            .capture_action(cx.listener(|v, _: &Paste, _, cx| {
                if v.attachments_enabled() && v.attachments.paste(cx) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .when(self.attachments_enabled(), |d| {
                d.children(self.attachments.render(|v: &mut Self| &mut v.attachments, cx))
            })
            .child(div()
                .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| v.focus_composer(window, cx)))
                .child(Textarea::new(&self.composer).appearance(false)))
            .into_any_element();
        let dock = composer::render(self.composer_parts(focused, input, window.modifiers().secondary(), window, cx))
            .when(self.attachments_enabled(), |d| {
                attachments::drop_target(d, cx.listener(|v, paths: &gpui::ExternalPaths, window, cx| {
                    v.attach_paths(paths.paths(), window, cx);
                }))
            });

        div()
            .flex_none()
            .w_full()
            .px(px(22.0))
            .pb(px(12.0))
            .pt(px(4.0))
            .flex()
            .justify_center()
            .id("thread-composer-dock")
            .on_modifiers_changed(cx.listener(|_, _: &gpui::ModifiersChangedEvent, _, cx| cx.notify()))
            .child(inner.children(self.render_waiting(window, cx)).child(dock))
    }

    /// The user's messages waiting for the turn to end, above the message box, each one editable or removable
    /// before it is sent.
    fn render_waiting(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let waiting = self.waiting_messages().collect::<Vec<_>>();
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
        let rows = waiting.into_iter().map(|(id, text)| {
            let editing = self.queue_edit.as_ref().is_some_and(|edit| edit.id == id);
            let can_act = !editing && !self.interrupting && !self.is_waiting();
            div()
                .id(SharedString::from(format!("queue-item-{id}")))
                .rounded(px(6.0))
                .hover(|style| style.bg(hsla(theme::SURFACE_HOVER)))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(4.0))
                .when(editing, |row| row.bg(hsla(theme::SURFACE_HOVER)))
                .child(icon(IconName::Clock, 12.0).text_color(hsla(theme::TEXT_FAINT)))
                .child(div().flex_1().min_w_0().overflow_hidden().text_size(scale::px(12.5)).text_color(hsla(theme::TEXT_DIM)).child(text.to_string()))
                .when(can_act, |row| {
                    row.child(button(SharedString::from(format!("queued-send-{id}")), "Send now", false)
                        .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.send_queued_now(id, cx))))
                    .when(self.queue_edit.is_none(), |row| row.child(
                        icon_button(SharedString::from(format!("queued-edit-{id}")), IconName::Pencil)
                            .accessibility_label("Edit queued message")
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Edit queued message").build(window, cx))
                            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| v.edit_queued(id, window, cx))),
                    ))
                    .children([(true, "Move up"), (false, "Move down")].into_iter().filter_map(|(earlier, label)| {
                        self.adjacent_queued(id, earlier)?;
                        Some(button(SharedString::from(format!("queued-move-{id}-{earlier}")), label, false)
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.move_queued(id, earlier, cx))))
                    }))
                    .child(
                        icon_button(SharedString::from(format!("queued-remove-{id}")), IconName::X)
                            .accessibility_label("Remove queued message")
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Remove queued message").build(window, cx))
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                if v.unqueue(id, cx).is_some() {
                                    v.queue_changed();
                                }
                            })),
                    )
                })
        });
        Some(
            div().child(self.queue_scroll.wire(div().id("thread-queue").w_full().max_h(px(180.0)), window)
                .p(px(4.0))
                .rounded(px(10.0))
                .bg(hsla(theme::SURFACE_ELEVATED))
                .border_1()
                .border_color(hsla(theme::PANEL_BORDER))
                .flex()
                .flex_col()
                .children(steering_rows)
                .when(!waiting_empty, |d| d.child(div().px(px(10.0)).pt(px(2.0)).text_size(scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(if self.queue_held { "Queue paused" } else { "Queued for after this turn" })))
                .children(rows)),
        )
    }

    /// Assemble the shared message box parts: model, permission mode, branch, review and send. What the agent is
    /// doing shows in the chat (`render_activity`) and what waits shows above the box (`render_waiting`).
    fn composer_parts(&self, focused: bool, input: AnyElement, alternate: bool, window: &mut Window, cx: &mut Context<Self>) -> composer::Composer {
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
            .on_key_down(cx.listener(|v, ev: &gpui::KeyDownEvent, window, cx| {
                let plain = !ev.keystroke.modifiers.control && !ev.keystroke.modifiers.alt && !ev.keystroke.modifiers.platform && !ev.keystroke.modifiers.shift;
                if plain && matches!(ev.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    v.toggle_model_picker(window, cx);
                    cx.notify();
                }
            }))
            .into_any_element();

        let label = self.initial_permission.or_else(|| self.permission()).map_or("Harness default", Permission::label);
        let permission = Some(composer::permission_chip("thread-mode", self.picker == Some(ThreadPicker::Permission), label)
            .debug_selector(|| "thread-mode".into())
            .dropdown_above(cx, |v, at, window, cx| {
                v.picker = Some(ThreadPicker::Permission);
                v.picker_at = at;
                let offered = v.permissions();
                let current = v.permission().filter(|p| offered.contains(p)).or_else(|| offered.first().copied());
                v.mode_highlight = Permission::ALL.iter().position(|p| Some(*p) == current).unwrap_or(0);
                v.focus_composer(window, cx);
                cx.notify();
            })
            .into_any_element());

        let git = self.git.as_ref().map(|git| {
            let tail = git.busy.then(|| spinner("thread-git-busy", 11.0).into_any_element());
            composer::git_chip("thread-git", git.open, git.primary.clone(), git.linked.clone(), tail)
                .dropdown_above(cx, |_, at, _, cx| cx.emit(ThreadEvent::OpenGitPicker(at)))
                .into_any_element()
        });

        let extra_chips: Vec<AnyElement> = vec![];
        let can_review = self.can_review();
        let mut extra_actions = vec![
            composer::review_button("thread-review-open", can_review)
                .when(can_review, |b| b.on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_review(cx))))
                .into_any_element(),
        ];
        if self.attachments_enabled() {
            extra_actions.push(attachments::button(&self.composer, |v: &mut Self| &mut v.attachments, cx).into_any_element());
        }
        let draft = self.composer.read(cx).value();
        let has_message = self.can_submit_draft(cx);
        let steering_text = if self.attachments.has_files() { "" } else { draft.trim() };
        let editing = self.queue_edit.is_some();
        let active_turn = self.has_active_turn();
        let can_interrupt = self.can_interrupt();
        let idle_for_resume = !active_turn && self.queue_held && !self.is_waiting()
            && (self.waiting_messages().next().is_some() || self.handoff_after_turn.is_some());
        let answering = self.active_question().is_some_and(|q| q.takes_own_answer);
        let follow_up = if alternate { self.follow_up.other() } else { self.follow_up };
        #[derive(Clone, Copy)]
        enum ActionKind {
            Save,
            Answer,
            Stopping,
            Interrupt,
            Resume,
            Steer,
            Queue,
            Send,
        }
        let action = if editing {
            ActionKind::Save
        } else if answering && has_message {
            ActionKind::Answer
        } else if !has_message && self.interrupting {
            ActionKind::Stopping
        } else if !has_message && can_interrupt {
            ActionKind::Interrupt
        } else if answering {
            ActionKind::Answer
        } else if !has_message && idle_for_resume {
            ActionKind::Resume
        } else if self.can_steer(steering_text, follow_up) {
            ActionKind::Steer
        } else if active_turn {
            ActionKind::Queue
        } else {
            ActionKind::Send
        };
        let (label, glyph) = match action {
            ActionKind::Save => ("Save", IconName::Check),
            ActionKind::Answer => ("Answer", IconName::Check),
            ActionKind::Stopping => ("Stopping…", IconName::Square),
            ActionKind::Interrupt => ("Interrupt", IconName::Square),
            ActionKind::Resume => ("Resume", IconName::Play),
            ActionKind::Steer => ("Steer", IconName::CornerDownRight),
            ActionKind::Queue => ("Queue", IconName::ListPlus),
            ActionKind::Send => ("Send", IconName::ArrowUp),
        };
        let enabled = if editing {
            has_message && !self.is_waiting()
        } else if answering && has_message {
            true
        } else if !has_message && can_interrupt {
            !self.interrupting
        } else if answering {
            false
        } else {
            has_message || idle_for_resume
        };
        let send = if enabled {
            let (bg, fg, hover, border) = match action {
                ActionKind::Interrupt => (hsla(0x7a242a), hsla(0xffe4e6), hsla(0x942c33), Some(hsla(theme::DANGER).opacity(0.65))),
                ActionKind::Steer => (hsla(theme::WARN), hsla(0x16120b), hsla(0xedd092), None),
                ActionKind::Queue => (hsla(theme::CODEX), hsla(0x0c141c), hsla(0x7cc0f5), None),
                ActionKind::Resume => (hsla(theme::OK), hsla(0x0e160c), hsla(0x92c27d), None),
                ActionKind::Save | ActionKind::Answer | ActionKind::Send | ActionKind::Stopping => (hsla(theme::ACCENT), hsla(theme::PANEL_BG), hsla(0xdcdcdc), None),
            };
            button_colored("thread-send", Some(glyph), label, bg, fg, hover, border, true)
                .on_click(cx.listener(|v, ev: &ClickEvent, window, cx| {
                    if v.queue_edit.is_some() || v.can_submit_draft(cx) {
                        v.submit_with(ev.modifiers().secondary(), window, cx);
                    } else if v.can_interrupt() {
                        v.interrupt(cx);
                    } else if v.queue_held && !v.is_waiting() {
                        v.resume_queue(cx);
                    }
                }))
        } else {
            button_disabled_leading("thread-send", Some(glyph), label)
        }.h(px(26.0)).px(px(12.0)).into_any_element();

        composer::Composer {
            lit: focused || self.active_question().is_some(),
            top: self.render_question(window, cx).map(IntoElement::into_any_element),
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

/// Display `path` relative to `cwd` when inside it, normalizing `file://` URLs, separators, Windows drive-letter casing,
/// and symlinks/canonicalized forms.
fn relative_to_cwd(cwd: &std::path::Path, path: &std::path::Path) -> String {
    let owned = path
        .to_str()
        .filter(|s| s.starts_with("file:"))
        .and_then(|s| url::Url::parse(s).ok())
        .and_then(|u| u.to_file_path().ok());
    let path = owned.as_deref().unwrap_or(path);
    let strip = |base: &std::path::Path, target: &std::path::Path| -> Option<String> {
        let base = dunce::simplified(base);
        let target = dunce::simplified(target);
        if let Ok(rel) = target.strip_prefix(base) {
            let s = rel.to_string_lossy();
            if !s.is_empty() {
                return Some(s.replace('\\', "/"));
            }
        }
        let cwd_norm = base.to_string_lossy().replace('\\', "/");
        let cwd_trimmed = cwd_norm.trim_end_matches('/');
        let path_norm = target.to_string_lossy().replace('\\', "/");
        let matches_prefix = if cfg!(windows) {
            path_norm.len() > cwd_trimmed.len()
                && path_norm.as_bytes().get(cwd_trimmed.len()) == Some(&b'/')
                && path_norm[..cwd_trimmed.len()].eq_ignore_ascii_case(cwd_trimmed)
        } else {
            path_norm.starts_with(cwd_trimmed) && path_norm.as_bytes().get(cwd_trimmed.len()) == Some(&b'/')
        };
        matches_prefix.then(|| path_norm[cwd_trimmed.len() + 1..].to_string())
    };
    if let Some(rel) = strip(cwd, path) {
        return rel;
    }
    if let (Ok(canon_cwd), Ok(canon_path)) = (dunce::canonicalize(cwd), dunce::canonicalize(path))
        && let Some(rel) = strip(&canon_cwd, &canon_path)
    {
        return rel;
    }
    dunce::simplified(path).display().to_string()
}

/// A tool's title with any `cwd`-prefixed file path shortened to a relative path.
fn title_in_cwd(call: &ToolCall, cwd: &std::path::Path) -> String {
    let raw = title(call);
    let cwd_norm = dunce::simplified(cwd).to_string_lossy().replace('\\', "/");
    let cwd_trimmed = cwd_norm.trim_end_matches('/');
    if !cwd_trimmed.is_empty() {
        let raw_norm = raw.replace('\\', "/");
        let needle = format!("{cwd_trimmed}/");
        let found = if cfg!(windows) {
            raw_norm.to_ascii_lowercase().find(&needle.to_ascii_lowercase())
        } else {
            raw_norm.find(&needle)
        };
        if let Some(pos) = found {
            let after = pos + needle.len();
            let mut out = String::with_capacity(raw.len());
            out.push_str(&raw[..pos]);
            out.push_str(&raw_norm[after..]);
            return out;
        }
    }
    raw
}

/// A tool's title, or its kind while the agent hasn't named it.
fn title(call: &ToolCall) -> String {
    if !call.title.is_empty() {
        return call.title.clone();
    }
    if call.kind.is_empty() { "Tool".into() } else { call.kind.clone() }
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

#[cfg(test)]
mod tests {
    use super::{keep_line_breaks, relative_to_cwd, report_line, run_label, title_in_cwd};

    #[test]
    fn tool_paths_shorten_relative_to_cwd() {
        let cwd = std::path::Path::new(if cfg!(windows) { r"C:\Users\dev\proj" } else { "/Users/dev/proj" });
        let file = std::path::Path::new(if cfg!(windows) { r"c:\Users\dev\proj\src\main.rs" } else { "/Users/dev/proj/src/main.rs" });
        assert_eq!(relative_to_cwd(cwd, file), "src/main.rs");
        let call = crate::harness::thread::ToolCall {
            id: "1".into(),
            title: format!("Read {}", file.display()),
            kind: "read".into(),
            status: crate::harness::thread::ToolStatus::Done,
            content: vec![],
            locations: vec![],
            details: Default::default(),
            subagent: None,
        };
        assert_eq!(title_in_cwd(&call, cwd), "Read src/main.rs");
    }

    #[gpui::test]
    fn conversation_selection_ends_after_a_missed_mouse_release(cx: &mut gpui::TestAppContext) {
        use gpui::{App, IntoElement, Modifiers, MouseButton, ParentElement, Render, Styled, Window, div, point, px};
        use gpui_kit::base::{TextSelection, TextSelectionLayer};
        use gpui_kit::component::text::TextView;

        struct Conversation;
        impl Render for Conversation {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                div()
                    .text_size(px(16.0))
                    .child(TextSelectionLayer)
                    .child(crate::github::ui::styled_markdown(TextView::markdown("message", "A line of conversation text")))
            }
        }

        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| Conversation);
        cx.update(|window, app| window.draw(app).clear(app));
        let start = point(px(2.0), px(8.0));
        let end = point(px(100.0), px(8.0));
        let modifiers = Modifiers::default();
        let selected = |window: &mut Window, app: &mut App| TextSelection::selected_text(window, app);

        cx.simulate_mouse_down(start, MouseButton::Left, modifiers);
        cx.simulate_mouse_move(end, None, modifiers);
        assert_eq!(cx.update(selected), "");
        cx.simulate_mouse_move(end, MouseButton::Left, modifiers);
        assert_eq!(cx.update(selected), "");

        cx.simulate_mouse_down(start, MouseButton::Left, modifiers);
        cx.simulate_mouse_move(end, MouseButton::Left, modifiers);
        cx.update(|window, app| window.draw(app).clear(app));
        let text = cx.update(selected);
        assert!(text.starts_with("A line"), "{text:?}");
        cx.simulate_mouse_move(start, None, modifiers);
        assert_eq!(cx.update(selected), text);
    }

    #[test]
    fn a_users_lines_stay_lines_but_code_and_paragraphs_do_not_change() {
        assert_eq!(keep_line_breaks("one\ntwo\nthree"), "one  \ntwo  \nthree");
        assert_eq!(keep_line_breaks("para\n\nnext"), "para\n\nnext", "a blank line already splits them");
        assert_eq!(keep_line_breaks("look:\n```\na\nb\n```\nafter"), "look:  \n```\na\nb\n```\nafter");
        assert_eq!(keep_line_breaks("single"), "single");
        assert_eq!(
            keep_line_breaks("````\n```\nliteral\nlines\n````\nafter\nmore"),
            "````\n```\nliteral\nlines\n````\nafter  \nmore",
            "a shorter marker inside a longer fence is code, not its end"
        );
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

}
