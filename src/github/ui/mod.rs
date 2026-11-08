//! Rendering of the GitHub pane: a list (search, filters) next to a detail view when the pane is wide, stacked
//! with a back button when it is narrow. All behaviour is in `view/`; handlers here only build a `GhAct`, which
//! runs on the pane through the workspace (`go`).
//! Children: `detail` (one item), `compose` (merge box, composer, new item), `changes` (files and diffs), `popup`
//! (filter, label and sort menus). This file has the shared pieces, the list and the header.

mod changes;
mod compose;
mod detail;
mod popup;

use popup::sort_name;

use rune::tok::motion;
use rune::{Color, Cursor, El, ScrollSpec, TextEdit, Transition, Tx, col, el, row, text, text_input};

use super::model::*;
use super::view::*;
use crate::app::Workspace;
use crate::panels::icons;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{ago, avatar, button, button_disabled, button_leading, danger_button, font, icon, icon_button, label, section_label, spinner};
use crate::platform::monospace_font;
use crate::store::now;

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

fn label_pill(l: &Label) -> El {
    row()
        .px(7.0)
        .h(18.0)
        .center_y()
        .radius(9.0)
        .border(1.0, rgb(l.color).alpha(0.5))
        .fill(rgb(l.color).alpha(0.16))
        .child(label(l.name.clone(), 11.0, lighten(l.color, 0.45)))
}

fn pill(t: impl Into<String>, color: u32) -> El {
    row().px(8.0).h(20.0).center_y().radius(10.0).fill(rgb(color)).child(text(t).font(font(11.5, 600.0)).color(rgb(0x0c0c0d)))
}

fn faint(t: impl Into<String>) -> El {
    label(t, 11.5, theme::TEXT_FAINT)
}

fn dim(t: impl Into<String>) -> El {
    label(t, 12.0, theme::TEXT_DIM)
}

fn mono(t: impl Into<String>, size: f32, color: u32) -> El {
    text(t).font(rune::TextStyle::new(crate::panels::scale::px(size), 400.0).family(rune::Family::Named(monospace_font()))).color(rgb(color))
}

fn state_style(item: &Item) -> (&'static rune::Icon, u32, &'static str) {
    match (item.kind, item.state) {
        (Kind::Issue, State::Open) => (&icons::CIRCLE_DOT, GREEN, "Open"),
        (Kind::Issue, _) if item.not_planned => (&icons::CIRCLE_SLASH, GRAY, "Not planned"),
        (Kind::Issue, _) => (&icons::CIRCLE_CHECK, PURPLE, "Closed"),
        (Kind::Pull, State::Merged) => (&icons::GIT_MERGE, PURPLE, "Merged"),
        (Kind::Pull, State::Closed) => (&icons::GIT_PULL_REQUEST_CLOSED, RED, "Closed"),
        (Kind::Pull, State::Open) if item.draft => (&icons::GIT_PULL_REQUEST_DRAFT, GRAY, "Draft"),
        (Kind::Pull, State::Open) => (&icons::GIT_PULL_REQUEST, GREEN, "Open"),
    }
}

/// Passed, failed or still running, drawn in the color `check_color` picked.
fn check_icon(color: u32) -> &'static rune::Icon {
    match color {
        GREEN => &icons::CIRCLE_CHECK,
        RED => &icons::CIRCLE_X,
        _ => &icons::CIRCLE_DOT,
    }
}

fn check_color(state: &str) -> u32 {
    match state {
        "SUCCESS" | "NEUTRAL" | "SKIPPED" => GREEN,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => RED,
        _ => YELLOW,
    }
}

