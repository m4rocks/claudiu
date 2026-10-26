//! Loading for the GitHub pane: the on-disk cache, background `gh` calls, refresh and the list and detail fetches.

use super::*;

impl GithubView {
    pub(super) fn cache_file(&self) -> PathBuf {
        crate::datadir::data_dir().join("github-cache").join(format!("{}.json", crate::datadir::safe_file_stem(&self.cwd.to_string_lossy())))
    }

    /// Fill the lists from the last run so the pane is populated before `gh` answers; only the default query is cached.
    /// The file is read off the UI thread, and then the lookup starts, so a cached repository goes first.
    pub(super) fn load_cache(&self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn detail_file(&self, number: u32) -> PathBuf {
        let dir = crate::datadir::data_dir().join("github-cache");
        dir.join(format!("{}.{number}.json", crate::datadir::safe_file_stem(&self.cwd.to_string_lossy())))
    }

    /// Write the first page of each list after a fresh load (skipped while a search is active). Written off the UI thread.
    pub(super) fn save_cache(&self, cx: &mut Context<Self>) {
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
    pub(super) fn spawn<T: Send + 'static>(
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
    pub(super) fn start_refresh_loop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn is_idle(&self) -> bool {
        self.busy.is_none() && self.editing.is_none() && self.popup.is_none() && self.phase == Phase::Ready
    }

    /// The 90-second refresh: only what is on screen; the other list reloads when its tab is chosen.
    pub(super) fn reload_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        // The tab strip and the list behind the detail follow the item, so Back lands on the item's own list.
        self.select_kind(kind, window, cx);
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

    pub(super) fn fetch_detail(&mut self, after: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn load_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn ensure_options(&mut self, kind: &PopKind, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn mutate(
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
}
