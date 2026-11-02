//! The GitHub pane's state and actions. Rendering is in `ui.rs`. Every `gh` call runs on a background thread; an
//! answer only lands if the view still wants it (a counter per request kind drops stale ones).
//! Children: `load` (cache and `gh` fetches), `act` (what a click does). This file has the state types and `new`.

mod act;
mod load;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels, Point, SharedString, Subscription, Window};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use serde_json::{Value, json};

use super::cli::{self, ErrKind, Gh, RepoRef};
use super::model::*;

/// What the pane asks the workspace to do; the workspace owns toasts, sessions and the terminal.
pub enum GhEvent {
    Toast { text: String, error: bool },
    /// Start a Claude session next to this pane with this prompt.
    AskClaude(String),
    /// `gh pr checkout <n>` in the project folder.
    Checkout(u32),
    /// Open a terminal running `gh auth login`.
    SignIn,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Loading,
    NoCli,
    SignedOut,
    NotGithub,
    Failed(String),
    Ready,
}

#[derive(Default)]
pub struct ListState {
    pub items: Vec<Item>,
    pub total: usize,
    pub open: usize,
    pub closed: usize,
    pub next: Option<String>,
    pub loading: bool,
    pub loaded: bool,
    pub error: Option<String>,
}

/// What survives a restart: the repository, its permissions and the first page of each list.
#[derive(serde::Serialize, serde::Deserialize)]
struct CacheFile {
    repo: Option<RepoRef>,
    info: Option<RepoInfo>,
    lists: [Cached; 2],
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Cached {
    items: Vec<Item>,
    total: usize,
    open: usize,
    closed: usize,
    next: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailTab {
    Conversation,
    Commits,
    Files,
}

pub struct DetailState {
    pub kind: Kind,
    pub number: u32,
    pub data: Option<Detail>,
    pub loading: bool,
    pub more_loading: bool,
    pub error: Option<String>,
    pub tab: DetailTab,
    pub files: Option<Vec<FileDiff>>,
    pub files_loading: bool,
    pub files_error: Option<String>,
    pub expanded: HashSet<String>,
    pub full: HashSet<String>,
    /// `data` is only what the list row knew (title, labels, ...); the description and history are still on their way.
    pub partial: bool,
}

impl DetailState {
    pub fn new(kind: Kind, number: u32) -> Self {
        Self {
            kind,
            number,
            data: None,
            loading: true,
            more_loading: false,
            error: None,
            tab: DetailTab::Conversation,
            files: None,
            files_loading: false,
            files_error: None,
            expanded: HashSet::new(),
            full: HashSet::new(),
            partial: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Title,
    Body,
    Comment(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Browse,
    New(Kind),
}

#[derive(Clone, Debug, PartialEq)]
pub enum PopKind {
    FilterAuthor,
    FilterLabel,
    FilterAssignee,
    FilterMilestone,
    FilterSort,
    FilterReview,
    FilterChecks,
    Labels,
    Assignees,
    Milestone,
    Reviewers,
    Merge,
    CloseReason,
    Claude,
    More,
    React(String),
    /// Comment id, whether the viewer may edit / delete it, and its permalink text.
    Comment(String, bool, bool, String),
}

pub struct Popup {
    pub kind: PopKind,
    pub pos: Point<Pixels>,
    pub filter: Entity<InputState>,
    pub scroll: crate::panels::glide::Smooth,
    _sub: Subscription,
}

#[derive(Default)]
pub struct Options {
    pub labels: Option<Vec<Label>>,
    pub users: Option<Vec<User>>,
    pub milestones: Option<Vec<(String, String)>>,
}

/// Everything a user can do in the pane; buttons, menus and keys all go through `GithubView::act`.
#[derive(Clone, Debug)]
pub enum GhAct {
    Refresh,
    Retry,
    SignIn,
    SetKind(Kind),
    SetQuery(String),
    SetState(Option<&'static str>),
    Open(Kind, u32),
    Back,
    LoadMore,
    LoadMoreTimeline,
    Tab(DetailTab),
    OpenWeb(String),
    Copy(String),
    New(Kind),
    SubmitNew,
    /// Post the composer text, optionally closing (`Some(true)`) or reopening (`Some(false)`) the item.
    Comment(Option<bool>),
    CloseItem(&'static str),
    ReopenItem,
    Review(&'static str),
    ToggleLabel(Label),
    ToggleAssignee(User),
    ToggleReviewer(User),
    SetMilestone(Option<(String, String)>),
    Merge(&'static str),
    PickMergeMethod(&'static str),
    AutoMerge(bool),
    Ready,
    ToDraft,
    UpdateBranch,
    DeleteBranch,
    React(String, String, bool),
    EditStart(Edit),
    EditSave,
    EditCancel,
    DeleteComment(String),
    Lock(bool),
    ToggleFile(String),
    ShowAllLines(String),
    Popup(PopKind, Point<Pixels>),
    ClosePopup,
    Claude(&'static str),
    Checkout,
    Escape,
}

pub struct GithubView {
    pub cwd: PathBuf,
    pub project: String,
    gh: Option<Gh>,
    pub phase: Phase,
    pub repo: Option<RepoRef>,
    pub info: Option<RepoInfo>,
    pub kind: Kind,
    pub queries: [String; 2],
    pub query_input: Entity<InputState>,
    pub lists: [ListState; 2],
    pub detail: Option<DetailState>,
    pub mode: Mode,
    pub composer: Entity<TextareaState>,
    pub edit_title: Entity<InputState>,
    pub edit_body: Entity<TextareaState>,
    pub editing: Option<Edit>,
    pub new_title: Entity<InputState>,
    pub new_body: Entity<TextareaState>,
    pub new_head: Entity<InputState>,
    pub new_base: Entity<InputState>,
    pub draft_labels: Vec<Label>,
    pub draft_assignees: Vec<User>,
    pub popup: Option<Popup>,
    pub options: Options,
    pub merge_method: &'static str,
    pub busy: Option<String>,
    /// Width of the pane in pixels, set by the workspace each frame; decides side-by-side or stacked.
    pub width: f32,
    pub focus: FocusHandle,
    list_seq: [u64; 2],
    detail_seq: u64,
    last_refresh: std::time::Instant,
    /// Smooth wheel scrolling of the list, the detail and the new-item form.
    pub scrolls: [crate::panels::glide::Smooth; 3],
    _subs: Vec<Subscription>,
}

impl EventEmitter<GhEvent> for GithubView {}

impl Focusable for GithubView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

const DEFAULT_QUERY: &str = "is:open";

fn idx(kind: Kind) -> usize {
    if kind == Kind::Issue { 0 } else { 1 }
}

fn is_web(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

impl GithubView {
    pub fn new(cwd: PathBuf, project: String, gh: Option<Gh>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search, e.g. is:open label:bug author:@me"));
        let mut subs = vec![];
        subs.push(cx.subscribe_in(&query_input, window, |v: &mut Self, input, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                let q = input.read(cx).value().to_string();
                v.act(GhAct::SetQuery(q), window, cx);
            }
        }));
        let composer = cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 14).placeholder("Leave a comment (Markdown)"));
        let edit_body = cx.new(|cx| TextareaState::new(window, cx).auto_grow(4, 16));
        let new_body = cx.new(|cx| TextareaState::new(window, cx).auto_grow(8, 18).placeholder("Describe it (Markdown)"));
        let edit_title = cx.new(|cx| InputState::new(window, cx));
        let new_title = cx.new(|cx| InputState::new(window, cx).placeholder("Title"));
        let new_head = cx.new(|cx| InputState::new(window, cx).placeholder("Branch with your changes"));
        let new_base = cx.new(|cx| InputState::new(window, cx).placeholder("Branch to merge into"));
        let focus = cx.focus_handle();

        let mut view = Self {
            cwd,
            project,
            phase: if gh.is_some() { Phase::Loading } else { Phase::NoCli },
            gh,
            repo: None,
            info: None,
            kind: Kind::Issue,
            queries: [DEFAULT_QUERY.into(), DEFAULT_QUERY.into()],
            query_input,
            lists: [ListState::default(), ListState::default()],
            detail: None,
            mode: Mode::Browse,
            composer,
            edit_title,
            edit_body,
            editing: None,
            new_title,
            new_body,
            new_head,
            new_base,
            draft_labels: vec![],
            draft_assignees: vec![],
            popup: None,
            options: Options::default(),
            merge_method: "MERGE",
            busy: None,
            width: 900.0,
            focus,
            list_seq: [0; 2],
            detail_seq: 0,
            last_refresh: std::time::Instant::now(),
            scrolls: Default::default(),
            _subs: subs,
        };
        // Coming back to the pane refreshes it (at most every 5 seconds, and not while the user is mid-edit).
        let focus_sub = cx.on_focus_in(&view.focus, window, |v: &mut Self, window, cx| v.refresh_on_return(window, cx));
        view._subs.push(focus_sub);
        view.query_input.update(cx, |s, cx| s.set_value(DEFAULT_QUERY, window, cx));
        view.load_cache(window, cx);
        view.start_refresh_loop(window, cx);
        view
    }

    /// Changes whenever the detail side shows something else, so it fades in again.
    pub fn detail_key(&self) -> String {
        match (&self.mode, &self.detail) {
            (Mode::New(k), _) => format!("gh-new-{k:?}"),
            (_, Some(d)) => format!("gh-detail-{:?}-{}", d.kind, d.number),
            _ => "gh-detail-none".into(),
        }
    }

    // ------------------------------------------------------------------------------------------ cache
}

/// The prompt for a Claude session about the open item. Short on purpose: Claude reads the details itself with `gh`.
pub fn claude_prompt(which: &str, item: &Item) -> String {
    let (n, title, url) = (item.number, &item.title, &item.url);
    match (which, item.kind) {
        ("investigate", _) => format!("Look into GitHub issue #{n} “{title}” ({url}). Read it with `gh issue view {n} --json title,body,comments`, find the cause in this repository and explain it. Do not change any files yet."),
        ("review", _) => format!("Review GitHub pull request #{n} “{title}” ({url}). Read it with `gh pr view {n} --json title,body,comments` and `gh pr diff {n}`. Report bugs, risks and missing tests, most serious first. Do not push anything."),
        ("address", _) => format!("Address the review feedback on GitHub pull request #{n} “{title}” ({url}). Read it with `gh pr view {n} --json title,body,comments` and check out the branch with `gh pr checkout {n}`. Make the changes locally, but ask before pushing."),
        ("summarize", Kind::Pull) => format!("Summarize GitHub pull request #{n} “{title}” ({url}) for me: what it changes, why, and what to check before merging. Use `gh pr view {n} --json title,body,comments` and `gh pr diff {n}`."),
        ("summarize", Kind::Issue) => format!("Summarize GitHub issue #{n} “{title}” ({url}) and its discussion for me. Use `gh issue view {n} --json title,body,comments`."),
        (_, Kind::Pull) => format!("Work on GitHub pull request #{n} “{title}” ({url}). Read it with `gh pr view {n} --json title,body,comments`."),
        _ => format!("Work on GitHub issue #{n} “{title}” ({url}). Read it first with `gh issue view {n} --json title,body,comments`, then propose a short plan and implement it in this repository. Ask me if anything is unclear."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_name_the_item_and_stay_short() {
        let item = Item {
            kind: Kind::Pull,
            id: String::new(),
            number: 42,
            title: "Fix it".into(),
            state: State::Open,
            not_planned: false,
            draft: false,
            url: "https://github.com/a/b/pull/42".into(),
            author: "x".into(),
            created: 0,
            updated: 0,
            comments: 0,
            labels: vec![],
            assignees: vec![],
            milestone: None,
            review: None,
            checks: None,
            head: String::new(),
            base: String::new(),
        };
        let p = claude_prompt("review", &item);
        assert!(p.contains("#42") && p.contains("gh pr diff 42") && p.len() < 500);
        assert!(claude_prompt("work", &item).contains("pull request #42"));
    }
}
