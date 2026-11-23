use gpui::{ClickEvent, Context, Div, ParentElement, Styled, div, prelude::FluentBuilder, px};

use super::{Phase, ThreadView};
use crate::panels::{scale, theme::{self, hsla}, widgets::{button, countdown, limit_bar}};
use crate::store::AccountUsage;

pub(crate) fn exhausted_usage(usage: &AccountUsage, now: i64) -> Option<String> {
    let reset = [usage.five_hour.as_ref(), usage.seven_day.as_ref()].into_iter().flatten()
        .filter(|limit| limit.utilization.is_some_and(|used| used >= 1.0))
        .filter_map(|limit| limit.resets_at.filter(|reset| *reset > now))
        .max()?;
    Some(format!("Usage limit reached. Resets in {}.", countdown(reset - now)))
}

impl ThreadView {
    pub(super) fn render_telemetry(&self, cx: &mut Context<Self>) -> Option<Div> {
        let context = self.thread.context.filter(|(_, limit)| *limit > 0);
        let usage_limit = self.usage_limit.as_ref();
        let error = self.error.as_ref();
        if context.is_none() && usage_limit.is_none() && error.is_none()
            && self.mcp_names.is_empty() && self.rule_files.is_empty() && self.last_cost_usd.is_none() && self.thread.cache_usage.turns == 0
        {
            return None;
        }
        let mut contents = div().w_full().max_w(px(840.0)).px(px(8.0)).py(px(4.0))
            .flex().flex_col().gap(px(4.0)).text_size(scale::px(12.0));
        if let Some(label) = usage_limit {
            contents = contents.child(div().text_color(hsla(theme::WARN)).child(label.clone()));
        }
        if let Some(error) = error {
            contents = contents.child(div().text_color(hsla(theme::DANGER)).child(format!("{}: {error}", self.harness.name())));
        }
        let label = context.map_or_else(|| "Session details".to_string(), |(used, limit)| format!("Context {}%", ((used as f64 / limit as f64) * 100.0).round() as u64));
        let mut controls = div().flex().items_center().gap(px(6.0))
            .child(button("chat-telemetry", label, self.telemetry_open).on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                v.telemetry_open = !v.telemetry_open;
                cx.notify();
            })));
        if self.phase == Phase::Ready && !self.working && self.questions.is_empty()
            && !self.compaction.running && context.is_some()
        {
            controls = controls.child(button("chat-compact", "Compact context", false)
                .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                    if !v.working && v.questions.is_empty() && !v.compaction.running {
                        v.try_run_builtin_command("/compact", cx);
                    }
                })));
        }
        contents = contents.child(controls).when(self.telemetry_open, |mut contents| {
            if let Some((used, limit)) = context {
                contents = contents.child(limit_bar(
                    "chat-context-meter",
                    Some((used as f64 / limit as f64).min(1.0) as f32),
                    format!("{used} / {limit} context tokens"),
                    String::new(),
                    Some(hsla(theme::ACCENT)),
                ));
            }
            if let Some(cache) = self.thread.cache_usage.summary() {
                contents = contents.child(div().child(format!("Prompt cache: {cache}")));
            }
            if let Some(cost) = self.last_cost_usd {
                contents = contents.child(div().child(format!("Last turn: ${cost:.4} reported by {}", self.harness.name())));
            }
            if !self.mcp_names.is_empty() {
                contents = contents.child(div().child(format!("MCP servers: {}", self.mcp_names.join(", "))));
            }
            if !self.rule_files.is_empty() {
                contents = contents.child(div().child(format!("Workspace rule files: {}", self.rule_files.join(", "))));
            }
            contents
        });
        Some(div().w_full().px(px(22.0)).flex().justify_center().child(contents))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RateLimit;

    #[test]
    fn recovery_waits_for_both_exhausted_windows() {
        let limit = |reset| Some(RateLimit { utilization: Some(1.0), resets_at: Some(reset), seen_at: 50 });
        let usage = AccountUsage { five_hour: limit(200), seven_day: limit(400) };
        assert_eq!(exhausted_usage(&usage, 100), Some("Usage limit reached. Resets in 5m.".into()));
        assert_eq!(exhausted_usage(&usage, 400), None);
    }
}
