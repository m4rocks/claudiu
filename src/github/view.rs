//! The GitHub pane's state and actions. Rendering is in `ui.rs`. Every `gh` call runs on a background thread; an
//! answer only lands if the view still wants it (a counter per request kind drops stale ones).

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
    pub scrolls: [crate::glide::Smooth; 3],
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

    fn cache_file(&self) -> PathBuf {
        crate::datadir::data_dir().join("github-cache").join(format!("{}.json", crate::datadir::safe_file_stem(&self.cwd.to_string_lossy())))
    }

    /// Fill the lists from the last run so the pane is populated before `gh` answers; only the default query is cached.
    /// The file is read off the UI thread, and then the lookup starts, so a cached repository goes first.
    fn load_cache(&self, window: &mut Window, cx: &mut Context<Self>) {
        let file = self.cache_file();
        cx.spawn_in(window, async move |this, cx| {
            let cached = cx.background_executor().spawn(async move { std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str::<CacheFile>(&t).ok()) }).await;
            let _ = this.update_in(cx, |v, window, cx| {
                if let Some(file) = cached {
                    // A known repository lets clicks work and the lists refresh before `gh repo view` answers.
                    (v.repo, v.info) = (file.repo, file.info);
                    for (list, c) in v.lists.iter_mut().zip(file.lists) {
                        (list.items, list.total, list.open, list.closed, list.next, list.loaded) = (c.items, c.total, c.open, c.closed, c.next, true);
                    }
                }
                v.connect(window, cx);
            });
        })
        .detach();
    }

    /// Raw first page of one item's detail, so reopening it shows at once. The dot keeps it apart from the repository's
    /// file (`safe_file_stem` never produces a dot).
    fn detail_file(&self, number: u32) -> PathBuf {
        let dir = crate::datadir::data_dir().join("github-cache");
        dir.join(format!("{}.{number}.json", crate::datadir::safe_file_stem(&self.cwd.to_string_lossy())))
    }

    /// Write the first page of each list after a fresh load (skipped while a search is active). Written off the UI thread.
    fn save_cache(&self, cx: &mut Context<Self>) {
        if self.queries.iter().any(|q| q != DEFAULT_QUERY) {
            return;
        }
        let one = |l: &ListState| Cached { items: l.items.clone(), total: l.total, open: l.open, closed: l.closed, next: l.next.clone() };
        let file = CacheFile { repo: self.repo.clone(), info: self.info.clone(), lists: [one(&self.lists[0]), one(&self.lists[1])] };
        let path = self.cache_file();
        cx.background_executor()
            .spawn(async move {
                if let Ok(text) = serde_json::to_string(&file) {
                    let _ = crate::datadir::write_atomic(&path, &text);
                }
            })
            .detach();
    }

    // ------------------------------------------------------------------------------------------ background work

    /// Run `job` off the UI thread with this view's `gh`, then `done` on it. A view without `gh` never gets here.
    fn spawn<T: Send + 'static>(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        job: impl FnOnce(Gh, Option<RepoRef>) -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(gh) = self.gh.clone() else { return };
        let repo = self.repo.clone();
        let gh = match (&repo, gh) {
            (Some(r), mut g) => {
                g.host = r.host.clone();
                g
            }
            (None, g) => g,
        };
        cx.spawn_in(window, async move |this, cx| {
            let out = cx.background_executor().spawn(async move { job(gh, repo) }).await;
            let _ = this.update_in(cx, |v, window, cx| done(v, out, window, cx));
        })
        .detach();
    }

    /// Find the repository, its permissions, then load both lists.
    pub fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.gh.is_none() {
            self.phase = Phase::NoCli;
            cx.notify();
            return;
        }
        // With a cached repository the pane is usable at once and the lists load alongside the lookup.
        let cached = self.repo.clone();
        if cached.is_some() {
            self.phase = Phase::Ready;
            self.load_list(Kind::Issue, false, window, cx);
            self.load_list(Kind::Pull, false, window, cx);
        } else {
            self.phase = Phase::Loading;
        }
        cx.notify();
        self.spawn(
            window,
            cx,
            |gh, _| -> Result<(RepoRef, RepoInfo, Gh), cli::Error> {
                let repo = gh.repo()?;
                let mut gh = gh;
                gh.host = repo.host.clone();
                let data = gh.graphql(cli::REPO_QUERY, json!({"owner": repo.owner, "name": repo.name}))?;
                let info = parse_repo(&data).ok_or_else(|| cli::Error { kind: ErrKind::Other, msg: "repository not found (is the GitHub app or token allowed to see it?)".into() })?;
                Ok((repo, info, gh))
            },
            move |v, result, window, cx| {
                match result {
                    Ok((repo, info, gh)) => {
                        v.gh = Some(gh);
                        v.repo = Some(repo);
                        v.info = Some(info);
                        v.phase = Phase::Ready;
                        if cached.as_ref() != v.repo.as_ref() {
                            v.load_list(Kind::Issue, false, window, cx);
                            v.load_list(Kind::Pull, false, window, cx);
                        }
                    }
                    Err(e) => {
                        v.phase = match e.kind {
                            ErrKind::NoCli => Phase::NoCli,
                            ErrKind::SignedOut => Phase::SignedOut,
                            ErrKind::NotGithub => Phase::NotGithub,
                            ErrKind::Other => Phase::Failed(e.msg),
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    /// Reload what is on screen every 90 seconds so the pane follows what teammates do. Skipped while busy or editing.
    fn start_refresh_loop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(90)).await;
                let alive = this.update_in(cx, |v, window, cx| {
                    if v.is_idle() {
                        v.reload_visible(window, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn reload_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load_list(self.kind, false, window, cx);
        let other = if self.kind == Kind::Issue { Kind::Pull } else { Kind::Issue };
        self.load_list(other, false, window, cx);
        self.reload_detail(window, cx);
    }

    /// Called when the user comes back to the pane or the window: refresh unless it just did or the user is mid-action.
    pub fn refresh_on_return(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.last_refresh.elapsed() > Duration::from_secs(5) && self.is_idle() {
            self.reload_visible(window, cx);
        }
    }

    fn is_idle(&self) -> bool {
        self.busy.is_none() && self.editing.is_none() && self.popup.is_none() && self.phase == Phase::Ready
    }

    /// The 90-second refresh: only what is on screen; the other list reloads when its tab is chosen.
    fn reload_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let other = if self.kind == Kind::Issue { Kind::Pull } else { Kind::Issue };
        self.last_refresh = std::time::Instant::now();
        self.lists[idx(other)].loaded = false;
        self.load_list(self.kind, false, window, cx);
        self.reload_detail(window, cx);
    }

    pub fn load_list(&mut self, kind: Kind, append: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let i = idx(kind);
        let after = if append { self.lists[i].next.clone() } else { None };
        if append && after.is_none() {
            return;
        }
        self.list_seq[i] += 1;
        let seq = self.list_seq[i];
        let list = &mut self.lists[i];
        list.loading = true;
        list.error = None;
        let q = self.queries[i].clone();
        let (full, qa, qb) = (
            scoped(&repo.full(), kind, &q),
            scoped(&repo.full(), kind, &set_state(&q, Some("open"))),
            scoped(&repo.full(), kind, &set_state(&q, Some("closed"))),
        );
        cx.notify();
        self.spawn(
            window,
            cx,
            move |gh, _| gh.graphql(cli::LIST_QUERY, json!({"q": full, "qa": qa, "qb": qb, "after": after})).map(|d| parse_page(&d)),
            move |v, result, window, cx| {
                if v.list_seq[i] != seq {
                    return;
                }
                let list = &mut v.lists[i];
                list.loading = false;
                list.loaded = true;
                match result {
                    Ok(page) => {
                        if append {
                            list.items.extend(page.items);
                        } else {
                            list.items = page.items;
                        }
                        (list.total, list.open, list.closed, list.next) = (page.total, page.open, page.closed, page.next);
                        if !append {
                            v.save_cache(cx);
                        }
                    }
                    Err(e) => {
                        if e.kind == ErrKind::SignedOut {
                            v.phase = Phase::SignedOut;
                        }
                        list.error = Some(e.msg);
                    }
                }
                let _ = window;
                cx.notify();
            },
        );
    }

    pub fn open_item(&mut self, kind: Kind, number: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Browse;
        self.editing = None;
        self.popup = None;
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        let mut state = DetailState::new(kind, number);
        // Show the list row's facts at once; the skeleton stands in for the rest until the answer comes.
        let row = self.lists[idx(kind)].items.iter().find(|i| i.number == number).cloned();
        state.partial = row.is_some();
        state.data = row.map(|item| Detail {
            item,
            body: String::new(),
            assoc: String::new(),
            edited: false,
            locked: false,
            mine: false,
            can_update: false,
            reactions: vec![],
            timeline: vec![],
            next: None,
            pr: None,
        });
        self.detail = Some(state);
        // The cached first page is read off the UI thread and replaces those facts, unless the answer came first.
        let file = self.detail_file(number);
        cx.spawn_in(window, async move |this, cx| {
            let cached = cx
                .background_executor()
                .spawn(async move { std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| parse_detail(&v)).filter(|d| d.item.kind == kind) })
                .await;
            let Some(cached) = cached else { return };
            let _ = this.update_in(cx, |v, _window, cx| {
                if let Some(d) = v.detail.as_mut().filter(|d| d.kind == kind && d.number == number && (d.partial || d.data.is_none())) {
                    (d.data, d.partial) = (Some(cached), false);
                    cx.notify();
                }
            });
        })
        .detach();
        self.fetch_detail(None, window, cx);
        cx.notify();
    }

    pub fn reload_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(d) = self.detail.as_mut() {
            d.loading = true;
            // The files tab shows the patch of the head commit; a new push makes it stale.
            if d.tab == DetailTab::Files {
                d.files = None;
            }
            self.fetch_detail(None, window, cx);
            if self.detail.as_ref().is_some_and(|d| d.tab == DetailTab::Files) {
                self.load_files(window, cx);
            }
        }
    }

    fn fetch_detail(&mut self, after: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail.as_ref() else { return };
        let (kind, number) = (d.kind, d.number);
        let Some(repo) = self.repo.clone() else { return };
        self.detail_seq += 1;
        let seq = self.detail_seq;
        let append = after.is_some();
        let file = self.detail_file(number);
        self.spawn(
            window,
            cx,
            move |gh, _| {
                let vars = json!({"owner": repo.owner, "name": repo.name, "number": number, "after": after});
                let data: Value = gh.graphql(cli::detail_query_text(), vars)?;
                if !append {
                    let _ = crate::datadir::write_atomic(&file, &data.to_string());
                }
                Ok::<_, cli::Error>(data)
            },
            move |v, result, _window, cx| {
                if v.detail_seq != seq {
                    return;
                }
                let Some(d) = v.detail.as_mut().filter(|d| d.number == number && d.kind == kind) else { return };
                d.loading = false;
                d.more_loading = false;
                match result.map(|data| parse_detail(&data)) {
                    Ok(Some(mut fresh)) => {
                        if append && let Some(old) = d.data.take() {
                            let mut timeline = old.timeline;
                            timeline.append(&mut fresh.timeline);
                            fresh.timeline = timeline;
                        }
                        d.error = None;
                        d.partial = false;
                        d.data = Some(fresh);
                    }
                    Ok(None) => {
                        d.partial = false;
                        d.error = Some("This item could not be found.".into());
                    }
                    Err(e) => {
                        d.partial = false;
                        d.error = Some(e.msg);
                    }
                }
                cx.notify();
            },
        );
    }

    fn load_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(d), Some(repo)) = (self.detail.as_mut(), self.repo.clone()) else { return };
        if d.files_loading || d.kind != Kind::Pull {
            return;
        }
        d.files_loading = true;
        d.files_error = None;
        let number = d.number;
        self.spawn(
            window,
            cx,
            move |gh, _| cli::pull_files(&gh, &repo, number, 3).map(|f| parse_files(&f)),
            move |v, result, _w, cx| {
                if let Some(d) = v.detail.as_mut().filter(|d| d.number == number) {
                    d.files_loading = false;
                    match result {
                        Ok(files) => d.files = Some(files),
                        Err(e) => d.files_error = Some(e.msg),
                    }
                }
                cx.notify();
            },
        );
    }

    /// Labels / people / milestones for a picker, fetched once per pane.
    fn ensure_options(&mut self, kind: &PopKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else { return };
        let vars = json!({"owner": repo.owner, "name": repo.name});
        match kind {
            PopKind::FilterLabel | PopKind::Labels if self.options.labels.is_none() => {
                self.spawn(window, cx, move |gh, _| gh.graphql(cli::LABELS_QUERY, vars).map(|d| parse_options_labels(&d)), |v, r, _w, cx| {
                    v.options.labels = Some(r.unwrap_or_default());
                    cx.notify();
                });
            }
            PopKind::FilterAuthor | PopKind::FilterAssignee | PopKind::Assignees | PopKind::Reviewers if self.options.users.is_none() => {
                self.spawn(window, cx, move |gh, _| gh.graphql(cli::USERS_QUERY, vars).map(|d| parse_options_users(&d)), |v, r, _w, cx| {
                    v.options.users = Some(r.unwrap_or_default());
                    cx.notify();
                });
            }
            PopKind::FilterMilestone | PopKind::Milestone if self.options.milestones.is_none() => {
                self.spawn(window, cx, move |gh, _| gh.graphql(cli::MILESTONES_QUERY, vars).map(|d| parse_options_milestones(&d)), |v, r, _w, cx| {
                    v.options.milestones = Some(r.unwrap_or_default());
                    cx.notify();
                });
            }
            _ => {}
        }
    }

    /// Run a change on GitHub, then reload the list and the open item. `ok` runs first on success (clear a box, etc.).
    fn mutate(
        &mut self,
        what: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        job: impl FnOnce(&Gh, &RepoRef) -> Result<(), cli::Error> + Send + 'static,
        ok: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(what.to_string());
        cx.notify();
        self.spawn(
            window,
            cx,
            move |gh, repo| match repo {
                Some(r) => job(&gh, &r),
                None => Err(cli::Error { kind: ErrKind::Other, msg: "no repository".into() }),
            },
            move |v, result, window, cx| {
                v.busy = None;
                match result {
                    Ok(()) => {
                        ok(v, window, cx);
                        v.reload_all(window, cx);
                    }
                    Err(e) => {
                        let hint = if e.msg.contains("Resource not accessible") { " (the token cannot do this here)" } else { "" };
                        v.toast(format!("{}{hint}", e.msg), true, cx);
                        v.reload_all(window, cx);
                    }
                }
                cx.notify();
            },
        );
    }

    fn toast(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        cx.emit(GhEvent::Toast { text: text.into(), error });
    }

    // ------------------------------------------------------------------------------------------------- actions

    fn current_id(&self) -> Option<String> {
        self.detail.as_ref().and_then(|d| d.data.as_ref()).map(|d| d.item.id.clone())
    }

    fn detail_data(&self) -> Option<&Detail> {
        self.detail.as_ref().and_then(|d| d.data.as_ref())
    }

    fn set_query_text(&mut self, q: &str, window: &mut Window, cx: &mut Context<Self>) {
        let shown = SharedString::from(q.to_string());
        self.query_input.update(cx, |s, cx| s.set_value(shown, window, cx));
    }

    pub fn act(&mut self, act: GhAct, window: &mut Window, cx: &mut Context<Self>) {
        // Choosing anything closes an open picker, except the multi-select ones that stay for more clicks.
        if !matches!(act, GhAct::ToggleLabel(_) | GhAct::ToggleAssignee(_) | GhAct::ToggleReviewer(_) | GhAct::Popup(..)) {
            self.popup = None;
        }
        match act {
            GhAct::Refresh => {
                self.options = Options::default();
                if self.phase == Phase::Ready { self.reload_all(window, cx) } else { self.connect(window, cx) }
            }
            GhAct::Retry => self.connect(window, cx),
            GhAct::SignIn => cx.emit(GhEvent::SignIn),
            GhAct::SetKind(k) => {
                self.kind = k;
                self.detail = None;
                self.mode = Mode::Browse;
                let q = self.queries[idx(k)].clone();
                self.set_query_text(&q, window, cx);
                if !self.lists[idx(k)].loaded {
                    self.load_list(k, false, window, cx);
                }
            }
            GhAct::SetQuery(q) => {
                let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
                self.queries[idx(self.kind)] = q.clone();
                self.set_query_text(&q, window, cx);
                self.load_list(self.kind, false, window, cx);
            }
            GhAct::SetState(s) => {
                let q = set_state(&self.queries[idx(self.kind)], s);
                self.act(GhAct::SetQuery(q), window, cx);
            }
            GhAct::Open(kind, n) => self.open_item(kind, n, window, cx),
            GhAct::Back => {
                self.detail = None;
                self.mode = Mode::Browse;
                self.editing = None;
            }
            GhAct::LoadMore => self.load_list(self.kind, true, window, cx),
            GhAct::LoadMoreTimeline => {
                if let Some(d) = self.detail.as_mut()
                    && let Some(next) = d.data.as_ref().and_then(|x| x.next.clone())
                {
                    d.more_loading = true;
                    self.fetch_detail(Some(next), window, cx);
                }
            }
            GhAct::Tab(t) => {
                if let Some(d) = self.detail.as_mut() {
                    d.tab = t;
                }
                if t == DetailTab::Files && self.detail.as_ref().is_some_and(|d| d.files.is_none()) {
                    self.load_files(window, cx);
                }
            }
            GhAct::OpenWeb(url) => {
                if is_web(&url) {
                    cx.open_url(&url);
                }
            }
            GhAct::Copy(text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                self.toast("Copied", false, cx);
            }
            GhAct::New(kind) => {
                self.mode = Mode::New(kind);
                self.detail = None;
                self.draft_labels.clear();
                self.draft_assignees.clear();
                let (head, base) = (
                    crate::git::detect(&self.cwd).and_then(|g| g.branch).unwrap_or_default(),
                    self.info.as_ref().map(|i| i.default_branch.clone()).unwrap_or_default(),
                );
                self.new_title.update(cx, |s, cx| s.set_value("", window, cx));
                self.new_body.update(cx, |s, cx| s.set_value("", window, cx));
                self.new_head.update(cx, |s, cx| s.set_value(head, window, cx));
                self.new_base.update(cx, |s, cx| s.set_value(base, window, cx));
            }
            GhAct::SubmitNew => self.submit_new(window, cx),
            GhAct::Comment(close) => self.comment(close, window, cx),
            GhAct::CloseItem(reason) => {
                let (Some(id), Some(d)) = (self.current_id(), self.detail.as_ref()) else { return };
                let pull = d.kind == Kind::Pull;
                self.show_state(State::Closed, cx);
                self.mutate(
                    "Closing…",
                    window,
                    cx,
                    move |gh, _| {
                        if pull { gh.graphql(cli::M_CLOSE_PR, json!({"id": id})) } else { gh.graphql(cli::M_CLOSE_ISSUE, json!({"id": id, "reason": reason})) }.map(|_| ())
                    },
                    |_, _, _| {},
                );
            }
            GhAct::ReopenItem => {
                let (Some(id), Some(d)) = (self.current_id(), self.detail.as_ref()) else { return };
                let pull = d.kind == Kind::Pull;
                let q = if pull { cli::M_REOPEN_PR } else { cli::M_REOPEN_ISSUE };
                self.show_state(State::Open, cx);
                self.mutate("Reopening…", window, cx, move |gh, _| gh.graphql(q, json!({"id": id})).map(|_| ()), |_, _, _| {});
            }
            GhAct::Review(event) => {
                let body = self.composer.read(cx).value().trim().to_string();
                if body.is_empty() && event != "APPROVE" {
                    self.toast("Write a comment for this review first.", true, cx);
                    return;
                }
                let Some(id) = self.current_id() else { return };
                self.mutate(
                    "Submitting review…",
                    window,
                    cx,
                    move |gh, _| gh.graphql(cli::M_REVIEW, json!({"id": id, "event": event, "body": (!body.is_empty()).then_some(body)})).map(|_| ()),
                    |v, window, cx| v.composer.update(cx, |s, cx| s.set_value("", window, cx)),
                );
            }
            GhAct::ToggleLabel(label) => self.toggle_label(label, window, cx),
            GhAct::ToggleAssignee(user) => self.toggle_assignee(user, window, cx),
            GhAct::ToggleReviewer(user) => {
                let Some(id) = self.current_id() else { return };
                self.mutate("Requesting review…", window, cx, move |gh, _| gh.graphql(cli::M_REQUEST_REVIEW, json!({"id": id, "ids": [user.id]})).map(|_| ()), |_, _, _| {});
            }
            GhAct::SetMilestone(m) => {
                let (Some(id), Some(d)) = (self.current_id(), self.detail.as_ref()) else { return };
                let pull = d.kind == Kind::Pull;
                // An explicit null clears the milestone; the other fields are left out so they stay as they are.
                let vars = json!({"id": id, "milestone": m.map(|m| m.0)});
                let q = if pull { cli::M_EDIT_PR } else { cli::M_EDIT_ISSUE };
                self.mutate("Saving…", window, cx, move |gh, _| gh.graphql(q, vars).map(|_| ()), |_, _, _| {});
            }
            GhAct::PickMergeMethod(m) => self.merge_method = m,
            GhAct::Merge(method) => {
                let (Some(id), Some(pr)) = (self.current_id(), self.detail_data().and_then(|d| d.pr.as_ref())) else { return };
                let head = pr.head_oid.clone();
                self.merge_method = method;
                self.mutate(
                    "Merging…",
                    window,
                    cx,
                    move |gh, _| gh.graphql(cli::M_MERGE, json!({"id": id, "method": method, "head": head})).map(|_| ()),
                    |v, _, cx| v.toast("Pull request merged", false, cx),
                );
            }
            GhAct::AutoMerge(on) => {
                let Some(id) = self.current_id() else { return };
                let method = self.merge_method;
                self.mutate(
                    "Saving…",
                    window,
                    cx,
                    move |gh, _| if on { gh.graphql(cli::M_AUTO_MERGE, json!({"id": id, "method": method})) } else { gh.graphql(cli::M_NO_AUTO_MERGE, json!({"id": id})) }.map(|_| ()),
                    |_, _, _| {},
                );
            }
            which @ (GhAct::Ready | GhAct::ToDraft | GhAct::UpdateBranch) => {
                let Some(id) = self.current_id() else { return };
                let (what, q) = match which {
                    GhAct::Ready => ("Marking ready…", cli::M_READY),
                    GhAct::ToDraft => ("Converting to draft…", cli::M_DRAFT),
                    _ => ("Updating branch…", cli::M_UPDATE_BRANCH),
                };
                self.mutate(what, window, cx, move |gh, _| gh.graphql(q, json!({"id": id})).map(|_| ()), |_, _, _| {});
            }
            GhAct::DeleteBranch => {
                let Some(branch) = self.detail_data().map(|d| d.item.head.clone()).filter(|b| !b.is_empty()) else { return };
                self.mutate(
                    "Deleting branch…",
                    window,
                    cx,
                    move |gh, repo| cli::delete_branch(gh, repo, &branch),
                    |v, _, cx| v.toast("Branch deleted", false, cx),
                );
            }
            GhAct::React(subject, content, remove) => {
                let q = if remove { cli::M_UNREACT } else { cli::M_REACT };
                self.mutate("Saving…", window, cx, move |gh, _| gh.graphql(q, json!({"id": subject, "content": content})).map(|_| ()), |_, _, _| {});
            }
            GhAct::EditStart(target) => self.start_edit(target, window, cx),
            GhAct::EditSave => self.save_edit(window, cx),
            GhAct::EditCancel => self.editing = None,
            GhAct::DeleteComment(id) => {
                self.mutate("Deleting…", window, cx, move |gh, _| gh.graphql(cli::M_DELETE_COMMENT, json!({"id": id})).map(|_| ()), |_, _, _| {});
            }
            GhAct::Lock(lock) => {
                let Some(id) = self.current_id() else { return };
                let q = if lock { cli::M_LOCK } else { cli::M_UNLOCK };
                self.mutate("Saving…", window, cx, move |gh, _| gh.graphql(q, json!({"id": id})).map(|_| ()), |_, _, _| {});
            }
            GhAct::ToggleFile(path) => {
                if let Some(d) = self.detail.as_mut()
                    && !d.expanded.remove(&path) {
                        d.expanded.insert(path);
                    }
            }
            GhAct::ShowAllLines(path) => {
                if let Some(d) = self.detail.as_mut() {
                    d.full.insert(path);
                }
            }
            GhAct::Popup(kind, pos) => self.open_popup(kind, pos, window, cx),
            GhAct::ClosePopup => self.popup = None,
            GhAct::Claude(which) => {
                if let Some(d) = self.detail_data() {
                    cx.emit(GhEvent::AskClaude(claude_prompt(which, &d.item)));
                }
            }
            GhAct::Checkout => {
                if let Some(d) = self.detail.as_ref().filter(|d| d.kind == Kind::Pull) {
                    cx.emit(GhEvent::Checkout(d.number));
                }
            }
            GhAct::Escape => {
                if self.popup.is_some() {
                    self.popup = None;
                } else if self.editing.is_some() {
                    self.editing = None;
                } else if self.detail.is_some() || self.mode != Mode::Browse {
                    self.detail = None;
                    self.mode = Mode::Browse;
                }
            }
        }
        cx.notify();
    }

    fn open_popup(&mut self, kind: PopKind, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_options(&kind, window, cx);
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let sub = cx.subscribe_in(&filter, window, |_, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        filter.update(cx, |s, cx| s.focus(window, cx));
        self.popup = Some(Popup { kind, pos, filter, _sub: sub });
    }

    fn comment(&mut self, close: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.composer.read(cx).value().trim().to_string();
        let (Some(id), Some(d)) = (self.current_id(), self.detail.as_ref()) else { return };
        let pull = d.kind == Kind::Pull;
        if body.is_empty() && close.is_none() {
            return;
        }
        let what = if close.is_some() { "Saving…" } else { "Commenting…" };
        self.mutate(
            what,
            window,
            cx,
            move |gh, _| {
                if !body.is_empty() {
                    gh.graphql(cli::M_COMMENT, json!({"id": id, "body": body}))?;
                }
                match (close, pull) {
                    (Some(true), true) => gh.graphql(cli::M_CLOSE_PR, json!({"id": id}))?,
                    (Some(true), false) => gh.graphql(cli::M_CLOSE_ISSUE, json!({"id": id, "reason": "COMPLETED"}))?,
                    (Some(false), true) => gh.graphql(cli::M_REOPEN_PR, json!({"id": id}))?,
                    (Some(false), false) => gh.graphql(cli::M_REOPEN_ISSUE, json!({"id": id}))?,
                    (None, _) => Value::Null,
                };
                Ok(())
            },
            |v, window, cx| v.composer.update(cx, |s, cx| s.set_value("", window, cx)),
        );
    }

    fn toggle_label(&mut self, label: Label, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::New(_)) {
            match self.draft_labels.iter().position(|l| l.id == label.id) {
                Some(i) => drop(self.draft_labels.remove(i)),
                None => self.draft_labels.push(label),
            }
            return;
        }
        let (Some(id), Some(d)) = (self.current_id(), self.detail_data()) else { return };
        let has = d.item.labels.iter().any(|l| l.name == label.name);
        let q = if has { cli::M_REMOVE_LABEL } else { cli::M_ADD_LABEL };
        self.edit_item(cx, |i| if has { i.labels.retain(|l| l.name != label.name) } else { i.labels.push(label.clone()) });
        self.mutate("Saving…", window, cx, move |gh, _| gh.graphql(q, json!({"id": id, "ids": [label.id]})).map(|_| ()), |_, _, _| {});
    }

    /// Show a change in the open item and its list row before GitHub confirms it; a failure reloads the truth.
    fn edit_item(&mut self, cx: &mut Context<Self>, f: impl Fn(&mut Item)) {
        // A change that `mutate` is about to refuse (busy) is not shown either.
        if self.busy.is_some() {
            return;
        }
        let Some(d) = self.detail.as_mut() else { return };
        if let Some(data) = d.data.as_mut() {
            f(&mut data.item);
        }
        if let Some(i) = self.lists[idx(d.kind)].items.iter_mut().find(|i| i.number == d.number) {
            f(i);
        }
        cx.notify();
    }

    fn show_state(&mut self, state: State, cx: &mut Context<Self>) {
        self.edit_item(cx, |i| i.state = state);
    }

    fn toggle_assignee(&mut self, user: User, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::New(_)) {
            match self.draft_assignees.iter().position(|u| u.id == user.id) {
                Some(i) => drop(self.draft_assignees.remove(i)),
                None => self.draft_assignees.push(user),
            }
            return;
        }
        let (Some(id), Some(d)) = (self.current_id(), self.detail_data()) else { return };
        let has = d.item.assignees.iter().any(|u| u.login == user.login);
        let q = if has { cli::M_REMOVE_ASSIGNEE } else { cli::M_ADD_ASSIGNEE };
        self.mutate("Saving…", window, cx, move |gh, _| gh.graphql(q, json!({"id": id, "ids": [user.id]})).map(|_| ()), |_, _, _| {});
    }

    fn start_edit(&mut self, target: Edit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.detail_data() else { return };
        match &target {
            Edit::Title => {
                let t = SharedString::from(d.item.title.clone());
                self.edit_title.update(cx, |s, cx| {
                    s.set_value(t, window, cx);
                    s.focus(window, cx);
                });
            }
            Edit::Body => {
                let t = SharedString::from(d.body.clone());
                self.edit_body.update(cx, |s, cx| s.set_value(t, window, cx));
            }
            Edit::Comment(id) => {
                let body = d.timeline.iter().find_map(|e| match e {
                    Entry::Comment(c) if &c.id == id => Some(c.body.clone()),
                    _ => None,
                });
                self.edit_body.update(cx, |s, cx| s.set_value(SharedString::from(body.unwrap_or_default()), window, cx));
            }
        }
        self.editing = Some(target);
    }

    fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(target), Some(id), Some(d)) = (self.editing.clone(), self.current_id(), self.detail.as_ref()) else { return };
        let pull = d.kind == Kind::Pull;
        let (title, body) = (self.edit_title.read(cx).value().trim().to_string(), self.edit_body.read(cx).value().to_string());
        let edit = |kind_pull: bool| if kind_pull { cli::M_EDIT_PR } else { cli::M_EDIT_ISSUE };
        match target {
            Edit::Title if title.is_empty() => (),
            Edit::Title => {
                let q = edit(pull);
                self.mutate("Saving…", window, cx, move |gh, _| gh.graphql(q, json!({"id": id, "title": title})).map(|_| ()), |v, _, _| v.editing = None);
            }
            Edit::Body => {
                let q = edit(pull);
                self.mutate("Saving…", window, cx, move |gh, _| gh.graphql(q, json!({"id": id, "body": body})).map(|_| ()), |v, _, _| v.editing = None);
            }
            Edit::Comment(cid) => {
                self.mutate("Saving…", window, cx, move |gh, _| gh.graphql(cli::M_EDIT_COMMENT, json!({"id": cid, "body": body})).map(|_| ()), |v, _, _| v.editing = None);
            }
        }
    }

    fn submit_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Mode::New(kind) = self.mode else { return };
        let (Some(info), Some(_)) = (self.info.clone(), self.repo.clone()) else { return };
        let title = self.new_title.read(cx).value().trim().to_string();
        let body = self.new_body.read(cx).value().to_string();
        if title.is_empty() {
            self.toast("A title is required.", true, cx);
            return;
        }
        let labels: Vec<String> = self.draft_labels.iter().map(|l| l.id.clone()).collect();
        let assignees: Vec<String> = self.draft_assignees.iter().map(|u| u.id.clone()).collect();
        let (head, base) = (self.new_head.read(cx).value().trim().to_string(), self.new_base.read(cx).value().trim().to_string());
        if kind == Kind::Pull && (head.is_empty() || base.is_empty()) {
            self.toast("Choose the branch to merge and the branch to merge into.", true, cx);
            return;
        }
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Creating…".into());
        cx.notify();
        self.spawn(
            window,
            cx,
            move |gh, _| {
                if kind == Kind::Issue {
                    let vars = json!({"repo": info.id, "title": title, "body": body, "labels": labels, "assignees": assignees});
                    gh.graphql(cli::M_CREATE_ISSUE, vars).map(|d| d["createIssue"]["issue"]["number"].as_u64())
                } else {
                    let vars = json!({"repo": info.id, "title": title, "body": body, "base": base, "head": head, "draft": false});
                    gh.graphql(cli::M_CREATE_PR, vars).map(|d| d["createPullRequest"]["pullRequest"]["number"].as_u64())
                }
            },
            move |v, result, window, cx| {
                v.busy = None;
                match result {
                    Ok(Some(n)) => {
                        v.toast(format!("Created #{n}"), false, cx);
                        v.load_list(kind, false, window, cx);
                        v.open_item(kind, n as u32, window, cx);
                    }
                    Ok(None) => v.toast("Created, but GitHub did not say which number.", false, cx),
                    Err(e) => v.toast(e.msg, true, cx),
                }
                cx.notify();
            },
        );
    }
}