/// GitHub markdown in Claudiu's style: selectable, web links open in the browser.
pub(crate) fn markdown(body: &str) -> El {
    crate::panels::markdown::markdown(body).render(&crate::panels::markdown::style())
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


/// A press that runs `act` on pane `pane`.
fn go(pane: &str, act: GhAct) -> impl Fn(&mut Workspace, &mut Tx) + 'static {
    let pane = pane.to_string();
    move |ws: &mut Workspace, tx: &mut Tx| {
        if let Some(v) = ws.gh_view_mut(&pane) {
            v.act(act.clone());
        }
        ws.after_input(tx);
    }
}

/// Key the trigger and make it open the picker `kind` under itself.
fn popup_trigger(e: El, pane: &str, key: String, kind: PopKind) -> El {
    let act = GhAct::Popup(kind, key.clone());
    e.key(key).on_press_tx(go(pane, act))
}

/// A text field of pane `pane`, bound to the field `which` picks. `multi` makes a box that grows with its text.
#[allow(clippy::too_many_arguments)]
fn gh_input(pane: &str, key: &str, edit: &TextEdit, which: fn(&mut GithubView) -> &mut TextEdit, placeholder: &str, multi: bool, submit: Option<GhAct>) -> El {
    let p = pane.to_string();
    let mut field = text_input(edit, font(13.0, 400.0))
        .key(format!("{key}-field"))
        .placeholder(placeholder.to_string(), rgb(theme::TEXT_FAINT))
        .color(rgb(theme::TEXT))
        .caret(rgb(theme::ACCENT), rgb(theme::SELECTION))
        .w_full()
        .on_edit(move |ws: &mut Workspace| ws.gh_field(&p, which));
    if !multi {
        field = field.single_line();
    }
    if let Some(act) = submit {
        field = field.on_submit(go(pane, act));
    } else if multi {
        // Return makes a new line in a comment; the buttons post it.
        field = field.on_submit(|ws: &mut Workspace, tx: &mut Tx| {
            ws.after_input(tx);
        });
    }
    let boxed = if multi { rune::auto_grow(field, 4, 14) } else { field };
    let mut b = col()
        .key(key.to_string())
        .w_full()
        .px(9.0)
        .py(if multi { 7.0 } else { 0.0 })
        .radius(6.0)
        .fill(rgb(0x101012))
        .border(1.0, rgb(if edit.focused { theme::ACCENT } else { 0x2a2a2e }))
        .motion(motion::SNAP)
        .child(boxed);
    if !multi {
        b = b.h(30.0).justify_center();
    }
    b
}

struct PopItem {
    label: String,
    swatch: Option<u32>,
    checked: Option<bool>,
    hint: Option<String>,
    act: GhAct,
}

fn item(l: impl Into<String>, act: GhAct) -> PopItem {
    PopItem { label: l.into(), swatch: None, checked: None, hint: None, act }
}

/// Grey bars where the description and the history will be, pulsing until they arrive.
fn skeleton() -> El {
    let bar = |w: f32, h: f32| el().h(h).w_frac(w).radius(4.0).fill(rgb(0x1f1f21));
    let card = |lines: &[f32]| {
        let mut body = col().gap(8.0).padding(12.0).w_full();
        for w in lines {
            body = body.child(bar(*w, 10.0));
        }
        col().w_full().radius(8.0).border(1.0, rgb(CARD_BORDER)).fill(rgb(CARD_BG)).child(row().h(36.0).w_full().px(12.0).center_y().child(bar(0.3, 10.0))).child(body)
    };
    col()
        .key("gh-skeleton")
        .w_full()
        .gap(12.0)
        .looping(rune::LoopSpec::opacity(1.2, 0.0, 0.45, 1.0))
        .child(card(&[0.95, 0.8, 0.88, 0.5]))
        .child(card(&[0.7, 0.4]))
        .child(card(&[0.85, 0.6, 0.3]))
}

