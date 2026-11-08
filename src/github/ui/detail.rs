//! The detail side of the GitHub pane: header, conversation, commits and reviews of one item.

use super::*;

impl GithubView {
    pub(super) fn render_detail(&self, stacked: bool) -> El {
        if let Mode::New(kind) = self.mode {
            return self.render_new(kind, stacked);
        }
        let Some(ds) = &self.detail else { return self.render_empty_detail() };
        let back = stacked.then(|| row().px(12.0).pt(8.0).child(button_leading(&icons::ARROW_LEFT, "Back to list", false).key("gh-back").on_press_tx(go(&self.id, GhAct::Back))));
        let Some(d) = &ds.data else {
            let msg = match &ds.error {
                Some(e) => label(e.clone(), 12.0, theme::DANGER).wrap(),
                None => row().gap(8.0).center_y().child(spinner(12.0, theme::TEXT_DIM)).child(dim("Loading…")),
            };
            let mut c = col().w_full().h_full();
            if let Some(b) = back {
                c = c.child(b);
            }
            return c.child(el().padding(18.0).child(msg));
        };
        let t = now();
        let can_triage = self.info.as_ref().is_some_and(|i| i.can_triage());
        let it = &d.item;
        let (_, color, state_name) = state_style(it);
        let editing_title = self.editing == Some(Edit::Title);

        let title_row = if editing_title {
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(col().grow(1.0).shrink().child(gh_input(&self.id, "gh-edit-title-input", &self.edit_title, |v| &mut v.edit_title, "Title", false, Some(GhAct::EditSave))))
                .child(button("Save", true).key("gh-title-save").on_press_tx(go(&self.id, GhAct::EditSave)))
                .child(button("Cancel", false).key("gh-title-cancel").on_press_tx(go(&self.id, GhAct::EditCancel)))
        } else {
            let mut r = row()
                .w_full()
                .gap(8.0)
                .child(rune::rich(format!("{}  #{}", it.title, it.number), vec![]).font(font(18.0, 600.0)).color(rgb(theme::TEXT)).grow(1.0).shrink().min_w(0.0));
            if d.can_update || d.mine {
                r = r.child(icon_button(&icons::PENCIL).key("gh-edit-title").help("Edit title").on_press_tx(go(&self.id, GhAct::EditStart(Edit::Title))));
            }
            r
        };

        let mut status = row().w_full().flex_wrap().center_y().gap(8.0).child(pill(state_name, color));
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
            let head = format!("{}{} → {}", if pr.cross_repo { format!("{}:", pr.head_repo.split('/').next().unwrap_or("")) } else { String::new() }, it.head, it.base);
            status = status
                .child(row().px(7.0).py(1.0).radius(5.0).fill(rgb(0x1a1a1b)).child(mono(head, 11.5, theme::TEXT_DIM)))
                .child(row().gap(6.0).child(label(format!("+{}", pr.additions), 12.0, GREEN)).child(label(format!("−{}", pr.deletions), 12.0, RED)));
        }

        let url = it.url.clone();
        let ask = button_leading(&icons::SPARKLES, "Ask Claude", true).child(icon(&icons::CHEVRON_DOWN, 12.0, 0x17100d));
        let mut actions = row()
            .w_full()
            .flex_wrap()
            .center_y()
            .gap(6.0)
            .child(popup_trigger(ask, &self.id, "gh-claude".into(), PopKind::Claude))
            .child(button_leading(&icons::EXTERNAL_LINK, "Open on GitHub", false).key("gh-open-web").on_press_tx(go(&self.id, GhAct::OpenWeb(url))))
            .child(popup_trigger(icon_button(&icons::ELLIPSIS), &self.id, "gh-more-menu".into(), PopKind::More));
        if ds.loading {
            actions = actions.child(row().key("gh-refreshing").gap(6.0).center_y().transition(Transition::opacity()).child(spinner(12.0, theme::TEXT_FAINT)).child(dim("Refreshing…")));
        }
        let header = col()
            .w_full()
            .gap(8.0)
            .px(16.0)
            .pt(12.0)
            .pb(10.0)
            .border_sides([0.0, 0.0, 1.0, 0.0], rgb(theme::BORDER))
            .child(title_row)
            .child(status)
            .child(actions);

