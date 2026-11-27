//! The strip above the message box: one chip each for the plan, background work and the queue, and the one tray a
//! chip opens (`panels::ui::dock`). Messages that need the user stay notices, drawn with the message box.

use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_kit::assets::IconName;

use super::render::{first_line, title};
use super::{Entry, ThreadView, ToolStatus};
use crate::panels::scale;
use crate::panels::theme::theme;
use crate::panels::ui::dock::{self, action, actions, caption, chip, row, tray};
use crate::panels::ui::spinner::spinner;
use crate::panels::widgets::icon;

/// The tray open above the message box; one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tray {
    Plan,
    Background,
    Queue,
}

/// Where background work came from, which decides how its output is read and whether it can be stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BackgroundKind {
    /// A Claude Code background shell: output and stop go through the bridge.
    Shell,
    /// A provider's own background task: output from its log, when it has one.
    Task,
    /// A tool call still running after its turn ended: output from the call, stop interrupts the agent.
    Tool,
}

/// Whether a piece of background work can be stopped from the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stop {
    No,
    Ready,
    Stopping,
}

/// One piece of work the agent left running, as the Background tray lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BackgroundItem {
    pub id: String,
    pub kind: BackgroundKind,
    pub label: String,
    pub output: bool,
    pub stop: Stop,
}

impl BackgroundKind {
    fn glyph(self) -> IconName {
        match self {
            Self::Shell => IconName::Terminal,
            Self::Task => IconName::Play,
            Self::Tool => IconName::Wrench,
        }
    }
}

/// The plan's chip text: the step in progress (or the next one) with how many are done, or that it is done.
fn plan_label(plan: &[crate::harness::thread::PlanItem]) -> String {
    let done = plan.iter().filter(|p| p.status == "completed").count();
    let current = plan.iter().find(|p| p.status == "in_progress").or_else(|| plan.iter().find(|p| p.status != "completed"));
    match current {
        Some(step) => format!("{} · {done}/{}", first_line(&step.text), plan.len()),
        None => format!("Plan done · {done}/{}", plan.len()),
    }
}

fn count_label(n: usize, what: &str) -> String {
    format!("{n} {what}")
}

impl ThreadView {
    /// Everything the agent left running: Claude Code shells, provider tasks, then tool calls still out after their
    /// turn. Child subagents are not here; the right sidebar's Subagents card lists them.
    pub(super) fn background_items(&self) -> Vec<BackgroundItem> {
        let shells = self.provider_shells.iter().map(|shell| BackgroundItem {
            id: shell.id.clone(),
            kind: BackgroundKind::Shell,
            label: shell.label.clone(),
            output: true,
            stop: if shell.stopping { Stop::Stopping } else { Stop::Ready },
        });
        let tasks = self.background_tasks.iter().map(|task| BackgroundItem {
            id: task.id.clone(),
            kind: BackgroundKind::Task,
            label: task.label.clone(),
            output: task.log_path.is_some(),
            stop: Stop::No,
        });
        let tools = self.thread.entries.iter().filter(|_| !self.working).filter_map(|e| match e {
            Entry::Tool(call) if call.status == ToolStatus::Running => Some(BackgroundItem {
                id: call.id.clone(),
                kind: BackgroundKind::Tool,
                label: title(call),
                output: true,
                stop: Stop::Ready,
            }),
            _ => None,
        });
        shells.chain(tasks).chain(tools).collect()
    }

    fn show_background_output(&mut self, item: &BackgroundItem, cx: &mut Context<Self>) {
        match item.kind {
            BackgroundKind::Shell => self.background_shell_action(&item.id, false, cx),
            BackgroundKind::Task => self.open_background_task_output(&item.id, cx),
            BackgroundKind::Tool => self.open_tool_output(&item.id, cx),
        }
    }

    fn stop_background(&mut self, item: &BackgroundItem, cx: &mut Context<Self>) {
        match item.kind {
            BackgroundKind::Shell => self.background_shell_action(&item.id, true, cx),
            BackgroundKind::Task => {}
            BackgroundKind::Tool => {
                self.settle_running_tools();
                self.interrupt(cx);
            }
        }
    }

    /// The tray that shows: the one the user opened while it has something to list, else the queue while it holds
    /// more than when it was last folded.
    pub(super) fn open_tray(&self, queued: usize) -> Option<Tray> {
        let folded = self.queue_folded.get().min(queued);
        self.queue_folded.set(folded);
        self.tray.filter(|tray| self.tray_has_items(*tray)).or((queued > folded).then_some(Tray::Queue))
    }

    fn tray_has_items(&self, tray: Tray) -> bool {
        match tray {
            Tray::Plan => self.plan_shown(),
            Tray::Background => !self.background_items().is_empty(),
            Tray::Queue => self.queue_count() > 0,
        }
    }

