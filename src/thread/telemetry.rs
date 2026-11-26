use gpui::{ClickEvent, Context, Div, ParentElement, Styled, div, px};

use super::{ThreadEvent, ThreadView};
use crate::panels::{scale, theme::theme, ui::button as btn, widgets::countdown};
use crate::store::UsageLimits;

pub(crate) fn exhausted_usage(usage: &UsageLimits, now: i64) -> Option<String> {
    let reset = [usage.five_hour.as_ref(), usage.seven_day.as_ref()].into_iter().flatten()
        .filter(|limit| limit.utilization.is_some_and(|used| used >= 1.0))
        .filter_map(|limit| limit.resets_at.filter(|reset| *reset > now))
        .max()?;
    Some(format!("Usage limit reached. Resets in {}.", countdown(reset - now)))
}

impl ThreadView {
    pub(super) fn render_session_notices(&self, cx: &mut Context<Self>) -> Option<Div> {
        let usage_limit = self.usage_limit.as_ref();
        let error = self.error.as_ref();
        if usage_limit.is_none() && error.is_none() {
            return None;
        }
        let mut contents = div().w_full().max_w(px(840.0)).px(px(8.0)).py(px(4.0))
            .flex().flex_col().gap(px(4.0)).text_size(scale::px(12.0));
        if let Some(label) = usage_limit {
            let mut row = div().flex().items_center().flex_wrap().gap(px(8.0))
                .child(div().text_color(theme().warning).child(label.clone()));
            if let Some(target) = self.fallback_model.clone() {
                let btn_label = format!("Continue in {}", target.name);
                row = row.child(
                    btn::text("usage-limit-handoff", btn_label, true).on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(ThreadEvent::Handoff {
                            to: target.harness,
                            model: (!target.id.is_empty()).then_some(target.id.clone()),
                            message: String::new(),
                            effort: None,
                            by_agent: false,
                        });
                    })),
                );
            }
            contents = contents.child(row);
        }
        if let Some(error) = error {
            contents = contents.child(div().text_color(theme().destructive).child(format!("{}: {error}", self.harness.name())));
        }
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
        let usage = UsageLimits { five_hour: limit(200), seven_day: limit(400) };
        assert_eq!(exhausted_usage(&usage, 100), Some("Usage limit reached. Resets in 5m.".into()));
        assert_eq!(exhausted_usage(&usage, 400), None);
    }
}
