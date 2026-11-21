//! Rendering of the GitHub pane: a list (search, filters) next to a detail view when the pane is wide, stacked
//! with a back button when it is narrow. All behaviour is in `view.rs`; handlers here only build a `GhAct`.
//! Children: `detail` (one item), `compose` (merge box, composer, new item), `changes` (files and diffs), `popup`
//! (filter, label and sort menus). This file has the shared pieces, the list and the header.

mod changes;
mod compose;
mod detail;
mod popup;

use popup::sort_name;

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled, TextAlign, Window, anchored, deferred, div,
    prelude::FluentBuilder, px, AnimationExt,
};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::text::TextView;

use super::model::*;
use crate::panels::widgets::Trigger;
use super::view::*;
use crate::store::now;
use crate::platform::monospace_font;
use crate::panels::theme::{self, hsla};
use gpui_kit::assets::IconName;
use crate::panels::widgets::{ago, avatar, button, button_disabled, button_leading, danger_button, fade_in, icon, icon_button, section_label};

const GREEN: u32 = 0x3fb950;
const PURPLE: u32 = 0xa371f7;
const RED: u32 = 0xf85149;
const GRAY: u32 = 0x8b949e;
const YELLOW: u32 = 0xd29922;
const CARD_BG: u32 = 0x111112;
const CARD_HEAD: u32 = 0x161617;
const CARD_BORDER: u32 = 0x252526;
/// Pane width from which the list and the detail sit side by side.
const WIDE: f32 = 860.0;
const LIST_W: f32 = 380.0;

/// Mix `color` toward white so a label's own color stays readable as text on the dark background.
fn lighten(color: u32, t: f32) -> u32 {
    let ch = |shift: u32| {
        let c = ((color >> shift) & 0xff) as f32;
        (c + (255.0 - c) * t) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

fn label_pill(l: &Label) -> Div {
    div()
        .flex_none()
        .px(px(7.0))
        .h(px(18.0))
        .flex()
        .items_center()
        .rounded(px(9.0))
        .border_1()
        .border_color(hsla(l.color).opacity(0.5))
        .bg(hsla(l.color).opacity(0.16))
        .text_size(crate::panels::scale::px(11.0))
        .text_color(hsla(lighten(l.color, 0.45)))
        .child(l.name.clone())
}

fn pill(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .flex_none()
        .px(px(8.0))
        .h(px(20.0))
        .flex()
        .items_center()
        .rounded(px(10.0))
        .bg(hsla(color))
        .text_size(crate::panels::scale::px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hsla(0x0c0c0d))
        .child(text.into())
}

fn faint(text: impl Into<SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(text.into())
}

fn dim(text: impl Into<SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(12.0)).text_color(hsla(theme::TEXT_DIM)).child(text.into())
}

fn state_style(item: &Item) -> (IconName, u32, &'static str) {
    match (item.kind, item.state) {
        (Kind::Issue, State::Open) => (IconName::CircleDot, GREEN, "Open"),
        (Kind::Issue, _) if item.not_planned => (IconName::CircleSlash, GRAY, "Not planned"),
        (Kind::Issue, _) => (IconName::CircleCheck, PURPLE, "Closed"),
        (Kind::Pull, State::Merged) => (IconName::GitMerge, PURPLE, "Merged"),
        (Kind::Pull, State::Closed) => (IconName::GitPullRequestClosed, RED, "Closed"),
        (Kind::Pull, State::Open) if item.draft => (IconName::GitPullRequestDraft, GRAY, "Draft"),
        (Kind::Pull, State::Open) => (IconName::GitPullRequest, GREEN, "Open"),
    }
}

/// Passed, failed or still running, drawn in the color `check_color` picked.
fn check_icon(color: u32) -> IconName {
    match color {
        GREEN => IconName::CircleCheck,
        RED => IconName::CircleX,
        _ => IconName::CircleDot,
    }
}

fn check_color(state: &str) -> u32 {
    match state {
        "SUCCESS" | "NEUTRAL" | "SKIPPED" => GREEN,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => RED,
        _ => YELLOW,
    }
}

pub(crate) fn markdown(id: String, text: &str) -> TextView {
    // The stock style highlights code with the light theme (dark text), which vanishes on Claudiu's dark panels.
    let style = gpui_kit::component::text::TextViewStyle {
        highlight_theme: gpui_kit::component::highlighter::HighlightTheme::default_dark(),
        is_dark: true,
        ..Default::default()
    };
    TextView::markdown(SharedString::from(id), SharedString::from(text.to_string()))
        .style(style)
        .selectable(true)
        .on_link_click(|url, _, _, cx: &mut App| {
            if url.starts_with("https://") || url.starts_with("http://") {
                cx.open_url(url);
            }
        })
}

fn emoji(content: &str) -> &'static str {
    match content {
        "THUMBS_UP" => "👍",
        "THUMBS_DOWN" => "👎",
        "LAUGH" => "😄",
        "HOORAY" => "🎉",
        "CONFUSED" => "😕",
        "HEART" => "❤️",
        "ROCKET" => "🚀",
        "EYES" => "👀",
        _ => "?",
    }
}

