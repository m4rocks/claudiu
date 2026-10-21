//! Rendering of the GitHub pane: a list (search, filters) next to a detail view when the pane is wide, stacked
//! with a back button when it is narrow. All behaviour is in `view.rs`; handlers here only build a `GhAct`.

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled, TextAlign, Window, anchored, deferred, div,
    prelude::FluentBuilder, px,
};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::text::TextView;

use super::model::*;
use super::view::*;
use crate::store::now;
use crate::terminal_view::FONT;
use crate::theme::{self, hsla};
use gpui_kit::assets::IconName;
use crate::widgets::{ago, avatar, button, button_disabled, button_leading, danger_button, fade_in, icon, icon_button, section_label};

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
        .text_size(px(11.0))
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
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(hsla(0x0c0c0d))
        .child(text.into())
}

fn faint(text: impl Into<SharedString>) -> Div {
    div().text_size(px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(text.into())
}

fn dim(text: impl Into<SharedString>) -> Div {
    div().text_size(px(12.0)).text_color(hsla(theme::TEXT_DIM)).child(text.into())
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
    TextView::markdown(SharedString::from(id), SharedString::from(text.to_string()))
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

/// A mouse-down that opens a picker at the pointer.
fn popup_at(
    cx: &Context<GithubView>,
    kind: PopKind,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |v: &mut GithubView, ev: &MouseDownEvent, window, cx| v.act(GhAct::Popup(kind.clone(), ev.position), window, cx))
}

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

impl Render for GithubView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = match self.phase.clone() {
            Phase::Ready => self.render_ready(cx),
            other => self.render_state(other, cx),
        };
        let popup = self.render_popup(cx);
        div()
            .id("github")
            .key_context("Github")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(theme::APP_BG))
            .text_color(hsla(theme::TEXT))
            .text_size(px(13.0))
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
            .child(div().text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().mt(px(8.0)).max_w(px(460.0)).text_size(px(13.0)).text_color(hsla(theme::TEXT_DIM)).child(text))
            .child(row)
            .into_any_element()
    }

    // ------------------------------------------------------------------------------------------------ layout

    fn render_ready(&self, cx: &mut Context<Self>) -> AnyElement {
        let wide = self.width >= WIDE;
        let showing_detail = self.detail.is_some() || matches!(self.mode, Mode::New(_));
        let header = self.render_header(cx);
        let content: AnyElement = if wide {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.render_list(true, cx))
                .child(fade_in(div().flex_1().min_w_0().h_full().child(self.render_detail(false, cx)), self.detail_key(), 160, 0.0))
                .into_any_element()
        } else if showing_detail {
            fade_in(div().flex_1().min_h_0().child(self.render_detail(true, cx)), self.detail_key(), 160, 0.0).into_any_element()
        } else {
            div().flex().flex_1().min_h_0().child(self.render_list(false, cx)).into_any_element()
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
                .text_size(px(12.5))
                .font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .bg(if on { hsla(theme::ROW_ACTIVE) } else { hsla(theme::APP_BG) })
                .text_color(hsla(if on { theme::TEXT } else { theme::TEXT_DIM }))
                .hover(|s| s.bg(hsla(theme::ROW_HOVER)))
                .child(label.to_string())
                .when(loaded, |d| d.child(div().text_size(px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(count.to_string())))
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
                            .child(format!("{repo}")),
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
            .text_size(px(12.0))
            .bg(hsla(if active { 0x2a211d } else { 0x161616 }))
            .text_color(hsla(if active { theme::ACCENT } else { theme::TEXT_DIM }))
            .hover(|s| s.bg(hsla(0x262626)))
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(label.to_string())
            .child(icon(IconName::ChevronDown, 12.0))
            .on_mouse_down(MouseButton::Left, popup_at(cx, kind))
    }

    fn render_list(&self, wide: bool, cx: &mut Context<Self>) -> Div {
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
                .text_size(px(12.5))
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
                    .text_size(px(12.0))
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
            .child(div().child(Input::new(&self.query_input).h(px(30.0)).text_size(px(12.5)).prefix(div().text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::Search, 14.0)))))
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
            .child(div().id("gh-list").flex_1().min_h_0().overflow_y_scroll().child(rows).children(footer))
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
        let mut right = div().flex().flex_none().items_center().gap(px(8.0)).text_size(px(11.5));
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
            .bg(hsla(if selected { theme::ROW_ACTIVE } else { theme::APP_BG }))
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
            .child(div().text_size(px(14.0)).text_color(hsla(theme::TEXT_DIM)).child("Select an issue or pull request"))
            .child(faint("Search with GitHub syntax, e.g. is:open label:bug author:@me"))
    }

    fn render_detail(&self, stacked: bool, cx: &mut Context<Self>) -> AnyElement {
        if let Mode::New(kind) = self.mode {
            return self.render_new(kind, stacked, cx).into_any_element();
        }
        let Some(ds) = &self.detail else { return self.render_empty_detail().into_any_element() };
        let back = stacked.then(|| {
            div()
                .flex_none()
                .px(px(12.0))
                .pt(px(8.0))
                .child(button_leading("gh-back", IconName::ArrowLeft, "Back to list", false).on_click(go(cx, GhAct::Back)))
        });
        let Some(d) = &ds.data else {
            let msg = match &ds.error {
                Some(e) => div().text_color(hsla(theme::DANGER)).child(e.clone()),
                None => dim("Loading…"),
            };
            return div().size_full().flex().flex_col().children(back).child(div().p(px(18.0)).child(msg)).into_any_element();
        };
        let t = now();
        let can_triage = self.info.as_ref().is_some_and(|i| i.can_triage());
        let it = &d.item;
        let (_, color, state_name) = state_style(it);
        let editing_title = self.editing == Some(Edit::Title);

        let title_row = if editing_title {
            div()
                .flex()
                .gap(px(8.0))
                .items_center()
                .child(div().flex_1().child(Input::new(&self.edit_title).h(px(32.0))))
                .child(button("gh-title-save", "Save", true).on_click(go(cx, GhAct::EditSave)))
                .child(button("gh-title-cancel", "Cancel", false).on_click(go(cx, GhAct::EditCancel)))
        } else {
            div()
                .flex()
                .items_start()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .items_baseline()
                        .gap(px(8.0))
                        .text_size(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(it.title.clone())
                        .child(div().text_color(hsla(theme::TEXT_FAINT)).font_weight(FontWeight::NORMAL).child(format!("#{}", it.number))),
                )
                .when(d.can_update || d.mine, |r| r.child(icon_button("gh-edit-title", IconName::Pencil).on_click(go(cx, GhAct::EditStart(Edit::Title)))))
        };

        let mut status = div().flex().flex_wrap().items_center().gap(px(8.0)).child(pill(state_name, color));
        status = status.child(dim(format!(
            "{} {} {} · {}",
            it.author,
            match it.state {
                State::Merged => "merged",
                _ => "opened",
            },
            ago(it.created, t),
            plural(it.comments as usize, "comment", "comments")
        )));
        if let Some(pr) = &d.pr {
            status = status.child(
                div()
                    .font_family(FONT)
                    .text_size(px(11.5))
                    .px(px(7.0))
                    .py(px(1.0))
                    .rounded(px(5.0))
                    .bg(hsla(0x1a1a1b))
                    .text_color(hsla(theme::TEXT_DIM))
                    .child(format!("{}{} → {}", if pr.cross_repo { format!("{}:", pr.head_repo.split('/').next().unwrap_or("")) } else { String::new() }, it.head, it.base)),
            );
            status = status.child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .text_size(px(12.0))
                    .child(div().text_color(hsla(GREEN)).child(format!("+{}", pr.additions)))
                    .child(div().text_color(hsla(RED)).child(format!("−{}", pr.deletions))),
            );
        }

        let url = it.url.clone();
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(px(6.0))
            .child(button("gh-claude", "Ask Claude", true).child(div().ml(px(4.0)).child(icon(IconName::ChevronDown, 12.0))).on_mouse_down(MouseButton::Left, popup_at(cx, PopKind::Claude)))
            .child(button("gh-open-web", "Open on GitHub", false).child(div().ml(px(4.0)).child(icon(IconName::ExternalLink, 12.0))).on_click(go(cx, GhAct::OpenWeb(url))))
            .child(button("gh-more-menu", "", false).child(icon(IconName::Ellipsis, 14.0)).on_mouse_down(MouseButton::Left, popup_at(cx, PopKind::More)))
            .when(ds.loading, |d| d.child(dim("Refreshing…")));

        let header = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(8.0))
            .px(px(16.0))
            .pt(px(12.0))
            .pb(px(10.0))
            .border_b_1()
            .border_color(hsla(theme::BORDER))
            .child(title_row)
            .child(status)
            .child(actions);

        // ---- meta: labels, assignees, milestone, reviewers
        let edit_btn = |id: &str, kind: PopKind, cx: &Context<Self>| {
            can_triage.then(|| {
                div()
                    .id(SharedString::from(format!("gh-meta-{id}")))
                    .cursor_pointer()
                    .px(px(6.0))
                    .rounded(px(5.0))
                    .text_size(px(12.0))
                    .text_color(hsla(theme::TEXT_FAINT))
                    .hover(|s| s.bg(hsla(0x262626)).text_color(hsla(theme::TEXT)))
                    .child("＋ Edit")
                    .on_mouse_down(MouseButton::Left, popup_at(cx, kind))
            })
        };
        let meta_row = |name: &str, value: Div, edit: Option<Stateful<Div>>| {
            div()
                .flex()
                .items_start()
                .gap(px(10.0))
                .child(div().w(px(84.0)).flex_none().pt(px(2.0)).child(section_label(name.to_uppercase())))
                .child(div().flex_1().min_w_0().child(value))
                .children(edit)
        };
        let mut labels = div().flex().flex_wrap().gap(px(5.0));
        for l in &it.labels {
            labels = labels.child(label_pill(l));
        }
        if it.labels.is_empty() {
            labels = labels.child(faint("None yet"));
        }
        let mut people = div().flex().flex_wrap().gap(px(10.0));
        for u in &it.assignees {
            people = people.child(div().flex().items_center().gap(px(5.0)).child(avatar(&u.login, 16.0)).child(dim(u.login.clone())));
        }
        if it.assignees.is_empty() {
            people = people.child(faint("No one"));
        }
        let mut meta = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(meta_row("Labels", labels, edit_btn("labels", PopKind::Labels, cx)))
            .child(meta_row("Assignees", people, edit_btn("assignees", PopKind::Assignees, cx)))
            .child(meta_row(
                "Milestone",
                div().child(match &it.milestone {
                    Some((_, m)) => dim(m.clone()),
                    None => faint("None"),
                }),
                edit_btn("milestone", PopKind::Milestone, cx),
            ));
        if let Some(pr) = &d.pr {
            let mut rv = div().flex().flex_wrap().gap(px(10.0));
            for (who, state) in &pr.reviews {
                let (g, c) = match state.as_str() {
                    "APPROVED" => (IconName::CircleCheck, GREEN),
                    "CHANGES_REQUESTED" => (IconName::CircleX, RED),
                    _ => (IconName::CircleDot, GRAY),
                };
                rv = rv.child(div().flex().items_center().gap(px(4.0)).child(div().text_color(hsla(c)).child(icon(g, 13.0))).child(dim(who.clone())));
            }
            for who in pr.requested.iter().filter(|w| !pr.reviews.iter().any(|(r, _)| r == *w)) {
                rv = rv.child(div().flex().items_center().gap(px(4.0)).child(div().text_color(hsla(YELLOW)).child(icon(IconName::Clock, 13.0))).child(dim(who.clone())));
            }
            if pr.reviews.is_empty() && pr.requested.is_empty() {
                rv = rv.child(faint("No reviews yet"));
            }
            meta = meta.child(meta_row("Reviewers", rv, edit_btn("reviewers", PopKind::Reviewers, cx)));
        }

        // ---- tabs (pull requests)
        let tabs = d.pr.as_ref().map(|pr| {
            let tab = |id: &str, name: String, which: DetailTab, cx: &mut Context<Self>| {
                let on = ds.tab == which;
                div()
                    .id(SharedString::from(format!("gh-dtab-{id}")))
                    .px(px(12.0))
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .border_b_2()
                    .border_color(if on { hsla(theme::ACCENT) } else { hsla(theme::APP_BG) })
                    .text_color(hsla(if on { theme::TEXT } else { theme::TEXT_DIM }))
                    .hover(|s| s.text_color(hsla(theme::TEXT)))
                    .child(name)
                    .on_click(go(cx, GhAct::Tab(which)))
            };
            div()
                .flex()
                .border_b_1()
                .border_color(hsla(theme::BORDER))
                .child(tab("conv", "Conversation".into(), DetailTab::Conversation, cx))
                .child(tab("commits", format!("Commits {}", pr.commits), DetailTab::Commits, cx))
                .child(tab("files", format!("Files changed {}", pr.files), DetailTab::Files, cx))
        });

        let content: AnyElement = match ds.tab {
            DetailTab::Conversation => self.render_conversation(d, ds, t, cx).into_any_element(),
            DetailTab::Commits => self.render_commits(d, t).into_any_element(),
            DetailTab::Files => self.render_files(ds, cx).into_any_element(),
        };
        let scroll = div()
            .id("gh-detail")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(div().px(px(16.0)).py(px(12.0)).child(meta))
            .children(tabs)
            .child(div().px(px(16.0)).py(px(12.0)).child(content));
        div().size_full().flex().flex_col().children(back).child(header).child(scroll).into_any_element()
    }

    fn reactions_row(&self, subject: &str, list: &[Reaction], can_react: bool, cx: &mut Context<Self>) -> Div {
        let mut row = div().flex().flex_wrap().items_center().gap(px(6.0));
        for r in list.iter().filter(|r| r.count > 0) {
            let act = GhAct::React(subject.to_string(), r.content.clone(), r.mine);
            row = row.child(
                div()
                    .id(SharedString::from(format!("gh-react-{subject}-{}", r.content)))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .h(px(22.0))
                    .px(px(8.0))
                    .rounded(px(11.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .border_1()
                    .border_color(hsla(if r.mine { theme::ACCENT } else { CARD_BORDER }))
                    .bg(hsla(if r.mine { 0x2a211d } else { CARD_HEAD }))
                    .hover(|s| s.bg(hsla(0x262626)))
                    .child(format!("{} {}", emoji(&r.content), r.count))
                    .on_click(go(cx, act)),
            );
        }
        if can_react {
            row = row.child(
                div()
                    .id(SharedString::from(format!("gh-addreact-{subject}")))
                    .flex()
                    .items_center()
                    .h(px(22.0))
                    .px(px(8.0))
                    .rounded(px(11.0))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(hsla(theme::TEXT_FAINT))
                    .hover(|s| s.bg(hsla(0x262626)).text_color(hsla(theme::TEXT)))
                    .child(icon(IconName::HeartPlus, 14.0))
                    .on_mouse_down(MouseButton::Left, popup_at(cx, PopKind::React(subject.to_string()))),
            );
        }
        row
    }

    /// One comment-like card: header with author and time, markdown body (or the editor), reactions.
    #[allow(clippy::too_many_arguments)]
    fn card(
        &self,
        key: &str,
        author: &str,
        assoc: &str,
        verb: &str,
        when: i64,
        edited: bool,
        body: &str,
        reactions: Option<(&str, &[Reaction])>,
        menu: Option<PopKind>,
        edit_target: Option<Edit>,
        t: i64,
        cx: &mut Context<Self>,
    ) -> Div {
        let editing = edit_target.is_some() && self.editing == edit_target;
        let head = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .h(px(36.0))
            .px(px(12.0))
            .bg(hsla(CARD_HEAD))
            .border_b_1()
            .border_color(hsla(CARD_BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .min_w_0()
                    .child(avatar(author, 20.0))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(author.to_string()))
                    .child(dim(format!("{verb} {}{}", ago(when, t), if edited { " · edited" } else { "" })))
                    .children(assoc_label(assoc).map(|a| div().px(px(6.0)).rounded(px(8.0)).border_1().border_color(hsla(CARD_BORDER)).text_size(px(10.5)).text_color(hsla(theme::TEXT_DIM)).child(a))),
            )
            .children(menu.map(|m| {
                div()
                    .id(SharedString::from(format!("gh-card-menu-{key}")))
                    .px(px(6.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .text_color(hsla(theme::TEXT_DIM))
                    .hover(|s| s.bg(hsla(0x262626)).text_color(hsla(theme::TEXT)))
                    .child("⋯")
                    .on_mouse_down(MouseButton::Left, popup_at(cx, m))
            }));
        let body_el: AnyElement = if editing {
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(Textarea::new(&self.edit_body))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(6.0))
                        .child(button(SharedString::from(format!("gh-edit-cancel-{key}")), "Cancel", false).on_click(go(cx, GhAct::EditCancel)))
                        .child(button(SharedString::from(format!("gh-edit-save-{key}")), "Save", true).on_click(go(cx, GhAct::EditSave))),
                )
                .into_any_element()
        } else if body.trim().is_empty() {
            faint("No description provided.").into_any_element()
        } else {
            div().text_size(px(13.5)).child(markdown(format!("md-{key}"), body)).into_any_element()
        };
        let react = reactions.map(|(subject, list)| self.reactions_row(subject, list, true, cx));
        div()
            .rounded(px(8.0))
            .border_1()
            .border_color(hsla(CARD_BORDER))
            .bg(hsla(CARD_BG))
            .overflow_hidden()
            .child(head)
            .child(div().p(px(12.0)).flex().flex_col().gap(px(10.0)).child(body_el).children(react))
    }

    fn render_conversation(&self, d: &Detail, ds: &DetailState, t: i64, cx: &mut Context<Self>) -> Div {
        let it = &d.item;
        let mut col = div().flex().flex_col().gap(px(12.0));
        let body_edit = (d.can_update || d.mine).then_some(Edit::Body);
        col = col.child(self.card(
            "body",
            &it.author,
            &d.assoc,
            "opened",
            it.created,
            d.edited,
            &d.body,
            Some((&it.id, &d.reactions)),
            body_edit.clone().map(|_| PopKind::Comment("body".into(), true, false, it.url.clone())),
            body_edit,
            t,
            cx,
        ));
        for (n, e) in d.timeline.iter().enumerate() {
            match e {
                Entry::Comment(c) => {
                    let menu = PopKind::Comment(c.id.clone(), c.can_edit, c.can_delete, format!("{}#issuecomment-{}", it.url, c.id));
                    col = col.child(self.card(
                        &c.id,
                        &c.author,
                        &c.assoc,
                        "commented",
                        c.created,
                        c.edited,
                        &c.body,
                        Some((&c.id, &c.reactions)),
                        Some(menu),
                        c.can_edit.then(|| Edit::Comment(c.id.clone())),
                        t,
                        cx,
                    ));
                }
                Entry::Review(r) => col = col.child(self.render_review(r, n, t, cx)),
                Entry::Event(ev) => {
                    col = col.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(4.0))
                            .child(div().text_color(hsla(theme::TEXT_FAINT)).child("•"))
                            .child(div().text_size(px(12.0)).font_weight(FontWeight::MEDIUM).text_color(hsla(theme::TEXT_DIM)).child(ev.actor.clone()))
                            .child(dim(ev.what.clone()))
                            .children(ev.label.as_ref().map(label_pill))
                            .child(faint(ago(ev.created, t))),
                    );
                }
                Entry::Commit(c) => col = col.child(self.render_commit_line(c, t)),
            }
        }
        if d.next.is_some() {
            col = col.child(div().child(button("gh-more-timeline", if ds.more_loading { "Loading…" } else { "Load more activity" }, false).on_click(go(cx, GhAct::LoadMoreTimeline))));
        }
        if let Some(pr) = &d.pr {
            col = col.child(self.render_merge_box(d, pr, cx));
        }
        col.child(self.render_composer(d, cx))
    }

    fn render_commit_line(&self, c: &CommitEntry, t: i64) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(4.0))
            .child(div().text_color(hsla(theme::TEXT_FAINT)).child(icon(IconName::GitCommitHorizontal, 14.0)))
            .child(div().font_family(FONT).text_size(px(11.5)).text_color(hsla(theme::ACCENT)).child(c.short.clone()))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(c.headline.clone()))
            .child(faint(format!("{} · {}", c.author, ago(c.created, t))))
    }

    fn render_commits(&self, d: &Detail, t: i64) -> Div {
        let mut col = div().flex().flex_col().gap(px(8.0));
        let commits: Vec<&CommitEntry> = d.timeline.iter().filter_map(|e| if let Entry::Commit(c) = e { Some(c) } else { None }).collect();
        for c in &commits {
            col = col.child(self.render_commit_line(c, t));
        }
        if commits.is_empty() {
            col = col.child(faint("No commits loaded."));
        }
        if d.next.is_some() {
            col = col.child(faint("More activity exists: open Conversation and load more to see later commits."));
        }
        col
    }

    fn render_review(&self, r: &Review, n: usize, t: i64, cx: &mut Context<Self>) -> Div {
        let (verb, color) = match r.state.as_str() {
            "APPROVED" => ("approved these changes", GREEN),
            "CHANGES_REQUESTED" => ("requested changes", RED),
            "DISMISSED" => ("had a review dismissed", GRAY),
            "PENDING" => ("has a pending review", YELLOW),
            _ => ("reviewed", GRAY),
        };
        let mut col = div()
            .rounded(px(8.0))
            .border_1()
            .border_color(hsla(color).opacity(0.5))
            .bg(hsla(CARD_BG))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(36.0))
                    .px(px(12.0))
                    .bg(hsla(CARD_HEAD))
                    .child(avatar(&r.author, 20.0))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(r.author.clone()))
                    .child(div().text_color(hsla(color)).child(verb))
                    .child(faint(ago(r.created, t))),
            );
        if !r.body.trim().is_empty() {
            col = col.child(div().p(px(12.0)).text_size(px(13.5)).child(markdown(format!("md-review-{n}"), &r.body)));
        }
        for (k, c) in r.comments.iter().enumerate() {
            let hunk: Vec<&str> = c.hunk.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
            let mut code = div().flex().flex_col().font_family(FONT).text_size(px(11.5)).bg(hsla(0x0e0e0f)).py(px(4.0)).overflow_hidden();
            for l in hunk {
                let bg = if l.starts_with('+') { Some(GREEN) } else if l.starts_with('-') { Some(RED) } else { None };
                code = code.child(div().px(px(10.0)).whitespace_nowrap().when_some(bg, |d, c| d.bg(hsla(c).opacity(0.14))).child(if l.is_empty() { " ".to_string() } else { l.to_string() }));
            }
            col = col.child(
                div()
                    .border_t_1()
                    .border_color(hsla(CARD_BORDER))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .px(px(12.0))
                            .py(px(6.0))
                            .child(div().font_family(FONT).text_size(px(11.5)).text_color(hsla(theme::TEXT_DIM)).child(format!("{}{}", c.path, c.line.map(|l| format!(":{l}")).unwrap_or_default())))
                            .children(c.outdated.then(|| faint("outdated"))),
                    )
                    .child(code)
                    .child(div().p(px(12.0)).text_size(px(13.5)).child(markdown(format!("md-rc-{n}-{k}"), &format!("**{}**: {}", c.author, c.body)))),
            );
        }
        let _ = cx;
        col
    }

    // ---------------------------------------------------------------------------------------------- merge box

    fn render_merge_box(&self, d: &Detail, pr: &Pr, cx: &mut Context<Self>) -> Div {
        let it = &d.item;
        let info = self.info.clone().unwrap_or_default();
        let line = |glyph: IconName, color: u32, title: String, sub: Option<String>| {
            div()
                .flex()
                .items_start()
                .gap(px(10.0))
                .child(div().w(px(16.0)).flex_none().text_color(hsla(color)).child(icon(glyph, 16.0)))
                .child(div().flex().flex_col().child(div().font_weight(FontWeight::MEDIUM).child(title)).children(sub.map(faint)))
        };
        let mut rows = div().flex().flex_col().gap(px(10.0));
        let mut actions = div().flex().flex_wrap().items_center().gap(px(8.0));
        let outline = match it.state {
            State::Merged => PURPLE,
            State::Closed => RED,
            State::Open => CARD_BORDER,
        };
        match it.state {
            State::Merged => {
                rows = rows.child(line(IconName::GitMerge, PURPLE, "Pull request merged".into(), pr.merged_by.clone().map(|b| format!("Merged by {b}"))));
                let branch_gone = d.timeline.iter().any(|e| matches!(e, Entry::Event(ev) if ev.what.starts_with("deleted the")));
                if !pr.cross_repo && !branch_gone && !it.head.is_empty() && info.can_write() && !info.delete_branch_on_merge {
                    actions = actions.child(button("gh-del-branch", format!("Delete {}", it.head), false).on_click(go(cx, GhAct::DeleteBranch)));
                }
            }
            State::Closed => {
                rows = rows.child(line(IconName::GitPullRequestClosed, RED, "This pull request is closed".into(), None));
                actions = actions.child(button("gh-reopen-pr", "Reopen pull request", false).on_click(go(cx, GhAct::ReopenItem)));
            }
            State::Open => {
                // reviews
                match it.review.as_deref() {
                    Some("APPROVED") => rows = rows.child(line(IconName::CircleCheck, GREEN, "Changes approved".into(), None)),
                    Some("CHANGES_REQUESTED") => rows = rows.child(line(IconName::CircleX, RED, "Changes requested".into(), Some("A reviewer asked for changes before this can merge.".into()))),
                    Some("REVIEW_REQUIRED") => rows = rows.child(line(IconName::CircleDot, YELLOW, "Review required".into(), Some("At least one approving review is required.".into()))),
                    _ => {}
                }
                // checks
                if !pr.checks.is_empty() {
                    let bad = pr.checks.iter().filter(|c| check_color(&c.result) == RED).count();
                    let wait = pr.checks.iter().filter(|c| check_color(&c.result) == YELLOW).count();
                    let ok = pr.checks.len() - bad - wait;
                    let (g, c, title) = if bad > 0 {
                        (IconName::CircleX, RED, format!("{bad} failing"))
                    } else if wait > 0 {
                        (IconName::CircleDot, YELLOW, format!("{wait} pending"))
                    } else {
                        (IconName::CircleCheck, GREEN, "All checks have passed".to_string())
                    };
                    let mut r = line(g, c, title, Some(format!("{} successful, {} failing, {} pending", ok, bad, wait)));
                    for ck in pr.checks.iter().take(12) {
                        let url = ck.url.clone();
                        r = r.child(
                            div()
                                .id(SharedString::from(format!("gh-check-{}", ck.name)))
                                .ml(px(26.0))
                                .flex()
                                .gap(px(6.0))
                                .cursor_pointer()
                                .text_size(px(12.0))
                                .hover(|s| s.text_color(hsla(theme::TEXT)))
                                .text_color(hsla(theme::TEXT_DIM))
                                .child(div().text_color(hsla(check_color(&ck.result))).child(icon(check_icon(check_color(&ck.result)), 13.0)))
                                .child(ck.name.clone())
                                .on_click(go(cx, GhAct::OpenWeb(url))),
                        );
                    }
                    rows = rows.child(r.flex_col());
                }
                // conflicts / behind
                if pr.mergeable == "CONFLICTING" {
                    rows = rows.child(line(IconName::CircleX, RED, "This branch has conflicts".into(), Some("Resolve them before merging (ask Claude, or check the branch out).".into())));
                } else if pr.merge_state == "BEHIND" && info.can_write() {
                    rows = rows.child(line(IconName::CircleDot, YELLOW, "This branch is out of date with the base branch".into(), None));
                    actions = actions.child(button("gh-update-branch", "Update branch", false).on_click(go(cx, GhAct::UpdateBranch)));
                }
                if let Some((who, method)) = &pr.auto_merge {
                    rows = rows.child(line(IconName::Clock, PURPLE, format!("Auto-merge enabled by {who}"), Some(format!("Will merge automatically ({}) once requirements pass.", method.to_lowercase()))));
                    actions = actions.child(button("gh-auto-off", "Disable auto-merge", false).on_click(go(cx, GhAct::AutoMerge(false))));
                }
                if it.draft {
                    rows = rows.child(line(IconName::GitPullRequestDraft, GRAY, "This pull request is still a work in progress".into(), Some("Draft pull requests cannot be merged.".into())));
                    if info.can_write() || d.mine {
                        actions = actions.child(button("gh-ready", "Ready for review", true).on_click(go(cx, GhAct::Ready)));
                    }
                } else if info.can_write() && pr.mergeable != "CONFLICTING" {
                    let allowed: Vec<(&'static str, &str)> = [("MERGE", "Create a merge commit", info.allow_merge), ("SQUASH", "Squash and merge", info.allow_squash), ("REBASE", "Rebase and merge", info.allow_rebase)]
                        .into_iter()
                        .filter(|(_, _, ok)| *ok)
                        .map(|(m, l, _)| (m, l))
                        .collect();
                    let method = allowed.iter().find(|(m, _)| *m == self.merge_method).or(allowed.first()).copied();
                    let blocked = pr.merge_state == "BLOCKED";
                    let unstable = pr.merge_state == "UNSTABLE";
                    if let Some((m, label)) = method {
                        let btn_label = if blocked && pr.can_admin_merge {
                            format!("{label} (override)")
                        } else if unstable {
                            format!("{label} anyway")
                        } else {
                            label.to_string()
                        };
                        if blocked && !pr.can_admin_merge {
                            actions = actions.child(button_disabled("gh-merge-disabled", "Merging is blocked"));
                            if info.allow_auto_merge && pr.can_auto_merge && pr.auto_merge.is_none() {
                                actions = actions.child(button("gh-auto-on", "Enable auto-merge", false).on_click(go(cx, GhAct::AutoMerge(true))));
                            }
                        } else {
                            let main = if blocked || unstable { danger_button("gh-merge", btn_label) } else { button("gh-merge", btn_label, true) };
                            actions = actions.child(
                                div()
                                    .flex()
                                    .child(main.on_click(go(cx, GhAct::Merge(m))))
                                    .when(allowed.len() > 1, |d| d.child(icon_button("gh-merge-pick", IconName::ChevronDown).ml(px(2.0)).on_mouse_down(MouseButton::Left, popup_at(cx, PopKind::Merge)))),
                            );
                        }
                    } else {
                        actions = actions.child(faint("This repository does not allow merging pull requests."));
                    }
                    if blocked {
                        rows = rows.child(line(IconName::CircleDot, YELLOW, "Merging is blocked".into(), Some("Required reviews or checks have not been met.".into())));
                    } else if pr.mergeable == "MERGEABLE" {
                        rows = rows.child(line(IconName::CircleCheck, GREEN, "No conflicts with the base branch".into(), Some("Merging can be performed automatically.".into())));
                    }
                }
            }
        }
        div()
            .rounded(px(8.0))
            .border_1()
            .border_color(hsla(outline))
            .bg(hsla(CARD_BG))
            .p(px(14.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(rows)
            .child(actions)
    }

    // ------------------------------------------------------------------------------------------------ composer

    fn render_composer(&self, d: &Detail, cx: &mut Context<Self>) -> Div {
        let it = &d.item;
        let open = it.state == State::Open;
        let can_triage = self.info.as_ref().is_some_and(|i| i.can_triage());
        let can_close = can_triage || d.mine;
        if d.locked && !can_triage {
            return div().p(px(12.0)).child(faint("This conversation is locked."));
        }
        let mut buttons = div().flex().flex_wrap().justify_end().gap(px(6.0));
        if it.kind == Kind::Pull && open && !d.mine {
            buttons = buttons
                .child(button("gh-approve", "Approve", false).on_click(go(cx, GhAct::Review("APPROVE"))))
                .child(button("gh-changes", "Request changes", false).on_click(go(cx, GhAct::Review("REQUEST_CHANGES"))));
        }
        if can_close && it.state != State::Merged {
            if open {
                if it.kind == Kind::Issue {
                    buttons = buttons
                        .child(button("gh-close", "Close issue", false).on_click(go(cx, GhAct::Comment(Some(true)))))
                        .child(icon_button("gh-close-reason", IconName::ChevronDown).on_mouse_down(MouseButton::Left, popup_at(cx, PopKind::CloseReason)));
                } else {
                    buttons = buttons.child(button("gh-close", "Close pull request", false).on_click(go(cx, GhAct::Comment(Some(true)))));
                }
            } else if it.kind == Kind::Issue {
                buttons = buttons.child(button("gh-reopen", "Reopen issue", false).on_click(go(cx, GhAct::Comment(Some(false)))));
            }
        }
        buttons = buttons.child(button("gh-comment", "Comment", true).on_click(go(cx, GhAct::Comment(None))));
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(section_label("ADD A COMMENT"))
            .child(Textarea::new(&self.composer))
            .child(buttons)
    }

    // ------------------------------------------------------------------------------------------------- files

    fn render_files(&self, ds: &DetailState, cx: &mut Context<Self>) -> Div {
        if let Some(e) = &ds.files_error {
            return div().text_color(hsla(theme::DANGER)).child(e.clone());
        }
        let Some(files) = &ds.files else { return div().child(dim("Loading files…")) };
        let mut col = div().flex().flex_col().gap(px(10.0));
        col = col.child(dim(format!(
            "{} · +{} −{}",
            plural(files.len(), "file", "files"),
            files.iter().map(|f| f.additions).sum::<u32>(),
            files.iter().map(|f| f.deletions).sum::<u32>()
        )));
        for (n, f) in files.iter().enumerate() {
            let open = ds.expanded.contains(&f.path);
            let head = div()
                .id(SharedString::from(format!("gh-file-{n}")))
                .flex()
                .items_center()
                .gap(px(8.0))
                .h(px(32.0))
                .px(px(12.0))
                .bg(hsla(CARD_HEAD))
                .cursor_pointer()
                .hover(|s| s.bg(hsla(0x1c1c1d)))
                .child(div().text_color(hsla(theme::TEXT_FAINT)).child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(FONT).text_size(px(12.0)).child(f.path.clone()))
                .children((f.status != "modified").then(|| faint(f.status.clone())))
                .child(div().text_size(px(11.5)).text_color(hsla(GREEN)).child(format!("+{}", f.additions)))
                .child(div().text_size(px(11.5)).text_color(hsla(RED)).child(format!("−{}", f.deletions)))
                .on_click(go(cx, GhAct::ToggleFile(f.path.clone())));
            let mut card = div().rounded(px(8.0)).border_1().border_color(hsla(CARD_BORDER)).overflow_hidden().child(head);
            if open {
                card = card.child(self.render_diff(f, ds.full.contains(&f.path), n, cx));
            }
            col = col.child(card);
        }
        col
    }

    fn render_diff(&self, f: &FileDiff, full: bool, n: usize, cx: &mut Context<Self>) -> Div {
        const SHOWN: usize = 400;
        if f.no_patch {
            return div().p(px(12.0)).child(faint("No inline diff for this file (binary or too large). Open on GitHub to view it."));
        }
        let mut lines = div().flex().flex_col().font_family(FONT).text_size(px(11.5)).py(px(4.0));
        for l in f.lines.iter().take(if full { usize::MAX } else { SHOWN }) {
            let (bg, sign) = match l.kind {
                DiffKind::Add => (Some(GREEN), "+"),
                DiffKind::Del => (Some(RED), "-"),
                DiffKind::Hunk => (Some(0x58a6ff), ""),
                DiffKind::Ctx => (None, " "),
            };
            let num = |v: Option<u32>| div().w(px(40.0)).flex_none().text_align(TextAlign::Right).pr(px(6.0)).text_color(hsla(theme::TEXT_FAINT)).child(v.map(|v| v.to_string()).unwrap_or_default());
            lines = lines.child(
                div()
                    .flex()
                    .when_some(bg, |d, c| d.bg(hsla(c).opacity(if l.kind == DiffKind::Hunk { 0.08 } else { 0.14 })))
                    .child(num(l.old))
                    .child(num(l.new))
                    .child(div().w(px(12.0)).flex_none().text_color(hsla(theme::TEXT_DIM)).child(sign))
                    .child(div().whitespace_nowrap().child(if l.kind == DiffKind::Hunk { l.text.clone() } else if l.text.is_empty() { " ".to_string() } else { l.text.clone() })),
            );
        }
        let hidden = f.lines.len().saturating_sub(SHOWN);
        div()
            .child(div().id(SharedString::from(format!("gh-diff-{n}"))).overflow_x_scroll().child(lines))
            .when(!full && hidden > 0, |d| {
                d.child(div().p(px(8.0)).child(button(SharedString::from(format!("gh-diff-all-{n}")), format!("Show {hidden} more lines"), false).on_click(go(cx, GhAct::ShowAllLines(f.path.clone())))))
            })
    }

    // -------------------------------------------------------------------------------------------------- new item

    fn render_new(&self, kind: Kind, stacked: bool, cx: &mut Context<Self>) -> Div {
        let can_triage = self.info.as_ref().is_some_and(|i| i.can_triage());
        let mut meta = div().flex().flex_col().gap(px(8.0));
        if kind == Kind::Issue && can_triage {
            let mut labels = div().flex().flex_wrap().gap(px(5.0));
            for l in &self.draft_labels {
                labels = labels.child(label_pill(l));
            }
            let mut people = div().flex().flex_wrap().gap(px(8.0));
            for u in &self.draft_assignees {
                people = people.child(dim(u.login.clone()));
            }
            meta = meta
                .child(div().flex().items_center().gap(px(10.0)).child(div().w(px(84.0)).child(section_label("LABELS"))).child(labels).child(button("gh-new-labels", "＋ Add", false).on_mouse_down(MouseButton::Left, popup_at(cx, PopKind::Labels))))
                .child(div().flex().items_center().gap(px(10.0)).child(div().w(px(84.0)).child(section_label("ASSIGNEES"))).child(people).child(button("gh-new-assignees", "＋ Add", false).on_mouse_down(MouseButton::Left, popup_at(cx, PopKind::Assignees))));
        }
        let field = |name: &str, el: AnyElement| div().flex().flex_col().gap(px(4.0)).child(section_label(name.to_string())).child(el);
        let mut form = div().flex().flex_col().gap(px(12.0)).p(px(16.0));
        form = form.child(div().text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD).child(if kind == Kind::Issue { "New issue" } else { "New pull request" }));
        if kind == Kind::Pull {
            form = form.child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(div().flex_1().child(field("MERGE BRANCH", Input::new(&self.new_head).h(px(30.0)).into_any_element())))
                    .child(div().flex_1().child(field("INTO", Input::new(&self.new_base).h(px(30.0)).into_any_element()))),
            );
        }
        form = form
            .child(field("TITLE", Input::new(&self.new_title).h(px(32.0)).into_any_element()))
            .child(field("DESCRIPTION", Textarea::new(&self.new_body).into_any_element()))
            .child(meta)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(button("gh-new-cancel", "Cancel", false).on_click(go(cx, GhAct::Back)))
                    .child(button("gh-new-submit", if kind == Kind::Issue { "Create issue" } else { "Create pull request" }, true).on_click(go(cx, GhAct::SubmitNew))),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .when(stacked, |d| d.child(div().px(px(12.0)).pt(px(8.0)).child(button_leading("gh-back", IconName::ArrowLeft, "Back to list", false).on_click(go(cx, GhAct::Back)))))
            .child(div().id("gh-new-scroll").flex_1().min_h_0().overflow_y_scroll().child(form))
    }

    // ---------------------------------------------------------------------------------------------- pickers

    fn popup_items(&self, kind: &PopKind) -> (Vec<PopItem>, bool) {
        let q = &self.queries[if self.kind == Kind::Issue { 0 } else { 1 }];
        let detail = self.detail.as_ref().and_then(|d| d.data.as_ref());
        let mut out = vec![];
        let mut loading = false;
        let me = self.info.as_ref().map(|i| i.viewer.clone()).unwrap_or_default();
        match kind {
            PopKind::FilterAuthor | PopKind::FilterAssignee => {
                let (key, anyone) = if *kind == PopKind::FilterAuthor { ("author", "Any author") } else { ("assignee", "Anyone") };
                let current = qualifiers(q, key).into_iter().next();
                out.push(item(anyone, GhAct::SetQuery(set_qualifier(q, key, None))));
                out.push(item("@me", GhAct::SetQuery(set_qualifier(q, key, Some("@me")))));
                if key == "assignee" {
                    out.push(item("Assigned to nobody", GhAct::SetQuery(set_qualifier(&set_qualifier(q, key, None), "no", Some("assignee")))));
                }
                match &self.options.users {
                    Some(users) => {
                        for u in users {
                            let mut it = item(u.login.clone(), GhAct::SetQuery(set_qualifier(q, key, Some(&u.login))));
                            it.checked = Some(current.as_deref() == Some(u.login.as_str()));
                            out.push(it);
                        }
                    }
                    None => loading = true,
                }
            }
            PopKind::FilterLabel => {
                let have = qualifiers(q, "label");
                out.push(item("Unlabeled", GhAct::SetQuery(set_qualifier(&set_qualifier(q, "label", None), "no", Some("label")))));
                match &self.options.labels {
                    Some(labels) => {
                        for l in labels {
                            let mut it = item(l.name.clone(), GhAct::SetQuery(toggle_qualifier(q, "label", &l.name)));
                            it.swatch = Some(l.color);
                            it.checked = Some(have.iter().any(|h| h.eq_ignore_ascii_case(&l.name)));
                            out.push(it);
                        }
                    }
                    None => loading = true,
                }
            }
            PopKind::FilterMilestone => {
                let current = qualifiers(q, "milestone").into_iter().next();
                out.push(item("Any milestone", GhAct::SetQuery(set_qualifier(q, "milestone", None))));
                out.push(item("No milestone", GhAct::SetQuery(set_qualifier(&set_qualifier(q, "milestone", None), "no", Some("milestone")))));
                match &self.options.milestones {
                    Some(ms) => {
                        for (_, title) in ms {
                            let mut it = item(title.clone(), GhAct::SetQuery(set_qualifier(q, "milestone", Some(title))));
                            it.checked = Some(current.as_deref() == Some(title.as_str()));
                            out.push(it);
                        }
                    }
                    None => loading = true,
                }
            }
            PopKind::FilterSort => {
                let current = qualifiers(q, "sort").into_iter().next().unwrap_or_else(|| "created-desc".into());
                for key in ["created-desc", "created-asc", "updated-desc", "updated-asc", "comments-desc", "comments-asc"] {
                    let mut it = item(sort_name(key), GhAct::SetQuery(set_qualifier(q, "sort", (key != "created-desc").then_some(key))));
                    it.checked = Some(current == key);
                    out.push(it);
                }
            }
            PopKind::FilterReview => {
                out.push(item("Any review state", GhAct::SetQuery(set_qualifier(&set_qualifier(q, "review", None), "draft", None))));
                for (v, label) in [("none", "No reviews"), ("required", "Review required"), ("approved", "Approved"), ("changes_requested", "Changes requested")] {
                    out.push(item(label, GhAct::SetQuery(set_qualifier(q, "review", Some(v)))));
                }
                out.push(item("Drafts only", GhAct::SetQuery(set_qualifier(q, "draft", Some("true")))));
                out.push(item("Hide drafts", GhAct::SetQuery(set_qualifier(q, "draft", Some("false")))));
            }
            PopKind::FilterChecks => {
                out.push(item("Any status", GhAct::SetQuery(set_qualifier(q, "status", None))));
                for (v, label) in [("success", "Checks passing"), ("failure", "Checks failing"), ("pending", "Checks pending")] {
                    out.push(item(label, GhAct::SetQuery(set_qualifier(q, "status", Some(v)))));
                }
            }
            PopKind::Labels => {
                let have: Vec<String> = match (&self.mode, detail) {
                    (Mode::New(_), _) => self.draft_labels.iter().map(|l| l.name.clone()).collect(),
                    (_, Some(d)) => d.item.labels.iter().map(|l| l.name.clone()).collect(),
                    _ => vec![],
                };
                match &self.options.labels {
                    Some(labels) => {
                        for l in labels {
                            let mut it = item(l.name.clone(), GhAct::ToggleLabel(l.clone()));
                            it.swatch = Some(l.color);
                            it.checked = Some(have.contains(&l.name));
                            out.push(it);
                        }
                    }
                    None => loading = true,
                }
            }
            PopKind::Assignees | PopKind::Reviewers => {
                let reviewers = *kind == PopKind::Reviewers;
                let have: Vec<String> = match (&self.mode, detail) {
                    (Mode::New(_), _) => self.draft_assignees.iter().map(|u| u.login.clone()).collect(),
                    (_, Some(d)) if reviewers => d.pr.iter().flat_map(|p| p.requested.iter().cloned().chain(p.reviews.iter().map(|r| r.0.clone()))).collect(),
                    (_, Some(d)) => d.item.assignees.iter().map(|u| u.login.clone()).collect(),
                    _ => vec![],
                };
                match &self.options.users {
                    Some(users) => {
                        for u in users.iter().filter(|u| !reviewers || detail.is_none_or(|d| d.item.author != u.login)) {
                            let act = if reviewers { GhAct::ToggleReviewer(u.clone()) } else { GhAct::ToggleAssignee(u.clone()) };
                            let mut it = item(u.login.clone(), act);
                            it.checked = Some(have.contains(&u.login));
                            if u.login == me {
                                it.hint = Some("you".into());
                            }
                            out.push(it);
                        }
                    }
                    None => loading = true,
                }
            }
            PopKind::Milestone => {
                let current = detail.and_then(|d| d.item.milestone.clone());
                let mut none = item("No milestone", GhAct::SetMilestone(None));
                none.checked = Some(current.is_none());
                out.push(none);
                match &self.options.milestones {
                    Some(ms) => {
                        for m in ms {
                            let mut it = item(m.1.clone(), GhAct::SetMilestone(Some(m.clone())));
                            it.checked = Some(current.as_ref().is_some_and(|c| c.1 == m.1));
                            out.push(it);
                        }
                    }
                    None => loading = true,
                }
            }
            PopKind::Merge => {
                let info = self.info.clone().unwrap_or_default();
                for (m, label, ok) in [("MERGE", "Create a merge commit", info.allow_merge), ("SQUASH", "Squash and merge", info.allow_squash), ("REBASE", "Rebase and merge", info.allow_rebase)] {
                    if ok {
                        let mut it = item(label, GhAct::PickMergeMethod(m));
                        it.checked = Some(self.merge_method == m);
                        out.push(it);
                    }
                }
            }
            PopKind::CloseReason => {
                out.push(item("Close as completed", GhAct::Comment(Some(true))));
                out.push(item("Close as not planned", GhAct::CloseItem("NOT_PLANNED")));
            }
            PopKind::Claude => {
                let pull = self.detail.as_ref().is_some_and(|d| d.kind == Kind::Pull);
                if pull {
                    out.push(item("Review this pull request", GhAct::Claude("review")));
                    out.push(item("Address the review feedback", GhAct::Claude("address")));
                    out.push(item("Summarize it", GhAct::Claude("summarize")));
                } else {
                    out.push(item("Fix this issue", GhAct::Claude("work")));
                    out.push(item("Investigate, no changes yet", GhAct::Claude("investigate")));
                    out.push(item("Summarize the discussion", GhAct::Claude("summarize")));
                }
            }
            PopKind::More => {
                if let Some(d) = detail {
                    let it = &d.item;
                    out.push(item("Copy link", GhAct::Copy(it.url.clone())));
                    out.push(item(format!("Copy #{}", it.number), GhAct::Copy(format!("#{}", it.number))));
                    if it.kind == Kind::Pull {
                        out.push(item("Check out locally", GhAct::Checkout));
                        if it.state == State::Open && (d.mine || self.info.as_ref().is_some_and(|i| i.can_write())) {
                            out.push(if it.draft { item("Ready for review", GhAct::Ready) } else { item("Convert to draft", GhAct::ToDraft) });
                        }
                        if it.state == State::Open {
                            out.push(item("Close pull request", GhAct::CloseItem("")));
                        }
                    }
                    if self.info.as_ref().is_some_and(|i| i.can_triage()) {
                        out.push(item(if d.locked { "Unlock conversation" } else { "Lock conversation" }, GhAct::Lock(!d.locked)));
                    }
                }
            }
            PopKind::React(subject) => {
                let mine: Vec<String> = match detail {
                    Some(d) if &d.item.id == subject => d.reactions.iter().filter(|r| r.mine).map(|r| r.content.clone()).collect(),
                    Some(d) => d
                        .timeline
                        .iter()
                        .find_map(|e| match e {
                            Entry::Comment(c) if &c.id == subject => Some(c.reactions.iter().filter(|r| r.mine).map(|r| r.content.clone()).collect()),
                            _ => None,
                        })
                        .unwrap_or_default(),
                    None => vec![],
                };
                for (c, name) in [("THUMBS_UP", "+1"), ("THUMBS_DOWN", "-1"), ("LAUGH", "Laugh"), ("HOORAY", "Hooray"), ("CONFUSED", "Confused"), ("HEART", "Heart"), ("ROCKET", "Rocket"), ("EYES", "Eyes")] {
                    let has = mine.iter().any(|m| m == c);
                    let mut it = item(format!("{}  {name}", emoji(c)), GhAct::React(subject.clone(), c.to_string(), has));
                    it.checked = Some(has);
                    out.push(it);
                }
            }
            PopKind::Comment(id, can_edit, can_delete, link) => {
                out.push(item("Copy link", GhAct::Copy(link.clone())));
                if *can_edit {
                    out.push(item("Edit", GhAct::EditStart(if id == "body" { Edit::Body } else { Edit::Comment(id.clone()) })));
                }
                if *can_delete {
                    out.push(item("Delete", GhAct::DeleteComment(id.clone())));
                }
            }
        }
        (out, loading)
    }

    fn render_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let popup = self.popup.as_ref()?;
        let (items, loading) = self.popup_items(&popup.kind);
        let needle = popup.filter.read(cx).value().to_lowercase();
        let searchable = !matches!(popup.kind, PopKind::Merge | PopKind::CloseReason | PopKind::Claude | PopKind::More | PopKind::React(_) | PopKind::Comment(..) | PopKind::FilterSort | PopKind::FilterReview | PopKind::FilterChecks);
        let mut list = div().id("gh-pop-list").flex().flex_col().max_h(px(320.0)).overflow_y_scroll();
        let mut shown = 0;
        for (i, it) in items.into_iter().enumerate() {
            if searchable && !needle.is_empty() && !it.label.to_lowercase().contains(&needle) {
                continue;
            }
            shown += 1;
            list = list.child(
                div()
                    .id(SharedString::from(format!("gh-pop-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(28.0))
                    .px(px(10.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .hover(|s| s.bg(hsla(0x252525)))
                    .child(div().w(px(12.0)).flex_none().text_color(hsla(theme::ACCENT)).children((it.checked == Some(true)).then(|| icon(IconName::Check, 13.0))))
                    .children(it.swatch.map(|c| div().size(px(10.0)).flex_none().rounded_full().bg(hsla(c))))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(it.label))
                    .children(it.hint.map(faint))
                    .on_click(go(cx, it.act)),
            );
        }
        if loading {
            list = list.child(div().px(px(10.0)).py(px(6.0)).child(dim("Loading…")));
        } else if shown == 0 {
            list = list.child(div().px(px(10.0)).py(px(6.0)).child(dim("Nothing matches")));
        }
        let card = div()
            .id("gh-popup")
            .occlude()
            .w(px(272.0))
            .p(px(4.0))
            .rounded(px(8.0))
            .bg(hsla(0x181818))
            .border_1()
            .border_color(hsla(0x2c2c2c))
            .shadow_lg()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .when(searchable, |d| d.child(div().p(px(2.0)).child(Input::new(&popup.filter).h(px(28.0)).text_size(px(12.5)))))
            .child(list);
        let close = cx.listener(|v: &mut GithubView, _: &MouseDownEvent, window, cx| v.act(GhAct::ClosePopup, window, cx));
        Some(
            deferred(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, close)
                    .child(anchored().position(popup.pos).snap_to_window_with_margin(px(8.0)).child(fade_in(card, format!("gh-pop-in-{:?}", popup.kind), 110, 0.0))),
            )
            .with_priority(5)
            .into_any_element(),
        )
    }
}

fn sort_name(key: &str) -> &'static str {
    match key {
        "created-asc" => "Oldest",
        "updated-desc" => "Recently updated",
        "updated-asc" => "Least recently updated",
        "comments-desc" => "Most commented",
        "comments-asc" => "Least commented",
        _ => "Newest",
    }
}
