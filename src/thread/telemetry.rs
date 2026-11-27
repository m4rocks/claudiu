use gpui::{ClickEvent, Context, Div, ParentElement, Styled, div, px};

use super::{ThreadEvent, ThreadView};
use crate::panels::{ui::{button as btn, notice::notice, alert::Tone}, widgets::countdown};
use crate::store::UsageLimits;

pub(crate) fn exhausted_usage(usage: &UsageLimits, now: i64) -> Option<String> {
    let reset = [usage.five_hour.as_ref(), usage.seven_day.as_ref()].into_iter().flatten()
        .filter(|limit| limit.utilization.is_some_and(|used| used >= 1.0))
        .filter_map(|limit| limit.resets_at.filter(|reset| *reset > now))
        .max()?;
    Some(format!("Usage limit reached. Resets in {}.", countdown(reset - now)))
}

impl ThreadView {
    pub(super) fn render_usage_notice(&self, label: String, cx: &mut Context<Self>) -> Div {
        let mut row = notice(label, Tone::Warning);
        if let Some(target) = self.fallback_model.clone()
            && self.thread.active_notice(crate::harness::thread::NoticeKind::Handoff).is_none() {
            row = row.child(btn::text("usage-limit-handoff", format!("Continue in {}", target.name), true)
                .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                    if v.thread.active_notice(crate::harness::thread::NoticeKind::Handoff).is_some() { return; }
                    cx.emit(ThreadEvent::Handoff {
                        to: target.harness, model: (!target.id.is_empty()).then_some(target.id.clone()),
                        message: String::new(), effort: None, by_agent: false,
                    });
                })));
        }
        row
    }

    pub(super) fn render_error_notice(&self, text: String, cx: &mut Context<Self>) -> Div {
        let mut row = notice(text, Tone::Destructive);
        if self.authentication_required {
            row = row.child(btn::text("session-sign-in", "Sign in", true).on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ThreadEvent::SignIn))));
        }
        row.child(btn::text("session-retry", "Retry session", false).on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ThreadEvent::Retry))))
    }

    pub(super) fn render_session_notices(&self, cx: &mut Context<Self>) -> Option<Div> {
        let quota_in_timeline = matches!(self.thread.entries.last(), Some(super::Entry::Notice { text, .. })
            if crate::harness::limits::looks_like_usage_limit_error(text));
        let usage_limit = self.usage_limit.as_ref().filter(|_| !quota_in_timeline
            && self.thread.active_notice(crate::harness::thread::NoticeKind::Handoff).is_none());
        let operation_failed = matches!(self.thread.entries.last(), Some(super::Entry::Notice { error: true, event: Some(_), .. }));
        let error = self.error.as_ref().filter(|_| !operation_failed);
        if usage_limit.is_none() && error.is_none() && self.pending_options.is_empty() && !self.stalled {
            return None;
        }
        let mut contents = div().w_full().flex().flex_col().gap(px(crate::panels::ui::dock::GAP));
        if let Some(label) = usage_limit {
            contents = contents.child(self.render_usage_notice(label.clone(), cx));
        }
        if self.stalled {
            contents = contents.child(notice(if self.interrupting { "The agent has not confirmed the stop request." } else { "No updates from the agent for three minutes. It may still be working." }, Tone::Warning)
                .child(btn::text("stalled-stop", "Stop", false).on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.interrupt(cx))))
                .child(btn::text("stalled-reconnect", "Reconnect", false).on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ThreadEvent::Retry)))));
        }
        if !self.pending_options.is_empty() {
            contents = contents.child(notice("Applying session options…", Tone::Default));
        }
        if let Some(error) = error {
            contents = contents.child(self.render_error_notice(format!("{}: {error}", self.harness.name()), cx));
        }
        Some(contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RateLimit;

    #[test]
    fn recovery_waits_for_both_exhausted_windows() {
        let limit = |reset| Some(RateLimit { utilization: Some(1.0), resets_at: Some(reset), seen_at: 50 });
        let usage = UsageLimits { five_hour: limit(200), seven_day: limit(400) };
        assert_eq!(exhausted_usage(&usage, 100), Some("Usage limit reached. Resets in 5m.".into()));
        assert_eq!(exhausted_usage(&usage, 400), None);
    }
}