fn assoc_label(a: &str) -> Option<&'static str> {
    match a {
        "OWNER" => Some("Owner"),
        "MEMBER" => Some("Member"),
        "COLLABORATOR" => Some("Collaborator"),
        "CONTRIBUTOR" => Some("Contributor"),
        "FIRST_TIME_CONTRIBUTOR" => Some("First-time contributor"),
        "FIRST_TIMER" => Some("First-timer"),
        _ => None,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A click that runs `act` on the pane.
fn go(cx: &Context<GithubView>, act: GhAct) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |v: &mut GithubView, _: &ClickEvent, window, cx| v.act(act.clone(), window, cx))
}

/// A trigger that opens a picker under itself on mouse-down.
trait Popup: Trigger {
    fn popup(self, cx: &Context<GithubView>, kind: PopKind) -> Self {
        self.dropdown(cx, move |v: &mut GithubView, at, window, cx| v.act(GhAct::Popup(kind.clone(), at.pos), window, cx))
    }
}

impl<T: Trigger> Popup for T {}

struct PopItem {
    label: String,
    swatch: Option<u32>,
    checked: Option<bool>,
    hint: Option<String>,
    act: GhAct,
}

fn item(label: impl Into<String>, act: GhAct) -> PopItem {
    PopItem { label: label.into(), swatch: None, checked: None, hint: None, act }
}

/// Grey bars where the description and the history will be, pulsing until they arrive.
fn skeleton() -> AnyElement {
    let bar = |w: f32, h: f32| div().h(px(h)).w(gpui::relative(w)).rounded(px(4.0)).bg(hsla(0x1f1f21));
    let card = |lines: &[f32]| {
        let mut body = div().flex().flex_col().gap(px(8.0)).p(px(12.0));
        for w in lines {
            body = body.child(bar(*w, 10.0));
        }
        div().flex().flex_col().rounded(px(8.0)).border_1().border_color(hsla(CARD_BORDER)).bg(hsla(CARD_BG)).child(div().h(px(36.0)).px(px(12.0)).flex().items_center().child(bar(0.3, 10.0))).child(body)
    };
    div()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .child(card(&[0.95, 0.8, 0.88, 0.5]))
        .child(card(&[0.7, 0.4]))
        .child(card(&[0.85, 0.6, 0.3]))
        .with_animation("gh-skeleton", gpui::Animation::new(std::time::Duration::from_millis(1200)).repeat().with_easing(gpui::pulsating_between(0.45, 1.0)), |d, t| d.opacity(t))
        .into_any_element()
}

impl Render for GithubView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = match self.phase.clone() {
            Phase::Ready => self.render_ready(window, cx),
            // Cached lists stand in while the repository is looked up.
            Phase::Loading if self.lists.iter().any(|l| l.loaded) => self.render_ready(window, cx),
            other => self.render_state(other, cx),
        };
        let popup = self.render_popup(window, cx);
        div()
            .id("github")
            .key_context("Github")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(theme::PANEL_BG))
            .text_color(hsla(theme::TEXT))
            .text_size(crate::panels::scale::px(13.0))
            .on_key_down(cx.listener(|v, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    v.act(GhAct::Escape, window, cx);
                }
            }))
            .child(body)
            .children(popup)
    }
}

impl GithubView {
    // --------------------------------------------------------------------------------------- not-ready states