    /// Ctrl+T and the chips: open `tray`, or fold it when it is the one showing.
    pub(super) fn toggle_tray(&mut self, tray: Tray) {
        let queued = self.queue_count();
        if self.open_tray(queued) == Some(tray) {
            self.tray = None;
            if tray == Tray::Queue {
                self.queue_folded.set(queued);
            }
        } else {
            self.tray = Some(tray);
        }
    }

    pub(super) fn queue_count(&self) -> usize {
        self.waiting_messages().count() + self.unread_steers().count()
    }

    fn plan_shown(&self) -> bool {
        !self.thread.plan.is_empty() && (self.working || self.thread.plan.iter().any(|p| p.status != "completed") || self.tray == Some(Tray::Plan))
    }

    /// The open tray, then the row of chips; nothing when there is nothing to show.
    pub(super) fn render_dock(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let background = self.background_items();
        let queued = self.queue_count();
        let plan = self.plan_shown();
        let open = self.open_tray(queued);
        let mut parts = Vec::new();
        match open {
            Some(Tray::Plan) => parts.push(self.render_plan_tray().into_any_element()),
            Some(Tray::Background) => parts.push(self.render_background_tray(&background, window, cx).into_any_element()),
            Some(Tray::Queue) => parts.push(self.render_queue_tray(window, cx).into_any_element()),
            None => {}
        }
        let toggle = |tray: Tray| cx.listener(move |v: &mut Self, _: &ClickEvent, _, cx| {
            v.toggle_tray(tray);
            cx.notify();
        });
        let mut chips = Vec::new();
        if plan {
            let running = self.working && self.thread.plan.iter().any(|p| p.status == "in_progress");
            let lead = if running { spinner("dock-plan-spin", 11.0).into_any_element() } else { icon(IconName::ListChecks, 12.0).into_any_element() };
            chips.push(chip("dock-plan", lead, plan_label(&self.thread.plan), open == Some(Tray::Plan)).on_click(toggle(Tray::Plan)));
        }
        if !background.is_empty() {
            let label = count_label(background.len(), "in background");
            chips.push(chip("dock-background", spinner("dock-background-spin", 11.0), label, open == Some(Tray::Background)).on_click(toggle(Tray::Background)));
        }
        if queued > 0 {
            let waiting = self.waiting_messages().count();
            let (lead, label) = if waiting == 0 {
                (spinner("dock-queue-spin", 11.0).into_any_element(), "Steering".to_string())
            } else {
                (icon(IconName::Clock, 12.0).into_any_element(), count_label(waiting, "queued"))
            };
            chips.push(chip("dock-queue", lead, label, open == Some(Tray::Queue)).on_click(toggle(Tray::Queue)));
        }
        if !chips.is_empty() {
            parts.push(div().flex().flex_wrap().items_center().gap(px(dock::GAP)).children(chips).into_any_element());
        }
        parts
    }

    fn render_plan_tray(&self) -> gpui::Div {
        let rows = self.thread.plan.iter().enumerate().map(|(i, item)| {
            let (glyph, color) = match item.status.as_str() {
                "completed" => (IconName::CircleCheck, theme().subtle_foreground),
                "in_progress" => (IconName::CircleDot, theme().primary),
                _ => (IconName::Circle, theme().muted_foreground),
            };
            let label_color = if item.status == "completed" { theme().subtle_foreground } else { theme().foreground };
            row(SharedString::from(format!("dock-plan-{i}")), div().text_color(color).child(icon(glyph, 13.0)), div().text_color(label_color).child(first_line(&item.text)), None)
        });
        tray().children(rows)
    }