impl GithubView {
    /// The pane at `width` points.
    pub fn render(&self, width: f32) -> El {
        let body = match self.phase.clone() {
            Phase::Ready => self.render_ready(width),
            // Cached lists stand in while the repository is looked up.
            Phase::Loading if self.lists.iter().any(|l| l.loaded) => self.render_ready(width),
            other => self.render_state(other),
        };
        let pane = self.id.clone();
        let mut root = col()
            .key(format!("github-{}", self.id))
            .w_full()
            .h_full()
            .fill(rgb(theme::PANEL_BG))
            .on_key(move |ws: &mut Workspace, k: &rune::KeyEvent, tx: &mut Tx| {
                if k.key != rune::Key::Escape {
                    return false;
                }
                go(&pane, GhAct::Escape)(ws, tx);
                true
            })
            .child(body);
        if let Some(p) = self.render_popup() {
            root = root.child(p);
        }
        root
    }

    // --------------------------------------------------------------------------------------- not-ready states

    fn render_state(&self, phase: Phase) -> El {
        let (title, body, actions): (&str, String, Vec<(&str, GhAct, bool)>) = match phase {
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
            Phase::NotGithub => ("No GitHub repository here", format!("{} has no remote that points to GitHub.", self.project), vec![("Try again", GhAct::Retry, false)]),
            Phase::Failed(msg) => ("Could not load GitHub", msg, vec![("Try again", GhAct::Retry, true)]),
            Phase::Ready => return el(),
        };
        let mut r = row().mt(18.0).gap(8.0);
        for (i, (l, act, primary)) in actions.into_iter().enumerate() {
            r = r.child(button(l, primary).key(format!("gh-state-{i}")).on_press_tx(go(&self.id, act)));
        }
        let mut c = col()
            .key(format!("gh-state-{title}"))
            .w_full()
            .h_full()
            .center()
            .px(32.0)
            .transition(Transition::opacity())
            .child(text(title).font(font(16.0, 600.0)).color(rgb(theme::TEXT)))
            .child(label(body, 13.0, theme::TEXT_DIM).wrap().max_w(460.0).mt(8.0).align(rune::Align::Center));
        if title.starts_with("Connecting") {
            c = c.child(spinner(16.0, theme::TEXT_FAINT).mt(14.0));
        }
        c.child(r)
    }

    // ------------------------------------------------------------------------------------------------ layout

    fn render_ready(&self, width: f32) -> El {
        let wide = width >= WIDE;
        let showing_detail = self.detail.is_some() || matches!(self.mode, Mode::New(_));
        let content = if wide {
            row()
                .grow(1.0)
                .shrink()
                .min_h(0.0)
                .w_full()
                .child(self.render_list(true))
                .child(col().key(self.detail_key()).grow(1.0).shrink().min_w(0.0).h_full().transition(Transition::opacity()).child(self.render_detail(false)))
        } else if showing_detail {
            col().key(self.detail_key()).grow(1.0).shrink().min_h(0.0).w_full().transition(Transition::opacity().combined(Transition::offset(12.0, 0.0))).child(self.render_detail(true))
        } else {
            row().key("gh-list-only").grow(1.0).shrink().min_h(0.0).w_full().transition(Transition::opacity()).child(self.render_list(false))
        };
        col().w_full().h_full().child(self.render_header()).child(content)
    }

