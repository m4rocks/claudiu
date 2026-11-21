//! How a thread is drawn: a readable conversation column, active plan/permission prompts, an elevated composer dock
//! with compact provider/model/effort and permission dialogs (closeable via Escape or clicking outside).

use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, Div, Focusable, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, deferred, div, list, prelude::FluentBuilder, px, rgba,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Textarea;

use super::{Phase, ThreadPicker, ThreadView};
use crate::claude::defaults::Effort;
use crate::harness::thread::{Entry, ToolCall, ToolContent, ToolStatus, diff_lines};
use crate::panels::logos;
use crate::panels::scale;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, effort_slider, icon, key_hint, section_label, spinner};
use crate::visual::ui::Scope;

/// Lines of a tool's output shown while it is folded (Ctrl+O shows all).
const FOLDED_LINES: usize = 6;

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
        if self.phase == Phase::Starting {
            return root.child(waiting(&format!("Starting {}…", self.harness.name())));
        }
        if let Some(review) = self.render_review(cx) {
            return root.child(review);
        }
        let conversation = list(self.list.clone(), cx.processor(|v: &mut Self, ix: usize, window, cx| v.render_entry(ix, window, cx)))
            .flex_1()
            .w_full()
            .px(px(22.0))
            .pt(px(14.0));
        root.child(conversation)
            .children(self.render_plan())
            .children(self.render_question(cx))
            .child(self.render_composer(window, cx))
            .children(self.render_picker_overlay(cx))
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

fn mode_description(id: &str) -> &'static str {
    match id {
        "default" | "manual" => "Ask before editing files or running commands",
        "acceptEdits" => "Accept file edits automatically; ask for commands",
        "auto" => "Decide permissions automatically",
        "plan" => "Read and plan only; no file edits or commands",
        "dontAsk" => "Only run pre-approved tools without prompting",
        "bypassPermissions" => "Run commands and edit files without asking",
        _ => "Harness permission mode",
    }
}

