//! What a click or key in the GitHub pane does: `GhAct` dispatch and the edits, comments and creations behind it.

use super::*;

impl GithubView {
    pub(super) fn toast(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        cx.emit(GhEvent::Toast { text: text.into(), error });
    }

    // ------------------------------------------------------------------------------------------------- actions

    pub(super) fn current_id(&self) -> Option<String> {
        self.detail.as_ref().and_then(|d| d.data.as_ref()).map(|d| d.item.id.clone())
    }

    pub(super) fn detail_data(&self) -> Option<&Detail> {
        self.detail.as_ref().and_then(|d| d.data.as_ref())
    }

    pub(super) fn set_query_text(&mut self, q: &str, window: &mut Window, cx: &mut Context<Self>) {
        let shown = SharedString::from(q.to_string());
        self.query_input.update(cx, |s, cx| s.set_value(shown, window, cx));
    }

    /// Switch the Issues / Pull requests tab: show that kind's search and load its list if it hasn't been.
    pub(super) fn select_kind(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = kind;
        let q = self.queries[idx(kind)].clone();
        self.set_query_text(&q, window, cx);
        if !self.lists[idx(kind)].loaded {
            self.load_list(kind, false, window, cx);
        }
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
                self.detail = None;
                self.mode = Mode::Browse;
                self.select_kind(k, window, cx);
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

    pub(super) fn open_popup(&mut self, kind: PopKind, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn comment(&mut self, close: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn toggle_label(&mut self, label: Label, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn edit_item(&mut self, cx: &mut Context<Self>, f: impl Fn(&mut Item)) {
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

    pub(super) fn show_state(&mut self, state: State, cx: &mut Context<Self>) {
        self.edit_item(cx, |i| i.state = state);
    }

    pub(super) fn toggle_assignee(&mut self, user: User, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn start_edit(&mut self, target: Edit, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn submit_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