    fn render_header(&self) -> El {
        let repo = self.repo.as_ref().map(|r| r.full()).unwrap_or_default();
        let private = self.info.as_ref().is_some_and(|i| i.private);
        let tab = |kind: Kind, name: &str| {
            let on = self.kind == kind;
            let l = &self.lists[if kind == Kind::Issue { 0 } else { 1 }];
            let mut t = row()
                .key(format!("gh-tab-{name}"))
                .h(26.0)
                .px(10.0)
                .gap(6.0)
                .center_y()
                .radius(6.0)
                .cursor(Cursor::Pointer)
                .role("tab")
                .hover(Color::WHITE.alpha(0.04))
                .motion(motion::SNAP)
                .child(text(name.to_string()).font(font(12.5, if on { 600.0 } else { 400.0 })).color(rgb(if on { theme::TEXT } else { theme::TEXT_DIM })));
            if on {
                t = t.fill(rgb(theme::ROW_ACTIVE));
            }
            if l.loaded {
                t = t.child(label(l.open.to_string(), 11.0, theme::TEXT_FAINT));
            }
            t.on_press_tx(go(&self.id, GhAct::SetKind(kind)))
        };
        let url = self.repo.as_ref().map(|r| r.url.clone()).unwrap_or_default();
        let loading = self.lists.iter().any(|l| l.loading);
        let new_kind = self.kind;
        let mut left = row().center_y().gap(10.0).shrink().min_w(0.0);
        if private {
            left = left.child(icon(&icons::LOCK, 13.0, theme::TEXT_DIM));
        }
        left = left.child(text(repo).font(font(13.0, 600.0)).color(rgb(theme::TEXT)).truncate().shrink()).child(tab(Kind::Issue, "Issues")).child(tab(Kind::Pull, "Pull requests"));
        let mut right = row().center_y().gap(6.0);
        if let Some(b) = self.busy.clone() {
            right = right.child(row().key("gh-busy").gap(6.0).center_y().transition(Transition::opacity()).child(spinner(12.0, theme::TEXT_DIM)).child(dim(b)));
        }
        let refresh = if loading { icon_button(&icons::REFRESH_CW).looping(rune::LoopSpec::rotation(1.0)) } else { icon_button(&icons::REFRESH_CW) };
        right = right
            .child(refresh.key("gh-refresh").help("Refresh").on_press_tx(go(&self.id, GhAct::Refresh)))
            .child(icon_button(&icons::EXTERNAL_LINK).key("gh-web").help("Open on GitHub").on_press_tx(go(&self.id, GhAct::OpenWeb(url))))
            .child(button_leading(&icons::PLUS, if new_kind == Kind::Issue { "New issue" } else { "New pull request" }, true).key("gh-new").on_press_tx(go(&self.id, GhAct::New(new_kind))));
        row()
            .w_full()
            .h(42.0)
            .px(12.0)
            .gap(8.0)
            .center_y()
            .justify_between()
            .border_sides([0.0, 0.0, 1.0, 0.0], rgb(theme::BORDER))
            .child(left)
            .child(right)
    }

    // ---------------------------------------------------------------------------------------------------- list

    fn chip(&self, id: &str, text_: String, active: bool, kind: PopKind) -> El {
        let c = row()
            .h(24.0)
            .px(9.0)
            .gap(4.0)
            .center_y()
            .radius(12.0)
            .cursor(Cursor::Pointer)
            .role("button")
            .fill(rgb(if active { 0x2a211d } else { 0x161616 }))
            .hover(Color::WHITE.alpha(0.05))
            .press_scale(0.97)
            .child(label(text_, 12.0, if active { theme::ACCENT } else { theme::TEXT_DIM }))
            .child(icon(&icons::CHEVRON_DOWN, 12.0, if active { theme::ACCENT } else { theme::TEXT_DIM }));
        popup_trigger(c, &self.id, format!("gh-chip-{id}"), kind)
    }