    fn render_state(&self, phase: Phase, cx: &mut Context<Self>) -> AnyElement {
        let (title, text, actions): (&str, String, Vec<(&str, GhAct, bool)>) = match phase {
            Phase::Loading => ("Connecting to GitHub…", format!("Looking up the repository of {}.", self.project), vec![]),
            Phase::NoCli => (
                "GitHub CLI not found",
                "Claudiu talks to GitHub through the `gh` command-line tool, so it never handles your credentials. Install it from cli.github.com, then try again.".into(),
                vec![("Get GitHub CLI", GhAct::OpenWeb("https://cli.github.com".into()), true), ("Try again", GhAct::Retry, false)],
            ),
            Phase::SignedOut => (
                "Sign in to GitHub",
                "The GitHub CLI is installed but not signed in. Sign in once in a terminal tab, then come back and press Try again.".into(),
                vec![("Sign in with gh", GhAct::SignIn, true), ("Try again", GhAct::Retry, false)],
            ),
            Phase::NotGithub => (
                "No GitHub repository here",
                format!("{} has no remote that points to GitHub.", self.project),
                vec![("Try again", GhAct::Retry, false)],
            ),
            Phase::Failed(msg) => ("Could not load GitHub", msg, vec![("Try again", GhAct::Retry, true)]),
            Phase::Ready => return div().into_any_element(),
        };
        let mut row = div().mt(px(18.0)).flex().gap(px(8.0));
        for (i, (label, act, primary)) in actions.into_iter().enumerate() {
            row = row.child(button(SharedString::from(format!("gh-state-{i}")), label, primary).on_click(go(cx, act)));
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(32.0))
            .child(div().text_size(crate::panels::scale::px(16.0)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().mt(px(8.0)).max_w(px(460.0)).text_size(crate::panels::scale::px(13.0)).text_color(hsla(theme::TEXT_DIM)).child(text))
            .child(row)
            .into_any_element()
    }

    // ------------------------------------------------------------------------------------------------ layout

    fn render_ready(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let wide = self.width >= WIDE;
        let showing_detail = self.detail.is_some() || matches!(self.mode, Mode::New(_));
        let header = self.render_header(cx);
        let content: AnyElement = if wide {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.render_list(true, window, cx))
                .child(fade_in(div().flex_1().min_w_0().h_full().child(self.render_detail(false, window, cx)), self.detail_key(), 160, 0.0))
                .into_any_element()
        } else if showing_detail {
            fade_in(div().flex_1().min_h_0().child(self.render_detail(true, window, cx)), self.detail_key(), 160, 0.0).into_any_element()
        } else {
            div().flex().flex_1().min_h_0().child(self.render_list(false, window, cx)).into_any_element()
        };
        div().size_full().flex().flex_col().child(header).child(content).into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let repo = self.repo.as_ref().map(|r| r.full()).unwrap_or_default();
        let private = self.info.as_ref().is_some_and(|i| i.private);
        let tab = |kind: Kind, label: &str| {
            let on = self.kind == kind;
            let count = self.lists[if kind == Kind::Issue { 0 } else { 1 }].open;
            let loaded = self.lists[if kind == Kind::Issue { 0 } else { 1 }].loaded;
            div()
                .id(SharedString::from(format!("gh-tab-{label}")))
                .flex()
                .items_center()
                .gap(px(6.0))
                .h(px(26.0))
                .px(px(10.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .text_size(crate::panels::scale::px(12.5))
                .font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .bg(if on { hsla(theme::ROW_ACTIVE) } else { hsla(theme::PANEL_BG) })
                .text_color(hsla(if on { theme::TEXT } else { theme::TEXT_DIM }))
                .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
                .child(label.to_string())
                .when(loaded, |d| d.child(div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(count.to_string())))
                .on_click(go(cx, GhAct::SetKind(kind)))
        };
        let url = self.repo.as_ref().map(|r| r.url.clone()).unwrap_or_default();
        let busy = self.busy.clone();
        let loading = self.lists.iter().any(|l| l.loading);
        let new_kind = self.kind;
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .h(px(42.0))
            .px(px(12.0))
            .border_b_1()
            .border_color(hsla(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .min_w_0()
                    .when(private, |d| d.child(icon(IconName::Lock, 13.0)))
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(repo.to_string()),
                    )
                    .child(tab(Kind::Issue, "Issues"))
                    .child(tab(Kind::Pull, "Pull requests")),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.0))
                    .when_some(busy, |d, b| d.child(dim(b)))
                    .child(icon_button("gh-refresh", if loading { IconName::LoaderCircle } else { IconName::RefreshCw }).on_click(go(cx, GhAct::Refresh)))
                    .child(icon_button("gh-web", IconName::ExternalLink).on_click(go(cx, GhAct::OpenWeb(url))))
                    .child(
                        button("gh-new", if new_kind == Kind::Issue { "New issue" } else { "New pull request" }, true)
                            .on_click(go(cx, GhAct::New(new_kind))),
                    ),
            )
    }

    // ---------------------------------------------------------------------------------------------------- list