/// The prompt for a Claude session about the open item. Short on purpose: Claude reads the details itself with `gh`.
pub fn claude_prompt(which: &str, item: &Item) -> String {
    let (n, title, url) = (item.number, &item.title, &item.url);
    match (which, item.kind) {
        ("investigate", _) => format!("Look into GitHub issue #{n} “{title}” ({url}). Read it with `gh issue view {n} --comments`, find the cause in this repository and explain it. Do not change any files yet."),
        ("review", _) => format!("Review GitHub pull request #{n} “{title}” ({url}). Read it with `gh pr view {n} --comments` and `gh pr diff {n}`. Report bugs, risks and missing tests, most serious first. Do not push anything."),
        ("address", _) => format!("Address the review feedback on GitHub pull request #{n} “{title}” ({url}). Read it with `gh pr view {n} --comments` and check out the branch with `gh pr checkout {n}`. Make the changes locally, but ask before pushing."),
        ("summarize", Kind::Pull) => format!("Summarize GitHub pull request #{n} “{title}” ({url}) for me: what it changes, why, and what to check before merging. Use `gh pr view {n} --comments` and `gh pr diff {n}`."),
        ("summarize", Kind::Issue) => format!("Summarize GitHub issue #{n} “{title}” ({url}) and its discussion for me. Use `gh issue view {n} --comments`."),
        (_, Kind::Pull) => format!("Work on GitHub pull request #{n} “{title}” ({url}). Read it with `gh pr view {n} --comments`."),
        _ => format!("Work on GitHub issue #{n} “{title}” ({url}). Read it first with `gh issue view {n} --comments`, then propose a short plan and implement it in this repository. Ask me if anything is unclear."),
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