impl ThreadView {
    fn render_entry(&mut self, ix: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.thread.entries.get(ix) else { return div().into_any_element() };
        let accent = hsla(theme::harness_color(self.harness));
        let body = match entry {
            Entry::User(text) => div()
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(10.0))
                .bg(hsla(theme::SURFACE_ELEVATED))
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
                    .bg(hsla(0x14151b))
                    .italic()
                    .text_size(scale::px(12.5))
                    .text_color(hsla(theme::TEXT_FAINT))
                    .child(format!("✻ {shown}"))
                    .into_any_element()
            }
            Entry::Tool(call) => self.render_tool(ix, call, cx),
            Entry::Notice { text, error } => div()
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(hsla(if *error { 0x251417 } else { 0x15171e }))
                .border_1()
                .border_color(hsla(if *error { 0x55242a } else { theme::PANEL_BORDER }))
                .text_size(scale::px(12.0))
                .text_color(hsla(if *error { theme::DANGER } else { theme::TEXT_DIM }))
                .child(text.clone())
                .into_any_element(),
        };
        div()
            .w_full()
            .pb(px(12.0))
            .flex()
            .justify_center()
            .child(div().w_full().max_w(px(900.0)).child(body))
            .into_any_element()
    }

    fn render_tool(&self, ix: usize, call: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        let (mark, color) = match call.status {
            ToolStatus::Pending | ToolStatus::Running => ("●", theme::WARN),
            ToolStatus::Done => ("●", theme::OK),
            ToolStatus::Failed => ("●", theme::DANGER),
        };
        let mut header = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(8.0))
            .child(div().text_size(scale::px(10.0)).text_color(hsla(color)).child(mark))
            .child(div().text_size(scale::px(12.5)).font_weight(FontWeight::MEDIUM).text_color(hsla(theme::TEXT)).child(title(call)));
        for (n, (path, line)) in call.locations.iter().enumerate().take(3) {
            let (path, line) = (path.clone(), *line);
            let label = path.file_name().map_or_else(|| path.display().to_string(), |f| f.to_string_lossy().into_owned());
            header = header.child(
                div()
                    .id(SharedString::from(format!("tool-loc-{ix}-{n}")))
                    .px(px(6.0))
                    .py(px(1.0))
                    .rounded(px(4.0))
                    .bg(hsla(theme::SURFACE_ELEVATED))
                    .border_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .text_size(scale::px(11.5))
                    .text_color(hsla(theme::TEXT_DIM))
                    .cursor_pointer()
                    .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)).text_color(hsla(theme::TEXT)))
                    .child(match line {
                        Some(l) => format!("{label}:{l}"),
                        None => label,
                    })
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.open_location(path.clone(), line, cx))),
            );
        }
        let mut block = div()
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(8.0))
            .bg(hsla(0x13151b))
            .border_1()
            .border_color(hsla(theme::BORDER))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(header);
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
        let mut plan = div()
            .w_full()
            .max_w(px(900.0))
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
                "in_progress" => ("◼", theme::harness_color(self.harness)),
                _ => ("☐", theme::TEXT_DIM),
            };
            plan = plan.child(div().flex().gap(px(8.0)).text_size(scale::px(12.5)).text_color(hsla(color)).child(mark).child(item.text.clone()));
        }
        Some(div().w_full().px(px(22.0)).mb(px(8.0)).flex().justify_center().child(plan))
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
        let card = div()
            .w_full()
            .max_w(px(900.0))
            .px(px(14.0))
            .py(px(12.0))
            .rounded(px(10.0))
            .bg(hsla(theme::SURFACE_ELEVATED))
            .border_1()
            .border_color(hsla(theme::harness_color(self.harness)))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(q.title.clone()))
            .child(options)
            .child(div().flex().gap(px(6.0)).items_center().text_color(hsla(theme::TEXT_FAINT)).text_size(scale::px(11.5)).child(key_hint("1-9")).child("to choose").child(key_hint("Esc")).child("to decline"));
        Some(div().w_full().px(px(22.0)).mb(px(8.0)).flex().justify_center().child(card))
    }

    /// Compact floating dialog over the composer for Model/Provider/Effort or Permission selection.
    fn render_picker_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker?;
        let active_harness = self.pending_harness.unwrap_or(self.harness);
        let accent = hsla(theme::harness_color(active_harness));

        let card = match picker {
            ThreadPicker::Model => {
                // 1. Provider switcher pills
                let mut providers = div().flex().flex_wrap().gap(px(5.0));
                for &h in &self.enabled_harnesses {
                    let selected = h == active_harness;
                    let color = theme::harness_color(h);
                    providers = providers.child(
                        div()
                            .id(SharedString::from(format!("picker-provider-{}", h.name())))
                            .px(px(8.0))
                            .h(px(26.0))
                            .rounded(px(6.0))
                            .flex()
                            .items_center()
                            .gap(px(5.0))
                            .cursor_pointer()
                            .bg(hsla(if selected { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
                            .border_1()
                            .border_color(if selected { hsla(color) } else { hsla(theme::PANEL_BORDER) })
                            .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                            .text_size(scale::px(11.5))
                            .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                            .text_color(hsla(if selected { theme::TEXT } else { theme::TEXT_DIM }))
                            .child(logos::draw(logos::for_harness(h), 12.0, hsla(color)))
                            .child(h.name())
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                v.stage_harness(h, cx);
                            })),
                    );
                }

                // 2. Model list (for active current harness)
                let models_section = if let Some(next) = self.pending_harness {
                    div()
                        .px(px(8.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .bg(hsla(theme::PANEL_BG))
                        .text_size(scale::px(11.5))
                        .text_color(hsla(theme::TEXT_DIM))
                        .child(format!("Next message will hand off this chat to {}. Models load once it starts.", next.name()))
                } else if let Some(model) = self.thread.option("model") {
                    let mut list = div().id("picker-model-list").max_h(px(170.0)).overflow_y_scroll().flex().flex_col().gap(px(2.0));
                    for (n, (value, name)) in model.choices.iter().enumerate() {
                        let (id, val) = (model.id.clone(), value.clone());
                        let current = val == model.current;
                        list = list.child(
                            div()
                                .id(SharedString::from(format!("thread-model-{n}")))
                                .px(px(8.0))
                                .py(px(5.0))
                                .rounded(px(5.0))
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.0))
                                .cursor_pointer()
                                .bg(hsla(if current { theme::ROW_ACTIVE } else { theme::SURFACE_ELEVATED }))
                                .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                                .text_size(scale::px(12.0))
                                .text_color(if current { accent } else { hsla(theme::TEXT) })
                                .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                                .when(current, |d| d.child(icon(IconName::Check, 12.0).text_color(accent)))
                                .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                    v.set_option(id.clone(), val.clone(), cx);
                                    v.picker = None;
                                    cx.notify();
                                })),
                        );
                    }
                    div().flex().flex_col().gap(px(4.0)).child(section_label("MODEL")).child(list)
                } else {
                    div().text_size(scale::px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child("No model choices reported by this harness yet.")
                };

                // 3. Draggable Effort bar (uses ACP `thought_level` choices when available, else `Effort::ALL`)
                let (effort_values, effort_labels, current_effort_ix, option_id): (Vec<String>, Vec<String>, Option<usize>, Option<String>) =
                    if self.pending_harness.is_none()
                        && let Some(opt) = self.thread.option("thought_level")
                        && !opt.choices.is_empty()
                    {
                        let values: Vec<String> = opt.choices.iter().map(|(v, _)| v.clone()).collect();
                        let labels: Vec<String> = opt.choices.iter().map(|(_, n)| short_effort_label(n)).collect();
                        let ix = values.iter().position(|v| v == &opt.current);
                        (values, labels, ix, Some(opt.id.clone()))
                    } else {
                        let values: Vec<String> = Effort::ALL.iter().map(|e| e.name().to_string()).collect();
                        let labels: Vec<String> = vec!["Low".into(), "Med".into(), "High".into(), "XHigh".into(), "Max".into()];
                        let ix = self.pending_effort.as_deref().and_then(|cur| values.iter().position(|v| v == cur));
                        (values, labels, ix, None)
                    };

                let values_rc = Rc::new(effort_values);
                let on_pick: crate::panels::widgets::EffortPick<Self> = Rc::new(move |v, ix, _, cx| {
                    let Some(val) = values_rc.get(ix).cloned() else { return };
                    v.pending_effort = Some(val.clone());
                    if let Some(id) = &option_id {
                        v.set_option(id.clone(), val, cx);
                    }
                    cx.notify();
                });

                let effort_section = div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(section_label("EFFORT"))
                            .child(div().text_size(scale::px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child("Drag or click")),
                    )
                    .child(effort_slider(
                        "thread-effort",
                        &effort_labels,
                        current_effort_ix,
                        accent,
                        self.effort_bounds.clone(),
                        self.effort_dragging.clone(),
                        on_pick,
                        cx,
                    ));

                div()
                    .id("thread-model-dialog")
                    .occlude()
                    .w(px(344.0))
                    .p(px(12.0))
                    .rounded(px(10.0))
                    .bg(hsla(theme::SURFACE_ELEVATED))
                    .border_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(cx.listener(|v, ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "escape" {
                            v.picker = None;
                            v.effort_dragging.set(false);
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(section_label("HARNESS"))
                            .child(div().flex().items_center().gap(px(4.0)).child(key_hint("Esc")).child(div().text_size(scale::px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child("close"))),
                    )
                    .child(providers)
                    .child(div().h(px(1.0)).w_full().bg(hsla(theme::BORDER)))
                    .child(models_section)
                    .child(div().h(px(1.0)).w_full().bg(hsla(theme::BORDER)))
                    .child(effort_section)
            }
            ThreadPicker::Permission => {
                let mut rows = div().flex().flex_col().gap(px(4.0));
                if self.thread.modes.is_empty() {
                    rows = rows.child(div().text_size(scale::px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child("This harness hasn't advertised permission modes."));
                } else {
                    for (i, (id, name)) in self.thread.modes.iter().enumerate() {
                        let current = self.thread.mode.as_deref() == Some(id.as_str());
                        let mode_id = id.clone();
                        rows = rows.child(
                            div()
                                .id(SharedString::from(format!("perm-mode-{i}")))
                                .px(px(10.0))
                                .py(px(7.0))
                                .rounded(px(7.0))
                                .cursor_pointer()
                                .bg(hsla(if current { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
                                .border_1()
                                .border_color(if current { accent } else { hsla(theme::BORDER) })
                                .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .gap(px(1.0))
                                        .child(div().text_size(scale::px(12.5)).font_weight(if current { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).text_color(if current { accent } else { hsla(theme::TEXT) }).child(name.clone()))
                                        .child(div().text_size(scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(mode_description(id))),
                                )
                                .when(current, |d| d.child(icon(IconName::Check, 13.0).text_color(accent)))
                                .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                    v.set_mode(mode_id.clone(), cx);
                                    v.picker = None;
                                    cx.notify();
                                })),
                        );
                    }
                }
                div()
                    .id("thread-permission-dialog")
                    .occlude()
                    .w(px(324.0))
                    .p(px(12.0))
                    .rounded(px(10.0))
                    .bg(hsla(theme::SURFACE_ELEVATED))
                    .border_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(cx.listener(|v, ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "escape" {
                            v.picker = None;
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().flex().items_center().gap(px(6.0)).child(icon(IconName::Lock, 12.0).text_color(hsla(theme::TEXT_DIM))).child(section_label("PERMISSION MODE")))
                            .child(div().flex().items_center().gap(px(4.0)).child(key_hint("Esc")).child(div().text_size(scale::px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child("close"))),
                    )
                    .child(rows)
            }
        };

        Some(
            deferred(
                div()
                    .id("thread-picker-backdrop")
                    .occlude()
                    .absolute()
                    .inset_0()
                    .bg(rgba(0x00000044))
                    .flex()
                    .items_end()
                    .justify_center()
                    .pb(px(96.0))
                    .px(px(22.0))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|v, _: &MouseDownEvent, _, cx| {
                            v.picker = None;
                            v.effort_dragging.set(false);
                            cx.notify();
                        }),
                    )
                    .child(div().w_full().max_w(px(900.0)).flex().justify_start().child(card)),
            )
            .with_priority(15)
            .into_any_element(),
        )
    }

    fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let active_harness = self.pending_harness.unwrap_or(self.harness);
        let accent = hsla(theme::harness_color(active_harness));
        let mut inner = div().w_full().max_w(px(900.0)).flex().flex_col().gap(px(6.0));

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

        if let Some(target) = self.pending_harness {
            let color = theme::harness_color(target);
            inner = inner.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.0))
                    .py(px(5.0))
                    .rounded(px(7.0))
                    .bg(hsla(0x161924))
                    .border_1()
                    .border_color(hsla(color))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(scale::px(11.5))
                            .text_color(hsla(theme::TEXT))
                            .child(logos::draw(logos::for_harness(target), 12.0, hsla(color)))
                            .child(format!("Next message will hand off this conversation to {}", target.name())),
                    )
                    .child(
                        div()
                            .id("cancel-handoff-stage")
                            .cursor_pointer()
                            .text_color(hsla(theme::TEXT_DIM))
                            .hover(|s| s.text_color(hsla(theme::TEXT)))
                            .child(icon(IconName::X, 12.0))
                            .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                                v.pending_harness = None;
                                cx.notify();
                            })),
                    ),
            );
        }

        let focused = self.composer.focus_handle(cx).is_focused(window);
        let border = if focused { accent } else { hsla(theme::PANEL_BORDER) };
        let dock = div()
            .flex()
            .flex_col()
            .rounded(px(12.0))
            .border_1()
            .border_color(border)
            .bg(hsla(theme::SURFACE_ELEVATED))
            .overflow_hidden()
            .child(
                div()
                    .px(px(12.0))
                    .pt(px(8.0))
                    .pb(px(4.0))
                    .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| v.composer.update(cx, |c, cx| c.focus(window, cx))))
                    .child(Textarea::new(&self.composer).appearance(false)),
            )
            .child(self.render_status(cx));

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

    fn render_status(&self, cx: &mut Context<Self>) -> Div {
        let faint = |text: String| div().text_color(hsla(theme::TEXT_FAINT)).child(text);
        let active_harness = self.pending_harness.unwrap_or(self.harness);
        let h_color = theme::harness_color(active_harness);

        let model_label = if self.pending_harness.is_some() {
            active_harness.name().to_string()
        } else if let Some(model) = self.thread.option("model") {
            let name = model.choices.iter().find(|(v, _)| *v == model.current).map_or(model.current.clone(), |(_, n)| n.clone());
            format!("{} · {name}", active_harness.name())
        } else {
            active_harness.name().to_string()
        };

        let effort_badge = if self.pending_harness.is_none()
            && let Some(opt) = self.thread.option("thought_level")
        {
            opt.choices.iter().find(|(v, _)| *v == opt.current).map(|(_, n)| short_effort_label(n))
        } else {
            self.pending_effort.as_deref().map(short_effort_label)
        };

        let mut line = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(6.0))
            .border_t_1()
            .border_color(hsla(theme::BORDER))
            .text_size(scale::px(11.5));

        // Compact Provider + Model + Effort chip (opens the compact picker popover)
        line = line.child(
            div()
                .id("thread-model")
                .px(px(8.0))
                .h(px(24.0))
                .rounded(px(6.0))
                .bg(hsla(if self.picker == Some(ThreadPicker::Model) { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
                .border_1()
                .border_color(hsla(if self.picker == Some(ThreadPicker::Model) { h_color } else { theme::PANEL_BORDER }))
                .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                .flex()
                .items_center()
                .gap(px(5.0))
                .cursor_pointer()
                .child(logos::draw(logos::for_harness(active_harness), 11.5, hsla(h_color)))
                .child(div().max_w(px(220.0)).truncate().text_color(hsla(theme::TEXT)).child(model_label))
                .when_some(effort_badge, |d, badge| {
                    d.child(div().px(px(5.0)).rounded(px(4.0)).bg(hsla(0x232632)).text_size(scale::px(10.0)).text_color(hsla(theme::TEXT_DIM)).child(badge))
                })
                .child(icon(IconName::ChevronDown, 11.0).text_color(hsla(theme::TEXT_FAINT)))
                .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                    v.picker = if v.picker == Some(ThreadPicker::Model) { None } else { Some(ThreadPicker::Model) };
                    cx.notify();
                })),
        );

        // Compact Permission Mode chip (opens the Permission selector dialog)
        if let Some(mode) = self.thread.mode_name() {
            line = line.child(
                div()
                    .id("thread-mode")
                    .px(px(8.0))
                    .h(px(24.0))
                    .rounded(px(6.0))
                    .bg(hsla(if self.picker == Some(ThreadPicker::Permission) { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
                    .border_1()
                    .border_color(hsla(if self.picker == Some(ThreadPicker::Permission) { theme::ACCENT } else { theme::PANEL_BORDER }))
                    .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .cursor_pointer()
                    .child(icon(IconName::Lock, 11.0).text_color(hsla(theme::TEXT_DIM)))
                    .child(div().text_color(hsla(theme::TEXT_DIM)).child(mode.to_string()))
                    .child(icon(IconName::ChevronDown, 11.0).text_color(hsla(theme::TEXT_FAINT)))
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                        v.picker = if v.picker == Some(ThreadPicker::Permission) { None } else { Some(ThreadPicker::Permission) };
                        cx.notify();
                    })),
            );
        }

        if self.working {
            line = line
                .child(div().ml(px(4.0)).flex().items_center().gap(px(5.0)).child(spinner("thread-working", 11.0)).child(faint("Working".into())))
                .child(div().flex().gap(px(4.0)).items_center().child(key_hint("Esc")).child(faint("stop".into())));
        }

        line = line.child(div().flex_1());

        if self.can_review() {
            line = line.child(
                div()
                    .id("thread-review-open")
                    .px(px(8.0))
                    .h(px(24.0))
                    .rounded(px(6.0))
                    .bg(hsla(theme::PANEL_BG))
                    .border_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_color(hsla(theme::TEXT_DIM))
                    .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)).text_color(hsla(theme::TEXT)))
                    .child("Review changes")
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.open_review(cx))),
            );
        }
        if !self.queue.is_empty() {
            line = line.child(faint(format!("{} queued", self.queue.len())));
        }
        if let Some((used, size)) = self.thread.context.filter(|(_, size)| *size > 0) {
            line = line.child(faint(format!("{}% ctx", used * 100 / size)));
        }
        line
    }
}

fn short_effort_label(name: &str) -> String {
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