    fn chip(&self, id: &str, label: String, active: bool, kind: PopKind, cx: &Context<Self>) -> Stateful<Div> {
        div()
            .id(SharedString::from(format!("gh-chip-{id}")))
            .flex()
            .items_center()
            .h(px(24.0))
            .px(px(9.0))
            .rounded(px(12.0))
            .cursor_pointer()
            .text_size(crate::panels::scale::px(12.0))
            .bg(hsla(if active { 0x2a211d } else { 0x161616 }))
            .text_color(hsla(if active { theme::ACCENT } else { theme::TEXT_DIM }))
            .hover(|s| s.bg(hsla(0x262626)))
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(label.to_string())
            .child(icon(IconName::ChevronDown, 12.0))
            .popup(cx, kind)
    }

    fn render_list(&self, wide: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let i = if self.kind == Kind::Issue { 0 } else { 1 };
        let q = &self.queries[i];
        let list = &self.lists[i];
        let state = state_of(q);
        let state_tab = |id: &str, glyph: IconName, name: &str, count: usize, key: &'static str| {
            let on = state.as_deref() == Some(key);
            div()
                .id(SharedString::from(format!("gh-state-tab-{id}")))
                .flex()
                .items_center()
                .gap(px(5.0))
                .cursor_pointer()
                .text_size(crate::panels::scale::px(12.5))
                .font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .text_color(hsla(if on { theme::TEXT } else { theme::TEXT_DIM }))
                .hover(|s| s.text_color(hsla(theme::TEXT)))
                .child(icon(glyph, 14.0))
                .child(format!("{name} {count}"))
                .on_click(go(cx, GhAct::SetState(Some(key))))
        };
        let open_glyph = if self.kind == Kind::Issue { IconName::CircleDot } else { IconName::GitPullRequest };
        let closed_glyph = if self.kind == Kind::Issue { IconName::CircleCheck } else { IconName::GitMerge };