        // ---- meta: labels, assignees, milestone, reviewers
        let edit_btn = |id: &str, kind: PopKind| -> Option<El> {
            can_triage.then(|| {
                let b = row().px(6.0).h(20.0).center_y().radius(5.0).cursor(Cursor::Pointer).role("button").hover(Color::WHITE.alpha(0.06)).child(label("＋ Edit", 12.0, theme::TEXT_FAINT));
                popup_trigger(b, &self.id, format!("gh-meta-{id}"), kind)
            })
        };
        let meta_row = |name: &str, value: El, edit: Option<El>| {
            let mut r = row().w_full().gap(10.0).child(el().w(84.0).pt(2.0).child(section_label(name.to_uppercase()))).child(col().grow(1.0).shrink().min_w(0.0).child(value));
            if let Some(e) = edit {
                r = r.child(e);
            }
            r
        };
        let mut labels = row().flex_wrap().gap(5.0);
        for l in &it.labels {
            labels = labels.child(label_pill(l));
        }
        if it.labels.is_empty() {
            labels = labels.child(faint("None yet"));
        }
        let mut people = row().flex_wrap().gap(10.0);
        for u in &it.assignees {
            people = people.child(row().center_y().gap(5.0).child(avatar(&u.login, 16.0)).child(dim(u.login.clone())));
        }
        if it.assignees.is_empty() {
            people = people.child(faint("No one"));
        }
        let milestone = match &it.milestone {
            Some((_, m)) => dim(m.clone()),
            None => faint("None"),
        };
        let mut meta = col()
            .w_full()
            .gap(8.0)
            .child(meta_row("Labels", labels, edit_btn("labels", PopKind::Labels)))
            .child(meta_row("Assignees", people, edit_btn("assignees", PopKind::Assignees)))
            .child(meta_row("Milestone", milestone, edit_btn("milestone", PopKind::Milestone)));
        if let Some(pr) = &d.pr {
            let mut rv = row().flex_wrap().gap(10.0);
            for (who, state) in &pr.reviews {
                let (g, c) = match state.as_str() {
                    "APPROVED" => (&icons::CIRCLE_CHECK, GREEN),
                    "CHANGES_REQUESTED" => (&icons::CIRCLE_X, RED),
                    _ => (&icons::CIRCLE_DOT, GRAY),
                };
                rv = rv.child(row().center_y().gap(4.0).child(icon(g, 13.0, c)).child(dim(who.clone())));
            }
            for who in pr.requested.iter().filter(|w| !pr.reviews.iter().any(|(r, _)| r == *w)) {
                rv = rv.child(row().center_y().gap(4.0).child(icon(&icons::CLOCK, 13.0, YELLOW)).child(dim(who.clone())));
            }
            if pr.reviews.is_empty() && pr.requested.is_empty() {
                rv = rv.child(faint("No reviews yet"));
            }
            meta = meta.child(meta_row("Reviewers", rv, edit_btn("reviewers", PopKind::Reviewers)));
        }

        // ---- tabs (pull requests): the underline slides to the chosen one
        let tabs = d.pr.as_ref().map(|pr| {
            let tab = |id: &str, name: String, which: DetailTab| {
                let on = ds.tab == which;
                col()
                    .key(format!("gh-dtab-{id}"))
                    .h(30.0)
                    .px(12.0)
                    .justify_center()
                    .cursor(Cursor::Pointer)
                    .role("tab")
                    .child(label(name, 12.5, if on { theme::TEXT } else { theme::TEXT_DIM }))
                    .child(el().pin(Some(0.0), None, Some(0.0), Some(0.0)).h(2.0).fill(rgb(if on { theme::ACCENT } else { theme::PANEL_BG })).motion(motion::SNAP))
                    .on_press_tx(go(&self.id, GhAct::Tab(which)))
            };
            row()
                .w_full()
                .border_sides([0.0, 0.0, 1.0, 0.0], rgb(theme::BORDER))
                .child(tab("conv", "Conversation".into(), DetailTab::Conversation))
                .child(tab("commits", format!("Commits {}", pr.commits), DetailTab::Commits))
                .child(tab("files", format!("Files changed {}", pr.files), DetailTab::Files))
        });

