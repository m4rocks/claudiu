//! What a click or key in the GitHub pane does: `GhAct` dispatch and the edits, comments and creations behind it.

use super::*;

impl GithubView {
    pub(super) fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.events.push(GhEvent::Toast { text: text.into(), error });
    }

    // ------------------------------------------------------------------------------------------------- actions

    pub(super) fn current_id(&self) -> Option<String> {
        self.detail.as_ref().and_then(|d| d.data.as_ref()).map(|d| d.item.id.clone())
    }

    pub(super) fn detail_data(&self) -> Option<&Detail> {
        self.detail.as_ref().and_then(|d| d.data.as_ref())
    }

    pub(super) fn set_query_text(&mut self, q: &str) {
        self.query_input = field(q, false);
    }

    /// Switch the Issues / Pull requests tab: show that kind's search and load its list if it hasn't been.
    pub(super) fn select_kind(&mut self, kind: Kind) {
        self.kind = kind;
        let q = self.queries[idx(kind)].clone();
        self.set_query_text(&q);
        if !self.lists[idx(kind)].loaded {
            self.load_list(kind, false);
        }
    }

    pub fn act(&mut self, act: GhAct) {
        // Choosing anything closes an open picker, except the multi-select ones that stay for more clicks.
        if !matches!(act, GhAct::ToggleLabel(_) | GhAct::ToggleAssignee(_) | GhAct::ToggleReviewer(_) | GhAct::Popup(..)) {
            self.popup = None;
        }
        match act {
            GhAct::Refresh => {
                self.options = Options::default();
                if self.phase == Phase::Ready { self.reload_all() } else { self.connect() }
            }
            GhAct::Retry => self.connect(),
            GhAct::SignIn => self.events.push(GhEvent::SignIn),
            GhAct::SetKind(k) => {
                self.detail = None;
                self.mode = Mode::Browse;
                self.select_kind(k);
            }
            GhAct::SetQuery(q) => {
                let q = q.split_whitespace().collect::<Vec<_>>().join(" ");
                self.queries[idx(self.kind)] = q.clone();
                self.set_query_text(&q);
                self.load_list(self.kind, false);
            }
            GhAct::SubmitQuery => {
                let q = self.query_input.text.clone();
                self.act(GhAct::SetQuery(q));
            }
            GhAct::SetState(s) => {
                let q = set_state(&self.queries[idx(self.kind)], s);
                self.act(GhAct::SetQuery(q));
            }
            GhAct::Open(kind, n) => self.open_item(kind, n),
            GhAct::Back => {
                self.detail = None;
                self.mode = Mode::Browse;
                self.editing = None;
            }
            GhAct::LoadMore => self.load_list(self.kind, true),
            GhAct::LoadMoreTimeline => {
                if let Some(d) = self.detail.as_mut()
                    && let Some(next) = d.data.as_ref().and_then(|x| x.next.clone())
                {
                    d.more_loading = true;
                    self.fetch_detail(Some(next));
                }
            }
            GhAct::Tab(t) => {
                if let Some(d) = self.detail.as_mut() {
                    d.tab = t;
                }
                if t == DetailTab::Files && self.detail.as_ref().is_some_and(|d| d.files.is_none()) {
                    self.load_files();
                }
            }
            GhAct::OpenWeb(url) => {
                if is_web(&url) {
                    self.fx.push(Box::new(move |tx: &mut rune::Tx| { let _ = tx.open_url(&url); }));
                }
            }
            GhAct::Copy(text) => {
                self.fx.push(Box::new(move |tx: &mut rune::Tx| tx.copy(&text)));
                self.toast("Copied", false);
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
                self.new_title = field("", true);
                self.new_body = field("", false);
                self.new_head = field(&head, false);
                self.new_base = field(&base, false);
            }
            GhAct::SubmitNew => self.submit_new(),
            GhAct::Comment(close) => self.comment(close),
            GhAct::CloseItem(reason) => {
                let (Some(id), Some(d)) = (self.current_id(), self.detail.as_ref()) else { return };
                let pull = d.kind == Kind::Pull;
                self.show_state(State::Closed);
                self.mutate(
                    "Closing…",
                    move |gh, _| {
                        if pull { gh.graphql(cli::M_CLOSE_PR, json!({"id": id})) } else { gh.graphql(cli::M_CLOSE_ISSUE, json!({"id": id, "reason": reason})) }.map(|_| ())
                    },
                    |_| {},
                );
            }
            GhAct::ReopenItem => {
                let (Some(id), Some(d)) = (self.current_id(), self.detail.as_ref()) else { return };
                let pull = d.kind == Kind::Pull;
                let q = if pull { cli::M_REOPEN_PR } else { cli::M_REOPEN_ISSUE };
                self.show_state(State::Open);
                self.mutate("Reopening…", move |gh, _| gh.graphql(q, json!({"id": id})).map(|_| ()), |_| {});
            }
            GhAct::Review(event) => {
                let body = self.composer.text.as_str().trim().to_string();
                if body.is_empty() && event != "APPROVE" {
                    self.toast("Write a comment for this review first.", true);
                    return;
                }
                let Some(id) = self.current_id() else { return };
                self.mutate(
                    "Submitting review…",
                    move |gh, _| gh.graphql(cli::M_REVIEW, json!({"id": id, "event": event, "body": (!body.is_empty()).then_some(body)})).map(|_| ()),
                    |v| v.composer = composer_field(""),
                );
            }
            GhAct::ToggleLabel(label) => self.toggle_label(label),
            GhAct::ToggleAssignee(user) => self.toggle_assignee(user),
            GhAct::ToggleReviewer(user) => {
                let Some(id) = self.current_id() else { return };
                self.mutate("Requesting review…", move |gh, _| gh.graphql(cli::M_REQUEST_REVIEW, json!({"id": id, "ids": [user.id]})).map(|_| ()), |_| {});
            }
            GhAct::SetMilestone(m) => {
                let (Some(id), Some(d)) = (self.current_id(), self.detail.as_ref()) else { return };
                let pull = d.kind == Kind::Pull;
                // An explicit null clears the milestone; the other fields are left out so they stay as they are.
                let vars = json!({"id": id, "milestone": m.map(|m| m.0)});
                let q = if pull { cli::M_EDIT_PR } else { cli::M_EDIT_ISSUE };
                self.mutate("Saving…", move |gh, _| gh.graphql(q, vars).map(|_| ()), |_| {});
            }
            GhAct::PickMergeMethod(m) => self.merge_method = m,
            GhAct::Merge(method) => {
                let (Some(id), Some(pr)) = (self.current_id(), self.detail_data().and_then(|d| d.pr.as_ref())) else { return };
                let head = pr.head_oid.clone();
                self.merge_method = method;
                self.mutate(
                    "Merging…",
                    move |gh, _| gh.graphql(cli::M_MERGE, json!({"id": id, "method": method, "head": head})).map(|_| ()),
                    |v| v.toast("Pull request merged", false),
                );
            }
            GhAct::AutoMerge(on) => {
                let Some(id) = self.current_id() else { return };
                let method = self.merge_method;
                self.mutate(
                    "Saving…",
                    move |gh, _| if on { gh.graphql(cli::M_AUTO_MERGE, json!({"id": id, "method": method})) } else { gh.graphql(cli::M_NO_AUTO_MERGE, json!({"id": id})) }.map(|_| ()),
                    |_| {},
                );
            }
            which @ (GhAct::Ready | GhAct::ToDraft | GhAct::UpdateBranch) => {
                let Some(id) = self.current_id() else { return };
                let (what, q) = match which {
                    GhAct::Ready => ("Marking ready…", cli::M_READY),
                    GhAct::ToDraft => ("Converting to draft…", cli::M_DRAFT),
                    _ => ("Updating branch…", cli::M_UPDATE_BRANCH),
                };
                self.mutate(what, move |gh, _| gh.graphql(q, json!({"id": id})).map(|_| ()), |_| {});
            }
            GhAct::DeleteBranch => {
                let Some(branch) = self.detail_data().map(|d| d.item.head.clone()).filter(|b| !b.is_empty()) else { return };
                self.mutate(
                    "Deleting branch…",
                    move |gh, repo| cli::delete_branch(gh, repo, &branch),
                    |v| v.toast("Branch deleted", false),
                );
            }
            GhAct::React(subject, content, remove) => {
                let q = if remove { cli::M_UNREACT } else { cli::M_REACT };
                self.mutate("Saving…", move |gh, _| gh.graphql(q, json!({"id": subject, "content": content})).map(|_| ()), |_| {});
            }
            GhAct::EditStart(target) => self.start_edit(target),
            GhAct::EditSave => self.save_edit(),
            GhAct::EditCancel => self.editing = None,
            GhAct::DeleteComment(id) => {
                self.mutate("Deleting…", move |gh, _| gh.graphql(cli::M_DELETE_COMMENT, json!({"id": id})).map(|_| ()), |_| {});
            }
            GhAct::Lock(lock) => {
                let Some(id) = self.current_id() else { return };
                let q = if lock { cli::M_LOCK } else { cli::M_UNLOCK };
                self.mutate("Saving…", move |gh, _| gh.graphql(q, json!({"id": id})).map(|_| ()), |_| {});
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
            GhAct::Popup(kind, anchor) => self.open_popup(kind, anchor),
            GhAct::Claude(which) => {
                if let Some(d) = self.detail_data() {
                    self.events.push(GhEvent::AskClaude(claude_prompt(which, &d.item)));
                }
            }
            GhAct::Checkout => {
                if let Some(d) = self.detail.as_ref().filter(|d| d.kind == Kind::Pull) {
                    self.events.push(GhEvent::Checkout(d.number));
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
    }

    pub(super) fn open_popup(&mut self, kind: PopKind, anchor: String) {
        self.ensure_options(&kind);
        self.popup = Some(Popup { kind, anchor, filter: field("", true) });
    }

    pub(super) fn comment(&mut self, close: Option<bool>) {
        let body = self.composer.text.as_str().trim().to_string();
        let (Some(id), Some(d)) = (self.current_id(), self.detail.as_ref()) else { return };
        let pull = d.kind == Kind::Pull;
        if body.is_empty() && close.is_none() {
            return;
        }
        let what = if close.is_some() { "Saving…" } else { "Commenting…" };
        self.mutate(
            what,
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
            |v| v.composer = composer_field(""),
        );
    }

    pub(super) fn toggle_label(&mut self, label: Label) {
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
        self.edit_item(|i| if has { i.labels.retain(|l| l.name != label.name) } else { i.labels.push(label.clone()) });
        self.mutate("Saving…", move |gh, _| gh.graphql(q, json!({"id": id, "ids": [label.id]})).map(|_| ()), |_| {});
    }

    /// Show a change in the open item and its list row before GitHub confirms it; a failure reloads the truth.
    pub(super) fn edit_item(&mut self, f: impl Fn(&mut Item)) {
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
    }

    pub(super) fn show_state(&mut self, state: State) {
        self.edit_item(|i| i.state = state);
    }

    pub(super) fn toggle_assignee(&mut self, user: User) {
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
        self.mutate("Saving…", move |gh, _| gh.graphql(q, json!({"id": id, "ids": [user.id]})).map(|_| ()), |_| {});
    }

    pub(super) fn start_edit(&mut self, target: Edit) {
        let Some(d) = self.detail_data() else { return };
        match &target {
            Edit::Title => {
                self.edit_title = field(&d.item.title, true);
            }
            Edit::Body => {
                self.edit_body = field(&d.body, true);
            }
            Edit::Comment(id) => {
                let body = d.timeline.iter().find_map(|e| match e {
                    Entry::Comment(c) if &c.id == id => Some(c.body.clone()),
                    _ => None,
                });
                self.edit_body = field(&body.unwrap_or_default(), true);
            }
        }
        self.editing = Some(target);
    }

    pub(super) fn save_edit(&mut self) {
        let (Some(target), Some(id), Some(d)) = (self.editing.clone(), self.current_id(), self.detail.as_ref()) else { return };
        let pull = d.kind == Kind::Pull;
        let (title, body) = (self.edit_title.text.as_str().trim().to_string(), self.edit_body.text.as_str().to_string());
        let edit = |kind_pull: bool| if kind_pull { cli::M_EDIT_PR } else { cli::M_EDIT_ISSUE };
        match target {
            Edit::Title if title.is_empty() => (),
            Edit::Title => {
                let q = edit(pull);
                self.mutate("Saving…", move |gh, _| gh.graphql(q, json!({"id": id, "title": title})).map(|_| ()), |v| v.editing = None);
            }
            Edit::Body => {
                let q = edit(pull);
                self.mutate("Saving…", move |gh, _| gh.graphql(q, json!({"id": id, "body": body})).map(|_| ()), |v| v.editing = None);
            }
            Edit::Comment(cid) => {
                self.mutate("Saving…", move |gh, _| gh.graphql(cli::M_EDIT_COMMENT, json!({"id": cid, "body": body})).map(|_| ()), |v| v.editing = None);
            }
        }
    }

    pub(super) fn submit_new(&mut self) {
        let Mode::New(kind) = self.mode else { return };
        let (Some(info), Some(_)) = (self.info.clone(), self.repo.clone()) else { return };
        let title = self.new_title.text.as_str().trim().to_string();
        let body = self.new_body.text.as_str().to_string();
        if title.is_empty() {
            self.toast("A title is required.", true);
            return;
        }
        let labels: Vec<String> = self.draft_labels.iter().map(|l| l.id.clone()).collect();
        let assignees: Vec<String> = self.draft_assignees.iter().map(|u| u.id.clone()).collect();
        let (head, base) = (self.new_head.text.as_str().trim().to_string(), self.new_base.text.as_str().trim().to_string());
        if kind == Kind::Pull && (head.is_empty() || base.is_empty()) {
            self.toast("Choose the branch to merge and the branch to merge into.", true);
            return;
        }
        if self.busy.is_some() {
            return;
        }
        self.busy = Some("Creating…".into());
        self.spawn(
            move |gh, _| {
                if kind == Kind::Issue {
                    let vars = json!({"repo": info.id, "title": title, "body": body, "labels": labels, "assignees": assignees});
                    gh.graphql(cli::M_CREATE_ISSUE, vars).map(|d| d["createIssue"]["issue"]["number"].as_u64())
                } else {
                    let vars = json!({"repo": info.id, "title": title, "body": body, "base": base, "head": head, "draft": false});
                    gh.graphql(cli::M_CREATE_PR, vars).map(|d| d["createPullRequest"]["pullRequest"]["number"].as_u64())
                }
            },
            move |v, result| {
                v.busy = None;
                match result {
                    Ok(Some(n)) => {
                        v.toast(format!("Created #{n}"), false);
                        v.load_list(kind, false);
                        v.open_item(kind, n as u32);
                    }
                    Ok(None) => v.toast("Created, but GitHub did not say which number.", false),
                    Err(e) => v.toast(e.msg, true),
                }
            },
        );
    }
}
