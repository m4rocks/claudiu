//! The GitHub pane's state and actions. Rendering is in `ui.rs`. Every `gh` call runs on a background thread; an
//! answer only lands if the view still wants it (a counter per request kind drops stale ones).
//! Children: `load` (cache and `gh` fetches), `act` (what a click does). This file has the state types and `new`.

mod act;
mod load;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use rune::TextEdit;
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::app::rt::Mailbox;

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
    /// The key of the button it opens under.
    pub anchor: String,
    pub filter: TextEdit,
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
    /// Search with what the search field holds (Return in it).
    SubmitQuery,
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
    Popup(PopKind, String),
    Claude(&'static str),
    Checkout,
    Escape,
}

pub struct GithubView {
    /// The pane's id in the workspace, so background answers find it again.
    pub id: String,
    mailbox: Mailbox<Workspace>,
    pub cwd: PathBuf,
    pub project: String,
    gh: Option<Gh>,
    pub phase: Phase,
    pub repo: Option<RepoRef>,
    pub info: Option<RepoInfo>,
    pub kind: Kind,
    pub queries: [String; 2],
    pub query_input: TextEdit,
    pub lists: [ListState; 2],
    pub detail: Option<DetailState>,
    pub mode: Mode,
    pub composer: TextEdit,
    pub edit_title: TextEdit,
    pub edit_body: TextEdit,
    pub editing: Option<Edit>,
    pub new_title: TextEdit,
    pub new_body: TextEdit,
    pub new_head: TextEdit,
    pub new_base: TextEdit,
    pub draft_labels: Vec<Label>,
    pub draft_assignees: Vec<User>,
    pub popup: Option<Popup>,
    pub options: Options,
    pub merge_method: &'static str,
    pub busy: Option<String>,
    list_seq: [u64; 2],
    detail_seq: u64,
    last_refresh: std::time::Instant,
    /// What the pane asks of the workspace, and what needs the frame (opening a link, copying); both are taken after
    /// each handler.
    pub events: Vec<GhEvent>,
    pub fx: Vec<crate::app::rt::Fx>,
}

const DEFAULT_QUERY: &str = "is:open";

fn idx(kind: Kind) -> usize {
    if kind == Kind::Issue { 0 } else { 1 }
}

/// A text field holding `value`, with the keyboard when `focused`.
pub fn field(value: &str, focused: bool) -> TextEdit {
    let mut edit = TextEdit::new(value);
    edit.set_caret(value.len());
    edit.focused = focused;
    edit
}

/// The comment box under an item.
pub fn composer_field(value: &str) -> TextEdit {
    field(value, false)
}

fn is_web(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

impl GithubView {
    pub fn new(id: String, mailbox: Mailbox<Workspace>, cwd: PathBuf, project: String, gh: Option<Gh>) -> Self {
        let mut view = Self {
            id,
            mailbox,
            cwd,
            project,
            phase: if gh.is_some() { Phase::Loading } else { Phase::NoCli },
            gh,
            repo: None,
            info: None,
            kind: Kind::Issue,
            queries: [DEFAULT_QUERY.into(), DEFAULT_QUERY.into()],
            query_input: field(DEFAULT_QUERY, false),
            lists: [ListState::default(), ListState::default()],
            detail: None,
            mode: Mode::Browse,
            composer: composer_field(""),
            edit_title: field("", false),
            edit_body: field("", false),
            editing: None,
            new_title: field("", false),
            new_body: field("", false),
            new_head: field("", false),
            new_base: field("", false),
            draft_labels: vec![],
            draft_assignees: vec![],
            popup: None,
            options: Options::default(),
            merge_method: "MERGE",
            busy: None,
            list_seq: [0; 2],
            detail_seq: 0,
            last_refresh: std::time::Instant::now(),
            events: vec![],
            fx: vec![],
        };
        view.load_cache();
        view.start_refresh_loop();
        view
    }

    /// Run `work` off the UI thread, then `done` on this view with its result (if the view is still there).
    pub(super) fn run<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&mut Self, T) + Send + 'static) {
        let (id, mailbox) = (self.id.clone(), self.mailbox.clone());
        std::thread::spawn(move || {
            let out = work();
            mailbox.send(move |ws: &mut Workspace| {
                if let Some(v) = ws.gh_view_mut(&id) {
                    done(v, out);
                }
            });
        });
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