        let mut chips = div().flex().flex_wrap().gap(px(6.0));
        let one = |key: &str| qualifiers(q, key).into_iter().next();
        let author = one("author");
        chips = chips.child(self.chip("author", author.clone().map_or("Author".into(), |a| format!("Author: {a}")), author.is_some(), PopKind::FilterAuthor, cx));
        let labels = qualifiers(q, "label");
        let label_text = match labels.len() {
            0 => "Label".to_string(),
            1 => format!("Label: {}", labels[0]),
            n => format!("Labels: {n}"),
        };
        chips = chips.child(self.chip("label", label_text, !labels.is_empty() || one("no").as_deref() == Some("label"), PopKind::FilterLabel, cx));
        let assignee = one("assignee");
        chips = chips.child(self.chip("assignee", assignee.clone().map_or("Assignee".into(), |a| format!("Assignee: {a}")), assignee.is_some(), PopKind::FilterAssignee, cx));
        let milestone = one("milestone");
        chips = chips.child(self.chip("milestone", milestone.clone().map_or("Milestone".into(), |a| format!("Milestone: {a}")), milestone.is_some(), PopKind::FilterMilestone, cx));
        if self.kind == Kind::Pull {
            let review = one("review");
            chips = chips.child(self.chip("review", review.clone().map_or("Review".into(), |a| format!("Review: {a}")), review.is_some() || one("draft").is_some(), PopKind::FilterReview, cx));
            let checks = one("status");
            chips = chips.child(self.chip("checks", checks.clone().map_or("Checks".into(), |a| format!("Checks: {a}")), checks.is_some(), PopKind::FilterChecks, cx));
        }
        let sort = one("sort");
        chips = chips.child(self.chip("sort", sort.clone().map_or("Newest".into(), |s| sort_name(&s).to_string()), sort.is_some(), PopKind::FilterSort, cx));
        let default_state = state_of(q).unwrap_or_default();
        let filtered = q.split_whitespace().count() > usize::from(!default_state.is_empty());
        if filtered {
            chips = chips.child(
                div()
                    .id("gh-clear")
                    .h(px(24.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(crate::panels::scale::px(12.0))
                    .text_color(hsla(theme::TEXT_FAINT))
                    .hover(|s| s.text_color(hsla(theme::TEXT)))
                    .child("Clear")
                    .on_click(go(cx, GhAct::SetQuery(format!("is:{}", if default_state.is_empty() { "open" } else { &default_state })))),
            );
        }

        let toolbar = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(8.0))
            .p(px(12.0))
            .border_b_1()
            .border_color(hsla(theme::BORDER))
            .child(div().child(Input::new(&self.query_input).h(px(30.0)).text_size(crate::panels::scale::px(12.5)).prefix(div().text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::Search, 14.0)))))
            .child(
                div()
                    .flex()
                    .gap(px(14.0))
                    .child(state_tab("open", open_glyph, "Open", list.open, "open"))
                    .child(state_tab("closed", closed_glyph, "Closed", list.closed, "closed")),
            )
            .child(chips);

        let t = now();
        let mut rows = div().flex().flex_col();
        for it in &list.items {
            rows = rows.child(self.render_row(it, t, cx));
        }
        if list.items.is_empty() {
            let msg = if let Some(e) = &list.error {
                div().text_color(hsla(theme::DANGER)).child(e.clone())
            } else if list.loading || !list.loaded {
                dim("Loading…")
            } else if self.kind == Kind::Issue && self.info.as_ref().is_some_and(|i| !i.issues_enabled) {
                dim("Issues are turned off for this repository.")
            } else {
                dim(format!("No {} match this search.", if self.kind == Kind::Issue { "issues" } else { "pull requests" }))
            };
            rows = rows.child(div().p(px(18.0)).child(msg));
        }
        let footer = if list.next.is_some() {
            Some(
                div().p(px(12.0)).child(
                    button("gh-more", if list.loading { "Loading…".to_string() } else { format!("Load more ({} of {})", list.items.len(), list.total) }, false)
                        .on_click(go(cx, GhAct::LoadMore)),
                ),
            )
        } else {
            (!list.items.is_empty()).then(|| div().p(px(12.0)).child(faint(plural(list.total, "result", "results"))))
        };
        div()
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .when(wide, |d| d.w(px(LIST_W)).flex_none().border_r_1().border_color(hsla(theme::BORDER)))
            .when(!wide, |d| d.flex_1().min_w_0())
            .child(toolbar)
            .child(self.scrolls[0].wire(div().id("gh-list").flex_1().min_h_0(), window).child(rows).children(footer))
    }

    fn render_row(&self, it: &Item, t: i64, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.detail.as_ref().is_some_and(|d| d.number == it.number && d.kind == it.kind);
        let (glyph, color, _) = state_style(it);
        let mut sub = div().flex().flex_wrap().items_center().gap(px(6.0)).child(faint(format!(
            "#{} {} {} by {}",
            it.number,
            match it.state {
                State::Open => "opened",
                State::Merged => "merged",
                State::Closed => "closed",
            },
            ago(if it.state == State::Open { it.created } else { it.updated }, t),
            it.author
        )));
        for l in it.labels.iter().take(3) {
            sub = sub.child(label_pill(l));
        }
        if it.labels.len() > 3 {
            sub = sub.child(faint(format!("+{}", it.labels.len() - 3)));
        }
        let mut right = div().flex().flex_none().items_center().gap(px(8.0)).text_size(crate::panels::scale::px(11.5));
        if it.kind == Kind::Pull {
            if let Some(c) = &it.checks {
                right = right.child(div().text_color(hsla(check_color(c))).child(icon(check_icon(check_color(c)), 14.0)));
            }
            match it.review.as_deref() {
                Some("APPROVED") => right = right.child(div().text_color(hsla(GREEN)).child("approved")),
                Some("CHANGES_REQUESTED") => right = right.child(div().text_color(hsla(RED)).child("changes")),
                _ => {}
            }
        }
        if it.comments > 0 {
            right = right.child(div().flex().items_center().gap(px(3.0)).text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::MessageSquare, 13.0)).child(it.comments.to_string()));
        }
        div()
            .id(SharedString::from(format!("gh-row-{:?}-{}", it.kind, it.number)))
            .flex()
            .gap(px(10.0))
            .px(px(14.0))
            .py(px(9.0))
            .border_b_1()
            .border_color(hsla(theme::BORDER))
            .cursor_pointer()
            .bg(hsla(if selected { theme::ROW_ACTIVE } else { theme::PANEL_BG }))
            .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
            .child(div().w(px(14.0)).flex_none().pt(px(1.0)).text_color(hsla(color)).child(icon(glyph, 14.0)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_weight(FontWeight::MEDIUM)
                            .child(if it.draft { format!("[Draft] {}", it.title) } else { it.title.clone() }),
                    )
                    .child(sub),
            )
            .child(right)
            .on_click(go(cx, GhAct::Open(it.kind, it.number)))
    }

    // -------------------------------------------------------------------------------------------------- detail

    fn render_empty_detail(&self) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .child(div().text_size(crate::panels::scale::px(14.0)).text_color(hsla(theme::TEXT_DIM)).child("Select an issue or pull request"))
            .child(faint("Search with GitHub syntax, e.g. is:open label:bug author:@me"))
    }
}
