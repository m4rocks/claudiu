//! How a thread is drawn: a readable conversation column, active plan/permission prompts, an elevated composer dock
//! with a unified model + draggable effort picker (aggregating every enabled harness) and a permission selector dialog.

use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, Div, Focusable, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, linear_color_stop, linear_gradient, list, prelude::FluentBuilder, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Textarea;

use super::{ThreadEvent, ThreadPicker, ThreadView};
use crate::harness::CatalogModel;
use crate::harness::thread::{Entry, ToolCall, ToolContent, ToolStatus, diff_lines};
use crate::panels::{composer, logos, picker, scale};
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{EffortPick, Trigger, effort_slider, icon, key_badge, spinner};
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
            .text_size(scale::px(13.5));
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
        let bottom_dock = div().flex_none().flex().flex_col().children(self.render_plan()).child(self.render_composer(window, cx));
        root.child(conversation)
            .child(bottom_dock)
            .children(self.render_picker_overlay(window, cx))
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
        let accent = hsla(theme::ACCENT);
        let body = match entry {
            Entry::User(text) => div()
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(9.0))
                .bg(hsla(theme::ROW_ACTIVE))
                .border_1()
                .border_color(hsla(theme::PANEL_BORDER))
                .child(
                    div()
                        .flex()
                        .gap(px(10.0))
                        .child(div().w(px(2.5)).rounded_full().bg(accent))
                        .child(div().flex_1().min_w_0().child(text.clone())),
                )
                .into_any_element(),
            Entry::Agent(text) => div().px(px(2.0)).child(crate::github::ui::markdown(format!("thread-md-{ix}"), text)).into_any_element(),
            Entry::Thought(text) => {
                let shown = if self.detail { text.clone() } else { text.lines().next().unwrap_or_default().to_string() };
                div()
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .bg(hsla(0x111113))
                    .italic()
                    .text_size(scale::px(12.5))
                    .text_color(hsla(theme::TEXT_FAINT))
                    .child(format!("✻ {shown}"))
                    .into_any_element()
            }
            Entry::Tool(call) => match tool_run(&self.groupable(), ix) {
                // The run's first call draws the run's line; the rest only show while the run is open.
                Some(run) if run.start == ix => self.render_tool_run(run, cx),
                Some(run) if !self.run_open(&run) => return div().into_any_element(),
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

    /// Compressed single-line tool call row; click to expand full output and diffs.
    fn render_tool(&self, ix: usize, call: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let (mark, color) = match call.status {
            ToolStatus::Pending | ToolStatus::Running => ("●", theme::WARN),
            ToolStatus::Done => ("●", theme::OK),
            ToolStatus::Failed => ("●", theme::DANGER),
        };
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
            .child(div().flex_none().text_size(scale::px(9.0)).text_color(hsla(color)).child(mark))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(scale::px(12.0))
                    .text_color(hsla(if expanded { theme::TEXT_DIM } else { theme::TEXT_FAINT }))
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

    /// Per entry: whether it is a tool call that folds into a run with its neighbours. A call with an inline card stays
    /// on its own, so the card is never hidden.
    fn groupable(&self) -> Vec<bool> {
        self.thread.entries.iter().map(|e| matches!(e, Entry::Tool(call) if !self.cards.contains_key(&call.id))).collect()
    }

    /// A run is open while Ctrl+O shows everything or after a click on its line; one call alone is always open.
    fn run_open(&self, run: &std::ops::Range<usize>) -> bool {
        let Some(Entry::Tool(first)) = self.thread.entries.get(run.start) else { return true };
        run.len() == 1 || self.detail || self.expanded_tools.contains(&format!("run:{}", first.id))
    }

    /// The dimmed line of a run of tool calls: how many, and the latest one's title. Click to show each call.
    fn render_tool_run(&self, run: std::ops::Range<usize>, cx: &mut Context<Self>) -> AnyElement {
        let calls: Vec<&ToolCall> = self.thread.entries[run.clone()].iter().filter_map(|e| if let Entry::Tool(c) = e { Some(c) } else { None }).collect();
        if calls.len() == 1 {
            return self.render_tool(run.start, calls[0], cx);
        }
        let open = self.run_open(&run);
        let color = if calls.iter().any(|c| matches!(c.status, ToolStatus::Pending | ToolStatus::Running)) {
            theme::WARN
        } else if calls.iter().any(|c| c.status == ToolStatus::Failed) {
            theme::DANGER
        } else {
            theme::TEXT_FAINT
        };
        let key = format!("run:{}", calls[0].id);
        let start = run.start;
        let latest = calls.last().map(|c| title(c)).unwrap_or_default();
        div()
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
            .text_color(hsla(theme::TEXT_FAINT))
            .child(div().flex_none().text_size(scale::px(9.0)).text_color(hsla(color)).child("●"))
            .child(div().flex_none().text_color(hsla(theme::TEXT_DIM)).child(format!("{} tool calls", calls.len())))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(latest))
            .child(div().flex_none().child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                if !v.expanded_tools.remove(&key) {
                    v.expanded_tools.insert(key.clone());
                }
                v.remeasure(start);
                cx.notify();
            }))
            .into_any_element()
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

    fn render_question(&self, cx: &mut Context<Self>) -> Option<Div> {
        let q = self.question.as_ref()?;
        let accent = hsla(theme::ACCENT);
        let mut options = div().flex().flex_col().gap(px(6.0)).w_full();
        for (i, opt) in q.options.iter().enumerate() {
            let primary = opt.kind == "allow_once" && i == 0;
            let row = div()
                .id(SharedString::from(format!("thread-perm-{i}")))
                .w_full()
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(7.0))
                .bg(hsla(if primary { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
                .border_1()
                .border_color(if primary { accent } else { hsla(theme::PANEL_BORDER) })
                .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                .cursor_pointer()
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
                                .text_color(if primary { accent } else { hsla(theme::TEXT) })
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
                .child(div().flex().gap(px(6.0)).items_center().text_color(hsla(theme::TEXT_FAINT)).text_size(scale::px(11.5)).child(key_badge("1-9")).child("to choose").child(key_badge("Esc")).child("to decline")),
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
                    let logo = logos::draw(logos::for_harness(m.harness), 11.5, if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    list = list.child(
                        picker::row(SharedString::from(format!("thread-model-{n}")), Some(logo), m.name.clone(), None, None, selected, false)
                            .when(focused && !selected, |d| d.bg(hsla(theme::SURFACE_HOVER)))
                            .when(focused, |d| d.border_1().border_color(accent.opacity(0.45)))
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                v.pick_model(choice.clone(), cx);
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
                    for (i, (id, name)) in self.thread.modes.iter().enumerate() {
                        let current = self.thread.mode.as_deref() == Some(id.as_str());
                        let mode_id = id.clone();
                        let display = picker::format_mode_name(id, name);
                        let lead = icon(IconName::Lock, 12.0).text_color(if current { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
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
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                v.set_mode(mode_id.clone(), cx);
                                v.picker = None;
                                cx.notify();
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
            |v: &mut Self, _, _| {
                v.picker = None;
                v.effort_dragging.set(false);
            },
            window,
            cx,
        ))
    }

    fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let accent = hsla(theme::ACCENT);
        let mut inner = div().w_full().max_w(px(COLUMN_MAX_W)).flex().flex_col().gap(px(6.0));

        let commands = self.matching_commands(cx);
        if !commands.is_empty() {
            let mut menu = div()
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
                        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                        .child(div().text_color(accent).child(format!("/{}", c.name)))
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
                    .border_color(accent)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(scale::px(11.5))
                            .text_color(hsla(theme::TEXT))
                            .child(logos::draw(logos::for_harness(target.harness), 11.5, accent))
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
        let (chips, actions) = self.footer(cx);
        let dock = composer::frame(focused || self.question.is_some(), self.render_question(cx).map(IntoElement::into_any_element), input, chips, actions);

        div()
            .flex_none()
            .w_full()
            .px(px(22.0))
            .pb(px(12.0))
            .pt(px(4.0))
            .flex()
            .justify_center()
            .child(inner.child(dock))
    }

    /// The message box's footer: model, mode and branch chips with the turn's state on the left; review and the queue
    /// on the right.
    fn footer(&self, cx: &mut Context<Self>) -> (Vec<AnyElement>, Vec<AnyElement>) {
        let faint = |text: String| div().text_size(scale::px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(text);
        let active_harness = self.pending_model.as_ref().map_or(self.harness, |m| m.harness);
        let (_, effort_labels, effort_ix, _) = self.effort_choices();
        let effort_badge = effort_labels.get(effort_ix).cloned();
        let mut chips: Vec<AnyElement> = vec![];

        // Model + effort; its dropdown opens directly above the chip.
        chips.push(
            composer::chip("thread-model", self.picker == Some(ThreadPicker::Model))
                .relative()
                .child(self.model_anchor.probe())
                .child(logos::draw(logos::for_harness(active_harness), 11.0, hsla(theme::TEXT_DIM)))
                .child(div().max_w(px(220.0)).truncate().text_color(hsla(theme::TEXT)).child(self.active_model_label()))
                .when_some(effort_badge, |d, badge| {
                    d.child(div().px(px(5.0)).rounded(px(4.0)).bg(hsla(0x1f1f22)).text_size(scale::px(10.0)).text_color(hsla(theme::TEXT_DIM)).child(badge))
                })
                .child(icon(IconName::ChevronDown, 10.5).text_color(hsla(theme::TEXT_FAINT)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|v, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        v.toggle_model_picker(window, cx);
                        cx.notify();
                    }),
                )
                .into_any_element(),
        );

        // Permission mode, always in plain English.
        if let Some(mode_id) = self.thread.mode.as_deref() {
            let raw_name = self.thread.mode_name().unwrap_or(mode_id);
            chips.push(
                composer::chip("thread-mode", self.picker == Some(ThreadPicker::Permission))
                    .child(icon(IconName::Lock, 11.0).text_color(hsla(theme::TEXT_DIM)))
                    .child(div().text_color(hsla(theme::TEXT_DIM)).child(picker::format_mode_name(mode_id, raw_name)))
                    .child(icon(IconName::ChevronDown, 10.5).text_color(hsla(theme::TEXT_FAINT)))
                    .dropdown_above(cx, |v, at, _, cx| {
                        v.picker = Some(ThreadPicker::Permission);
                        v.picker_at = at;
                        cx.notify();
                    })
                    .into_any_element(),
            );
        }

        if let Some(git) = &self.git {
            let tail = git.busy.then(|| spinner("thread-git-busy", 11.0).into_any_element());
            chips.push(
                composer::git_chip("thread-git", git.open, git.primary.clone(), git.linked.clone(), tail)
                    .dropdown_above(cx, |_, at, _, cx| cx.emit(ThreadEvent::OpenGitPicker(at)))
                    .into_any_element(),
            );
        }

        if self.working {
            chips.push(div().ml(px(4.0)).flex().items_center().gap(px(5.0)).child(spinner("thread-working", 11.0)).child(faint("Working".into())).into_any_element());
            chips.push(div().flex().gap(px(4.0)).items_center().child(key_badge("Esc")).child(faint("stop".into())).into_any_element());
        }

        let mut actions: Vec<AnyElement> = vec![];
        if !self.queue.is_empty() {
            actions.push(faint(format!("{} queued", self.queue.len())).into_any_element());
        }
        if self.can_review() {
            actions.push(
                composer::chip("thread-review-open", false)
                    .text_color(hsla(theme::TEXT_DIM))
                    .child("Review changes")
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_review(cx)))
                    .into_any_element(),
            );
        }
        (chips, actions)
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

/// The run of foldable tool calls (`groupable`) that entry `ix` belongs to, or None when it is not one.
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
    use super::tool_run;

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
