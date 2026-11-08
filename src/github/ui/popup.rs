//! The filter, label, assignee and sort popups over the list.

use super::*;

impl GithubView {
    pub(super) fn popup_items(&self, kind: &PopKind) -> (Vec<PopItem>, bool) {
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

    pub(super) fn render_popup(&self) -> Option<El> {
        let popup = self.popup.as_ref()?;
        let (items, loading) = self.popup_items(&popup.kind);
        let needle = popup.filter.text.to_lowercase();
        let searchable = !matches!(popup.kind, PopKind::Merge | PopKind::CloseReason | PopKind::Claude | PopKind::More | PopKind::React(_) | PopKind::Comment(..) | PopKind::FilterSort | PopKind::FilterReview | PopKind::FilterChecks);
        let mut list = col().key("gh-pop-list").w_full().max_h(320.0).scroll(ScrollSpec { stick_bottom: false });
        let mut shown = 0;
        for (i, it) in items.into_iter().enumerate() {
            if searchable && !needle.is_empty() && !it.label.to_lowercase().contains(&needle) {
                continue;
            }
            shown += 1;
            let mut r = row()
                .key(format!("gh-pop-{i}"))
                .w_full()
                .h(28.0)
                .px(10.0)
                .gap(8.0)
                .center_y()
                .radius(5.0)
                .cursor(Cursor::Pointer)
                .role("menuitem")
                .hover(Color::WHITE.alpha(0.06))
                .child(row().w(12.0).child(if it.checked == Some(true) { icon(&icons::CHECK, 13.0, theme::ACCENT) } else { el() }));
            if let Some(c) = it.swatch {
                r = r.child(el().size(10.0, 10.0).radius(5.0).fill(rgb(c)));
            }
            r = r.child(label(it.label, 12.5, theme::TEXT).truncate().grow(1.0).shrink());
            if let Some(h) = it.hint {
                r = r.child(faint(h));
            }
            list = list.child(r.on_press_tx(go(&self.id, it.act)));
        }
        if loading {
            list = list.child(row().px(10.0).py(6.0).gap(8.0).center_y().child(spinner(12.0, theme::TEXT_DIM)).child(dim("Loading…")));
        } else if shown == 0 {
            list = list.child(el().px(10.0).py(6.0).child(dim("Nothing matches")));
        }
        let pane = self.id.clone();
        let mut card = col()
            .key(format!("gh-popup-{:?}", popup.kind))
            .w(272.0)
            .padding(4.0)
            .gap(4.0)
            .radius(9.0)
            .fill(rgb(0x18181a))
            .border(1.0, rgb(0x2c2c30))
            .shadow(10.0, 28.0, Color::BLACK.alpha(0.5))
            .role("menu")
            .transition(Transition::scale(0.96).combined(Transition::opacity()))
            .modal(true, move |ws: &mut Workspace| {
                if let Some(v) = ws.gh_view_mut(&pane) {
                    v.popup = None;
                }
            });
        if searchable {
            card = card.child(el().padding(2.0).child(gh_input(&self.id, "gh-pop-filter", &popup.filter, |v| match v.popup.as_mut() {
                Some(p) => &mut p.filter,
                None => &mut v.query_input,
            }, "Filter", false, None)));
        }
        Some(card.child(list).anchored(popup.anchor.clone(), rune::Side::Bottom, rune::Align::Start, 4.0))
    }
}

pub(super) fn sort_name(key: &str) -> &'static str {
    match key {
        "created-asc" => "Oldest",
        "updated-desc" => "Recently updated",
        "updated-asc" => "Least recently updated",
        "comments-desc" => "Most commented",
        "comments-asc" => "Least commented",
        _ => "Newest",
    }
}