    fn render_background_tray(&self, items: &[BackgroundItem], window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let mut panel = tray();
        for (i, item) in items.iter().enumerate() {
            let showing = self.shell_output.as_ref().is_some_and(|out| out.task_id == item.id);
            let key = SharedString::from(format!("dock-bg-{i}"));
            let meta = (item.stop == Stop::Stopping).then(|| SharedString::from("Stopping…"));
            let mut buttons = actions(key.clone(), showing);
            if item.output {
                let it = item.clone();
                buttons = buttons.child(action(format!("dock-bg-output-{i}"), IconName::FileText, if showing { "Hide output" } else { "Show output" })
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        v.toggle_background_output(&it, cx);
                    })));
            }
            if item.stop == Stop::Ready {
                let it = item.clone();
                let label = if item.kind == BackgroundKind::Tool { "Stop (interrupts the agent)" } else { "Stop" };
                buttons = buttons.child(action(format!("dock-bg-stop-{i}"), IconName::Square, label)
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        v.stop_background(&it, cx);
                    })));
            }
            let it = item.clone();
            panel = panel.child(
                row(key, icon(item.kind.glyph(), 13.0), item.label.clone(), meta)
                    .when(showing, |r| r.bg(theme().row_hover))
                    .when(item.output, |r| r.cursor_pointer().on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.toggle_background_output(&it, cx))))
                    .child(buttons),
            );
            if showing && let Some(output) = &self.shell_output {
                panel = panel.child(self.render_output(item, &output.text, window, cx));
            }
        }
        panel
    }

    fn toggle_background_output(&mut self, item: &BackgroundItem, cx: &mut Context<Self>) {
        if self.shell_output.as_ref().is_some_and(|out| out.task_id == item.id) {
            self.shell_output = None;
            cx.notify();
        } else {
            self.show_background_output(item, cx);
        }
    }

    /// The output of the open background item, under its row: refresh, copy, then the text.
    fn render_output(&self, item: &BackgroundItem, text: &str, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let refresh = item.clone();
        let copy = text.to_string();
        let bar = div().flex().items_center().justify_end().gap(px(2.0))
            .child(action("dock-output-refresh", IconName::RefreshCw, "Refresh").on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.show_background_output(&refresh, cx))))
            .child(action("dock-output-copy", IconName::Copy, "Copy output").on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone())))));
        div().mx(px(8.0)).mb(px(4.0)).rounded(px(dock::RADIUS - 1.0)).bg(theme().card).flex().flex_col()
            .child(div().px(px(4.0)).pt(px(2.0)).child(bar))
            .child(self.shell_output_scroll.wire(div().id("dock-output").max_h(px(220.0)), window)
                .px(px(8.0)).pb(px(8.0))
                .font_family(crate::platform::monospace_font()).text_size(scale::px(11.5)).text_color(theme().muted_foreground)
                .child(text.to_string()))
    }

    fn render_queue_tray(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let steering = self.unread_steers().map(first_line).enumerate().map(|(i, line)| {
            row(SharedString::from(format!("steering-{i}")), spinner(SharedString::from(format!("steering-spin-{i}")), 12.0), div().text_color(theme().muted_foreground).child(line), Some("after this step".into()))
        });
        let waiting: Vec<_> = self.waiting_messages().map(|(id, text)| (id, first_line(text))).collect();
        let heading = (!waiting.is_empty()).then(|| caption(if self.queue_held { "Queue paused" } else { "Queued for after this turn" }));
        let rows = waiting.into_iter().map(|(id, text)| {
            let editing = self.queue_edit.as_ref().is_some_and(|edit| edit.id == id);
            let can_act = !editing && !self.interrupting && !self.is_waiting();
            let key = SharedString::from(format!("queue-item-{id}"));
            let mut buttons = actions(key.clone(), false);
            if can_act {
                buttons = buttons.child(action(format!("queued-send-{id}"), IconName::Send, "Send now")
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.send_queued_now(id, cx))));
                if self.queue_edit.is_none() {
                    buttons = buttons.child(action(format!("queued-edit-{id}"), IconName::Pencil, "Edit queued message")
                        .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| v.edit_queued(id, window, cx))));
                }
                for (earlier, glyph, label) in [(true, IconName::ChevronUp, "Move up"), (false, IconName::ChevronDown, "Move down")] {
                    if self.adjacent_queued(id, earlier).is_some() {
                        buttons = buttons.child(action(format!("queued-move-{id}-{earlier}"), glyph, label)
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.move_queued(id, earlier, cx))));
                    }
                }
                buttons = buttons.child(action(format!("queued-remove-{id}"), IconName::X, "Remove queued message")
                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                        if v.unqueue(id, cx).is_some() {
                            v.queue_changed();
                        }
                    })));
            }
            row(key, icon(IconName::Clock, 12.0), div().text_color(theme().muted_foreground).child(text), None)
                .when(editing, |r| r.bg(theme().accent))
                .child(buttons)
        });
        self.queue_scroll.wire(div().id("thread-queue").w_full().max_h(px(200.0)), window)
            .child(tray().children(steering).children(heading).children(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::thread::PlanItem;

    fn step(text: &str, status: &str) -> PlanItem {
        PlanItem { text: text.into(), status: status.into() }
    }

    #[test]
    fn the_plan_chip_names_the_current_step_and_progress() {
        let plan = [step("Read the code", "completed"), step("Write tests\nwith detail", "in_progress"), step("Ship", "pending")];
        assert_eq!(plan_label(&plan), "Write tests · 1/3");
        let plan = [step("Read the code", "completed"), step("Ship", "pending")];
        assert_eq!(plan_label(&plan), "Ship · 1/2");
        let plan = [step("Read the code", "completed")];
        assert_eq!(plan_label(&plan), "Plan done · 1/1");
    }
}