        let content = if ds.partial {
            skeleton()
        } else {
            match ds.tab {
                DetailTab::Conversation => self.render_conversation(d, ds, t),
                DetailTab::Commits => self.render_commits(d, t),
                DetailTab::Files => self.render_files(ds),
            }
        };
        let mut scroll = col().key("gh-detail").w_full().grow(1.0).shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(el().w_full().px(16.0).py(12.0).child(meta));
        if let Some(t) = tabs {
            scroll = scroll.child(t);
        }
        scroll = scroll.child(col().key(format!("gh-tab-body-{:?}", ds.tab)).w_full().px(16.0).py(12.0).transition(Transition::opacity()).child(content));
        let mut c = col().w_full().h_full();
        if let Some(b) = back {
            c = c.child(b);
        }
        c.child(header).child(scroll)
    }

    pub(super) fn reactions_row(&self, subject: &str, list: &[Reaction], can_react: bool) -> El {
        let mut r = row().w_full().flex_wrap().center_y().gap(6.0);
        for re in list.iter().filter(|r| r.count > 0) {
            let act = GhAct::React(subject.to_string(), re.content.clone(), re.mine);
            r = r.child(
                row()
                    .key(format!("gh-react-{subject}-{}", re.content))
                    .h(22.0)
                    .px(8.0)
                    .gap(4.0)
                    .center_y()
                    .radius(11.0)
                    .cursor(Cursor::Pointer)
                    .role("button")
                    .border(1.0, rgb(if re.mine { theme::ACCENT } else { CARD_BORDER }))
                    .fill(rgb(if re.mine { 0x2a211d } else { CARD_HEAD }))
                    .hover(Color::WHITE.alpha(0.05))
                    .press_scale(0.94)
                    .child(label(format!("{} {}", emoji(&re.content), re.count), 12.0, theme::TEXT))
                    .on_press_tx(go(&self.id, act)),
            );
        }
        if can_react {
            let add = row().h(22.0).px(8.0).center_y().radius(11.0).cursor(Cursor::Pointer).role("button").hover(Color::WHITE.alpha(0.05)).child(icon(&icons::HEART_PLUS, 14.0, theme::TEXT_FAINT));
            r = r.child(popup_trigger(add, &self.id, format!("gh-addreact-{subject}"), PopKind::React(subject.to_string())));
        }
        r
    }

    /// One comment-like card: header with author and time, markdown body (or the editor), reactions.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn card(
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
    ) -> El {
        let editing = edit_target.is_some() && self.editing == edit_target;
        let mut who = row()
            .center_y()
            .gap(8.0)
            .shrink()
            .min_w(0.0)
            .child(avatar(author, 20.0))
            .child(text(author.to_string()).font(font(13.0, 600.0)).color(rgb(theme::TEXT)))
            .child(dim(format!("{verb} {}{}", ago(when, t), if edited { " · edited" } else { "" })).truncate().shrink());
        if let Some(a) = assoc_label(assoc) {
            who = who.child(row().px(6.0).radius(8.0).border(1.0, rgb(CARD_BORDER)).child(label(a, 10.5, theme::TEXT_DIM)));
        }
        let mut head = row().w_full().h(36.0).px(12.0).gap(8.0).center_y().justify_between().fill(rgb(CARD_HEAD)).border_sides([0.0, 0.0, 1.0, 0.0], rgb(CARD_BORDER)).child(who);
        if let Some(m) = menu {
            head = head.child(popup_trigger(icon_button(&icons::ELLIPSIS), &self.id, format!("gh-card-menu-{key}"), m));
        }
        let body_el = if editing {
            col()
                .w_full()
                .gap(8.0)
                .child(gh_input(&self.id, &format!("gh-edit-body-{key}"), &self.edit_body, |v| &mut v.edit_body, "", true, None))
                .child(
                    row()
                        .w_full()
                        .justify_end()
                        .gap(6.0)
                        .child(button("Cancel", false).key(format!("gh-edit-cancel-{key}")).on_press_tx(go(&self.id, GhAct::EditCancel)))
                        .child(button("Save", true).key(format!("gh-edit-save-{key}")).on_press_tx(go(&self.id, GhAct::EditSave))),
                )
        } else if body.trim().is_empty() {
            faint("No description provided.")
        } else {
            col().w_full().child(markdown(body))
        };
        let mut inner = col().w_full().padding(12.0).gap(10.0).child(body_el);
        if let Some((subject, list)) = reactions {
            inner = inner.child(self.reactions_row(subject, list, true));
        }
        col().key(format!("gh-card-{key}")).w_full().radius(8.0).border(1.0, rgb(CARD_BORDER)).fill(rgb(CARD_BG)).clip_rounded(8.0).motion(motion::REFLOW).child(head).child(inner)
    }

    pub(super) fn render_conversation(&self, d: &Detail, ds: &DetailState, t: i64) -> El {
        let it = &d.item;
        let mut c = col().w_full().gap(12.0);
        let body_edit = (d.can_update || d.mine).then_some(Edit::Body);
        c = c.child(self.card(
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
        ));
        for (n, e) in d.timeline.iter().enumerate() {
            match e {
                Entry::Comment(cm) => {
                    let menu = PopKind::Comment(cm.id.clone(), cm.can_edit, cm.can_delete, format!("{}#issuecomment-{}", it.url, cm.id));
                    c = c.child(self.card(&cm.id, &cm.author, &cm.assoc, "commented", cm.created, cm.edited, &cm.body, Some((&cm.id, &cm.reactions)), Some(menu), cm.can_edit.then(|| Edit::Comment(cm.id.clone())), t));
                }
                Entry::Review(r) => c = c.child(self.render_review(r, n, t)),
                Entry::Event(ev) => {
                    let mut r = row()
                        .w_full()
                        .flex_wrap()
                        .center_y()
                        .gap(6.0)
                        .px(4.0)
                        .child(label("•", 12.0, theme::TEXT_FAINT))
                        .child(text(ev.actor.clone()).font(font(12.0, 500.0)).color(rgb(theme::TEXT_DIM)))
                        .child(dim(ev.what.clone()));
                    if let Some(l) = &ev.label {
                        r = r.child(label_pill(l));
                    }
                    c = c.child(r.child(faint(ago(ev.created, t))));
                }
                Entry::Commit(cm) => c = c.child(self.render_commit_line(cm, t)),
            }
        }
        if d.next.is_some() {
            c = c.child(row().child(button(if ds.more_loading { "Loading…" } else { "Load more activity" }, false).key("gh-more-timeline").on_press_tx(go(&self.id, GhAct::LoadMoreTimeline))));
        }
        if let Some(pr) = &d.pr {
            c = c.child(self.render_merge_box(d, pr));
        }
        c.child(self.render_composer(d))
    }

    pub(super) fn render_commit_line(&self, c: &CommitEntry, t: i64) -> El {
        row()
            .w_full()
            .center_y()
            .gap(8.0)
            .px(4.0)
            .child(icon(&icons::GIT_COMMIT_HORIZONTAL, 14.0, theme::TEXT_FAINT))
            .child(mono(c.short.clone(), 11.5, theme::ACCENT))
            .child(label(c.headline.clone(), 13.0, theme::TEXT).truncate().grow(1.0).shrink())
            .child(faint(format!("{} · {}", c.author, ago(c.created, t))))
    }

    pub(super) fn render_commits(&self, d: &Detail, t: i64) -> El {
        let mut c = col().w_full().gap(8.0);
        let commits: Vec<&CommitEntry> = d.timeline.iter().filter_map(|e| if let Entry::Commit(c) = e { Some(c) } else { None }).collect();
        for cm in &commits {
            c = c.child(self.render_commit_line(cm, t));
        }
        if commits.is_empty() {
            c = c.child(faint("No commits loaded."));
        }
        if d.next.is_some() {
            c = c.child(faint("More activity exists: open Conversation and load more to see later commits."));
        }
        c
    }

    pub(super) fn render_review(&self, r: &Review, n: usize, t: i64) -> El {
        let (verb, color) = match r.state.as_str() {
            "APPROVED" => ("approved these changes", GREEN),
            "CHANGES_REQUESTED" => ("requested changes", RED),
            "DISMISSED" => ("had a review dismissed", GRAY),
            "PENDING" => ("has a pending review", YELLOW),
            _ => ("reviewed", GRAY),
        };
        let mut c = col()
            .key(format!("gh-review-{n}"))
            .w_full()
            .radius(8.0)
            .border(1.0, rgb(color).alpha(0.5))
            .fill(rgb(CARD_BG))
            .clip_rounded(8.0)
            .child(
                row()
                    .w_full()
                    .h(36.0)
                    .px(12.0)
                    .gap(8.0)
                    .center_y()
                    .fill(rgb(CARD_HEAD))
                    .child(avatar(&r.author, 20.0))
                    .child(text(r.author.clone()).font(font(13.0, 600.0)).color(rgb(theme::TEXT)))
                    .child(label(verb, 13.0, color))
                    .child(faint(ago(r.created, t))),
            );
        if !r.body.trim().is_empty() {
            c = c.child(col().w_full().padding(12.0).child(markdown(&r.body)));
        }
        for cm in &r.comments {
            let hunk: Vec<&str> = cm.hunk.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
            let mut code = col().w_full().fill(rgb(0x0e0e0f)).py(4.0).clip_rounded(0.0);
            for l in hunk {
                let bg = if l.starts_with('+') { Some(GREEN) } else if l.starts_with('-') { Some(RED) } else { None };
                let mut line = row().w_full().px(10.0).child(mono(if l.is_empty() { " ".to_string() } else { l.to_string() }, 11.5, theme::TEXT));
                if let Some(b) = bg {
                    line = line.fill(rgb(b).alpha(0.14));
                }
                code = code.child(line);
            }
            let mut place = row().w_full().gap(8.0).px(12.0).py(6.0).child(mono(format!("{}{}", cm.path, cm.line.map(|l| format!(":{l}")).unwrap_or_default()), 11.5, theme::TEXT_DIM));
            if cm.outdated {
                place = place.child(faint("outdated"));
            }
            c = c.child(
                col()
                    .w_full()
                    .border_sides([1.0, 0.0, 0.0, 0.0], rgb(CARD_BORDER))
                    .child(place)
                    .child(code)
                    .child(col().w_full().padding(12.0).child(markdown(&format!("**{}**: {}", cm.author, cm.body)))),
            );
        }
        c
    }
}