    fn render_list(&self, wide: bool) -> El {
        let i = if self.kind == Kind::Issue { 0 } else { 1 };
        let q = &self.queries[i];
        let list = &self.lists[i];
        let state = state_of(q);
        let state_tab = |id: &str, glyph: &'static rune::Icon, name: &str, count: usize, key: &'static str| {
            let on = state.as_deref() == Some(key);
            let color = if on { theme::TEXT } else { theme::TEXT_DIM };
            row()
                .key(format!("gh-state-tab-{id}"))
                .gap(5.0)
                .center_y()
                .cursor(Cursor::Pointer)
                .role("tab")
                .child(icon(glyph, 14.0, color))
                .child(text(format!("{name} {count}")).font(font(12.5, if on { 600.0 } else { 400.0 })).color(rgb(color)))
                .on_press_tx(go(&self.id, GhAct::SetState(Some(key))))
        };
        let open_glyph = if self.kind == Kind::Issue { &icons::CIRCLE_DOT } else { &icons::GIT_PULL_REQUEST };
        let closed_glyph = if self.kind == Kind::Issue { &icons::CIRCLE_CHECK } else { &icons::GIT_MERGE };

        let mut chips = row().w_full().flex_wrap().gap(6.0);
        let one = |key: &str| qualifiers(q, key).into_iter().next();
        let author = one("author");
        chips = chips.child(self.chip("author", author.clone().map_or("Author".into(), |a| format!("Author: {a}")), author.is_some(), PopKind::FilterAuthor));
        let labels = qualifiers(q, "label");
        let label_text = match labels.len() {
            0 => "Label".to_string(),
            1 => format!("Label: {}", labels[0]),
            n => format!("Labels: {n}"),
        };
        chips = chips.child(self.chip("label", label_text, !labels.is_empty() || one("no").as_deref() == Some("label"), PopKind::FilterLabel));
        let assignee = one("assignee");
        chips = chips.child(self.chip("assignee", assignee.clone().map_or("Assignee".into(), |a| format!("Assignee: {a}")), assignee.is_some(), PopKind::FilterAssignee));
        let milestone = one("milestone");
        chips = chips.child(self.chip("milestone", milestone.clone().map_or("Milestone".into(), |a| format!("Milestone: {a}")), milestone.is_some(), PopKind::FilterMilestone));
        if self.kind == Kind::Pull {
            let review = one("review");
            chips = chips.child(self.chip("review", review.clone().map_or("Review".into(), |a| format!("Review: {a}")), review.is_some() || one("draft").is_some(), PopKind::FilterReview));
            let checks = one("status");
            chips = chips.child(self.chip("checks", checks.clone().map_or("Checks".into(), |a| format!("Checks: {a}")), checks.is_some(), PopKind::FilterChecks));
        }
        let sort = one("sort");
        chips = chips.child(self.chip("sort", sort.clone().map_or("Newest".into(), |s| sort_name(&s).to_string()), sort.is_some(), PopKind::FilterSort));
        let default_state = state_of(q).unwrap_or_default();
        let filtered = q.split_whitespace().count() > usize::from(!default_state.is_empty());
        if filtered {
            let reset = format!("is:{}", if default_state.is_empty() { "open" } else { &default_state });
            chips = chips.child(
                row()
                    .key("gh-clear")
                    .h(24.0)
                    .px(6.0)
                    .center_y()
                    .cursor(Cursor::Pointer)
                    .role("button")
                    .transition(Transition::opacity())
                    .child(label("Clear", 12.0, theme::TEXT_FAINT))
                    .on_press_tx(go(&self.id, GhAct::SetQuery(reset))),
            );
        }

        let search = row()
            .w_full()
            .gap(6.0)
            .center_y()
            .child(icon(&icons::SEARCH, 14.0, theme::TEXT_FAINT))
            .child(col().grow(1.0).shrink().child(gh_input(&self.id, "gh-query", &self.query_input, |v| &mut v.query_input, "Search, e.g. is:open label:bug author:@me", false, Some(GhAct::SubmitQuery))));
        let toolbar = col()
            .w_full()
            .gap(8.0)
            .padding(12.0)
            .border_sides([0.0, 0.0, 1.0, 0.0], rgb(theme::BORDER))
            .child(search)
            .child(row().gap(14.0).child(state_tab("open", open_glyph, "Open", list.open, "open")).child(state_tab("closed", closed_glyph, "Closed", list.closed, "closed")))
            .child(chips);

        let t = now();
        let mut rows = col().w_full();
        for it in &list.items {
            rows = rows.child(self.render_row(it, t));
        }
        if list.items.is_empty() {
            let msg = if let Some(e) = &list.error {
                label(e.clone(), 12.0, theme::DANGER).wrap()
            } else if list.loading || !list.loaded {
                row().gap(8.0).center_y().child(spinner(12.0, theme::TEXT_DIM)).child(dim("Loading…"))
            } else if self.kind == Kind::Issue && self.info.as_ref().is_some_and(|i| !i.issues_enabled) {
                dim("Issues are turned off for this repository.")
            } else {
                dim(format!("No {} match this search.", if self.kind == Kind::Issue { "issues" } else { "pull requests" }))
            };
            rows = rows.child(el().padding(18.0).child(msg));
        }
        if list.next.is_some() {
            rows = rows.child(
                el().padding(12.0).child(
                    button(if list.loading { "Loading…".to_string() } else { format!("Load more ({} of {})", list.items.len(), list.total) }, false)
                        .key("gh-more")
                        .on_press_tx(go(&self.id, GhAct::LoadMore)),
                ),
            );
        } else if !list.items.is_empty() {
            rows = rows.child(el().padding(12.0).child(faint(plural(list.total, "result", "results"))));
        }
        let mut c = col().h_full().min_h(0.0).child(toolbar).child(col().key("gh-list").w_full().grow(1.0).shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(rows));
        c = if wide { c.w(LIST_W).border_sides([0.0, 1.0, 0.0, 0.0], rgb(theme::BORDER)) } else { c.grow(1.0).shrink().min_w(0.0) };
        c
    }

