//! Progress after shadcn's Progress: [`meter`], a labelled bar, and [`limit_bar`], the compact track with its text
//! drawn over the fill. Both turn to the warning color from [`METER_WARN_AT`] and to the destructive one near full.

use gpui::{Div, ElementId, Hsla, InteractiveElement, ParentElement, Stateful, Styled, div, prelude::FluentBuilder, px};

use crate::panels::theme::theme;
use crate::panels::ui::label::section_label;

/// From here up, a meter's bar turns to the warning color.
pub const METER_WARN_AT: f32 = 0.7;

/// Compact usage track, with `fill` behind the percentage and reset countdown.
/// `severity` is always the *used* fraction (0..=1) so warn/danger colors trigger near exhaustion even in "Left" mode.
pub fn limit_bar(
    id: impl Into<ElementId>,
    fill: Option<f32>,
    severity: Option<f32>,
    left: String,
    right: String,
    tone: Option<Hsla>,
    stale: bool,
) -> Stateful<Div> {
    let color = match severity {
        Some(f) if f >= 0.9 => theme().destructive,
        Some(f) if f >= METER_WARN_AT => theme().warning,
        _ => tone.unwrap_or_else(|| theme().primary),
    };
    let height = crate::panels::scale::px(16.0);
    let radius = crate::panels::scale::px(3.0);
    // Faint enough that the text drawn over it stays readable.
    let fill_opacity = if stale { 0.18 } else { 0.32 };
    let fill_bar = div()
        .absolute()
        .top_0()
        .left_0()
        .h(height)
        .w(gpui::relative(fill.unwrap_or(0.0).clamp(0.0, 1.0)))
        .rounded(radius)
        .bg(color.opacity(fill_opacity));
    let percent_color = if fill.is_some() && !stale { theme().foreground } else { theme().subtle_foreground };
    let texts = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .px(crate::panels::scale::px(5.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(crate::panels::scale::px(4.0))
        .text_size(crate::panels::scale::px(10.5))
        .child(div().flex_none().text_color(percent_color).child(left))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(crate::panels::scale::px(10.0))
                .text_color(if stale { theme().subtle_foreground } else { theme().muted_foreground })
                .child(right),
        );
    div().id(id.into()).relative().h(height).w_full().rounded(radius).overflow_hidden().bg(theme().row_hover).child(fill_bar).child(texts)
}

/// Progress bar. `fraction == None` renders the neutral "unavailable" state. `mark` puts a tick across the track at
/// `mark.0` (0..=1), such as where auto-compaction starts, and names it with `mark.1` at the right of the caption.
pub fn meter(label: &str, fraction: Option<f32>, caption: String, tone: Option<Hsla>, mark: Option<(f32, String)>) -> Div {
    let track = theme().row_hover;
    let color = tone.unwrap_or_else(|| match fraction {
        Some(f) if f >= 0.9 => theme().destructive,
        Some(f) if f >= METER_WARN_AT => theme().warning,
        _ => theme().primary,
    });
    let pct = fraction
        .map(|f| format!("{:.0}%", (f * 100.0).round()))
        .unwrap_or_else(|| "—".into());
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .flex()
                .justify_between()
                .items_baseline()
                .child(section_label(label.to_string()))
                .child(
                    div()
                        .text_size(crate::panels::scale::px(11.5))
                        .text_color(if fraction.is_some() {
                            theme().foreground
                        } else {
                            theme().subtle_foreground
                        })
                        .child(pct),
                ),
        )
        .child(
            div()
                .relative()
                .h(px(5.0))
                .w_full()
                .rounded(px(3.0))
                .bg(track)
                .child(match fraction {
                    Some(f) => div()
                        .h_full()
                        .w(gpui::relative(f.clamp(0.0, 1.0)))
                        .rounded(px(3.0))
                        .bg(color),
                    None => div().h_full(),
                })
                .when_some(mark.as_ref(), |d, (at, _)| {
                    // A light tick standing past the track, in a gap of the panel's color so it reads over the fill.
                    d.child(
                        div()
                            .absolute()
                            .top(px(-3.0))
                            .left(gpui::relative(at.clamp(0.0, 1.0)))
                            .ml(px(-2.0))
                            .w(px(4.0))
                            .h(px(11.0))
                            .rounded(px(2.0))
                            .bg(theme().background)
                            .flex()
                            .justify_center()
                            .child(div().w(px(2.0)).h_full().rounded(px(1.0)).bg(theme().muted_foreground)),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .gap(px(8.0))
                .text_size(crate::panels::scale::px(10.5))
                .text_color(theme().subtle_foreground)
                .child(caption)
                .children(mark.map(|(_, note)| div().flex_none().child(note))),
        )
}