    fn render_row(&self, it: &Item, t: i64) -> El {
        let selected = self.detail.as_ref().is_some_and(|d| d.number == it.number && d.kind == it.kind);
        let (glyph, color, _) = state_style(it);
        let mut sub = row().w_full().flex_wrap().center_y().gap(6.0).child(faint(format!(
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
        let mut right = row().center_y().gap(8.0);
        if it.kind == Kind::Pull {
            if let Some(c) = &it.checks {
                right = right.child(icon(check_icon(check_color(c)), 14.0, check_color(c)));
            }
            match it.review.as_deref() {
                Some("APPROVED") => right = right.child(label("approved", 11.5, GREEN)),
                Some("CHANGES_REQUESTED") => right = right.child(label("changes", 11.5, RED)),
                _ => {}
            }
        }
        if it.comments > 0 {
            right = right.child(row().center_y().gap(3.0).child(icon(&icons::MESSAGE_SQUARE, 13.0, theme::TEXT_FAINT)).child(label(it.comments.to_string(), 11.5, theme::TEXT_FAINT)));
        }
        let mut r = row()
            .key(format!("gh-row-{:?}-{}", it.kind, it.number))
            .w_full()
            .gap(10.0)
            .px(14.0)
            .py(9.0)
            .border_sides([0.0, 0.0, 1.0, 0.0], rgb(theme::BORDER))
            .cursor(Cursor::Pointer)
            .role("button")
            .hover(Color::WHITE.alpha(0.03))
            .motion(motion::REFLOW)
            .child(el().w(14.0).pt(1.0).child(icon(glyph, 14.0, color)))
            .child(col().grow(1.0).shrink().min_w(0.0).gap(4.0).child(text(if it.draft { format!("[Draft] {}", it.title) } else { it.title.clone() }).font(font(13.0, 500.0)).color(rgb(theme::TEXT)).truncate()).child(sub))
            .child(right)
            .on_press_tx(go(&self.id, GhAct::Open(it.kind, it.number)));
        if selected {
            r = r.fill(rgb(theme::ROW_ACTIVE));
        }
        r
    }

    // -------------------------------------------------------------------------------------------------- detail

    fn render_empty_detail(&self) -> El {
        col()
            .w_full()
            .h_full()
            .center()
            .gap(6.0)
            .child(label("Select an issue or pull request", 14.0, theme::TEXT_DIM))
            .child(faint("Search with GitHub syntax, e.g. is:open label:bug author:@me"))
    }
}
