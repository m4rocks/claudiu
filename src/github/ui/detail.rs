//! The detail side of the GitHub pane: header, conversation, commits and reviews of one item.

use super::*;

impl GithubView {
    pub(super) fn render_detail(&self, stacked: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Mode::New(kind) = self.mode {
            return self.render_new(kind, stacked, window, cx).into_any_element();
        }
        let Some(ds) = &self.detail else { return self.render_empty_detail().into_any_element() };
        let back = stacked.then(|| back_button(cx));
        let Some(d) = &ds.data else {
            let msg = match &ds.error {
                Some(e) => failed(e.clone()).into_any_element(),
                None => skeleton(),
            };
            return div().size_full().flex().flex_col().children(back).child(div().p(px(16.0)).child(msg)).into_any_element();
        };
        let t = now();
        let can_triage = self.info.as_ref().is_some_and(|i| i.can_triage());
        let it = &d.item;
        let editing_title = self.editing == Some(Edit::Title);

        let title_row = if editing_title {
            div()
                .flex()
                .gap(px(8.0))
                .items_center()
                .child(div().flex_1().child(Input::new(&self.edit_title).h(px(32.0))))
                .child(btn::text("gh-title-save", "Save", true).on_click(go(cx, GhAct::EditSave)))
                .child(btn::text("gh-title-cancel", "Cancel", false).on_click(go(cx, GhAct::EditCancel)))
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
                        .text_size(crate::panels::scale::px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_normal().child(it.title.clone()))
                        .child(div().text_color(theme().subtle_foreground).font_weight(FontWeight::NORMAL).child(format!("#{}", it.number))),
                )
                .when(d.can_update || d.mine, |r| r.child(icon_action("gh-edit-title", IconName::Pencil, "Edit title").on_click(go(cx, GhAct::EditStart(Edit::Title)))))
        };

        let mut status = div().flex().flex_wrap().items_center().gap(px(8.0)).child(state_badge(it));
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
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .font_family(monospace_font())
                    .text_size(crate::panels::scale::px(11.5))
                    .px(px(7.0))
                    .h(px(22.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(theme().border)
                    .bg(theme().inset)
                    .text_color(theme().muted_foreground)
                    .child(icon(IconName::GitBranch, 12.0))
                    .child(format!("{}{} → {}", if pr.cross_repo { format!("{}:", pr.head_repo.split('/').next().unwrap_or("")) } else { String::new() }, it.head, it.base)),
            );
            status = status.child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .text_size(crate::panels::scale::px(12.0))
                    .child(div().text_color(theme().state_open).child(format!("+{}", pr.additions)))
                    .child(div().text_color(theme().state_closed).child(format!("−{}", pr.deletions))),
            );
        }

        let url = it.url.clone();
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(px(6.0))
            // Claude would work in the folder, which is not a clone of a repository named from elsewhere.
            .when(self.named.is_none(), |d| d.child(btn::text("gh-claude", "Ask agent", true).child(icon(IconName::ChevronDown, 12.0)).popup(cx, PopKind::Claude)))
            .child(btn::button("gh-open-web", Some(IconName::ExternalLink), "Open on GitHub", Variant::Outline, Size::Sm).on_click(go(cx, GhAct::OpenWeb(url))))
            .child(btn::button("gh-more-menu", Some(IconName::Ellipsis), "", Variant::Outline, Size::Sm).w(px(26.0)).px(px(0.0)).justify_center().tooltip(tooltip("More actions")).popup(cx, PopKind::More))
            .when(ds.loading, |d| d.child(div().flex().items_center().gap(px(5.0)).text_color(theme().muted_foreground).child(crate::panels::ui::spinner::spinner("gh-refreshing", 12.0)).child(dim("Refreshing…"))));

        let header = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(10.0))
            .px(px(16.0))
            .pt(px(14.0))
            .pb(px(12.0))
            .border_b_1()
            .border_color(theme().border)
            .child(title_row)
            .child(status)
            .child(actions);

        // ---- meta: labels, assignees, milestone, reviewers
        let edit_btn = |id: &str, kind: PopKind, cx: &Context<Self>| {
            can_triage.then(|| {
                btn::button(SharedString::from(format!("gh-meta-{id}")), Some(IconName::Pencil), "Edit", Variant::Ghost, Size::Xs)
                    .text_color(theme().muted_foreground)
                    .popup(cx, kind)
            })
        };
        let meta_row = |name: &str, value: Div, edit: Option<gpui_kit::base::Button>| {
            div()
                .flex()
                .items_start()
                .gap(px(10.0))
                .min_h(px(22.0))
                .child(div().w(px(84.0)).flex_none().pt(px(3.0)).child(section_label(name.to_uppercase())))
                .child(div().flex_1().min_w_0().pt(px(1.0)).child(value))
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
                    "APPROVED" => (IconName::CircleCheck, theme().state_open),
                    "CHANGES_REQUESTED" => (IconName::CircleX, theme().state_closed),
                    _ => (IconName::CircleDot, theme().state_neutral),
                };
                rv = rv.child(div().flex().items_center().gap(px(4.0)).child(div().text_color(c).child(icon(g, 13.0))).child(dim(who.clone())));
            }
            for who in pr.requested.iter().filter(|w| !pr.reviews.iter().any(|(r, _)| r == *w)) {
                rv = rv.child(div().flex().items_center().gap(px(4.0)).child(div().text_color(theme().state_pending).child(icon(IconName::Clock, 13.0))).child(dim(who.clone())));
            }
            if pr.reviews.is_empty() && pr.requested.is_empty() {
                rv = rv.child(faint("No reviews yet"));
            }
            meta = meta.child(meta_row("Reviewers", rv, edit_btn("reviewers", PopKind::Reviewers, cx)));
        }

        // ---- tabs (pull requests)
        let tabs = d.pr.as_ref().map(|pr| {
            let tab = |id: &str, name: &str, count: Option<u32>, which: DetailTab, cx: &mut Context<Self>| {
                tabs::line_trigger(SharedString::from(format!("gh-dtab-{id}")), name.to_string(), ds.tab == which)
                    .children(count.map(tabs::count))
                    .on_click(go(cx, GhAct::Tab(which)))
            };
            tabs::line_list()
                .child(tab("conv", "Conversation", None, DetailTab::Conversation, cx))
                .child(tab("commits", "Commits", Some(pr.commits), DetailTab::Commits, cx))
                .child(tab("files", "Files changed", Some(pr.files), DetailTab::Files, cx))
        });

        let content: AnyElement = if ds.partial { skeleton() } else { match ds.tab {
            DetailTab::Conversation => self.render_conversation(d, ds, t, cx).into_any_element(),
            DetailTab::Commits => self.render_commits(d, t).into_any_element(),
            DetailTab::Files => self.render_files(ds, window, cx).into_any_element(),
        } };
        let scroll = self.scrolls[1]
            .wire(div().id("gh-detail").flex_1().min_h_0(), window)
            .child(div().px(px(16.0)).py(px(12.0)).child(card::card().child(card::content().gap(px(8.0)).child(meta))))
            .children(tabs)
            .child(div().px(px(16.0)).py(px(12.0)).child(content));
        div().size_full().flex().flex_col().children(back).child(header).child(scroll).into_any_element()
    }

    pub(super) fn reactions_row(&self, subject: &str, list: &[Reaction], can_react: bool, cx: &mut Context<Self>) -> Div {
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
                    .text_size(crate::panels::scale::px(12.0))
                    .border_1()
                    .border_color(if r.mine { theme().primary } else { theme().border })
                    .bg(if r.mine { theme().current } else { theme().card_header })
                    .hover(|s| s.bg(theme().secondary_hover))
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
                    .text_size(crate::panels::scale::px(12.0))
                    .text_color(theme().subtle_foreground)
                    .hover(|s| s.bg(theme().secondary_hover).text_color(theme().foreground))
                    .child(icon(IconName::HeartPlus, 14.0))
                    .popup(cx, PopKind::React(subject.to_string())),
            );
        }
        row
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
        cx: &mut Context<Self>,
    ) -> Div {
        let editing = edit_target.is_some() && self.editing == edit_target;
        let head = card::header()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .min_w_0()
                    .child(avatar(author, 20.0))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(author.to_string()))
                    .child(dim(format!("{verb} {}{}", ago(when, t), if edited { " · edited" } else { "" })))
                    .children(assoc_label(assoc).map(|a| badge::badge(a, badge::Variant::Outline))),
            )
            .children(menu.map(|m| icon_action(SharedString::from(format!("gh-card-menu-{key}")), IconName::Ellipsis, "Comment actions").popup(cx, m)));
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
                        .child(btn::text(SharedString::from(format!("gh-edit-cancel-{key}")), "Cancel", false).on_click(go(cx, GhAct::EditCancel)))
                        .child(btn::text(SharedString::from(format!("gh-edit-save-{key}")), "Save", true).on_click(go(cx, GhAct::EditSave))),
                )
                .into_any_element()
        } else if body.trim().is_empty() {
            faint("No description provided.").into_any_element()
        } else {
            div().text_size(crate::panels::scale::px(13.5)).child(markdown(format!("md-{key}"), body)).into_any_element()
        };
        let react = reactions.map(|(subject, list)| self.reactions_row(subject, list, true, cx));
        card::card().child(head).child(card::content().child(body_el).children(react))
    }

    pub(super) fn render_conversation(&self, d: &Detail, ds: &DetailState, t: i64, cx: &mut Context<Self>) -> Div {
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
                    let pending = c.id.starts_with("pending-");
                    let menu = PopKind::Comment(c.id.clone(), c.can_edit, c.can_delete, format!("{}#issuecomment-{}", it.url, c.id));
                    col = col.child(self.card(
                        &c.id,
                        &c.author,
                        &c.assoc,
                        if pending && c.assoc == "Sending…" { "sending" } else { "commented" },
                        c.created,
                        c.edited,
                        &c.body,
                        (!pending).then_some((&c.id, &c.reactions)),
                        (!pending).then_some(menu),
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
                            .child(div().text_color(theme().subtle_foreground).child("•"))
                            .child(div().text_size(crate::panels::scale::px(12.0)).font_weight(FontWeight::MEDIUM).text_color(theme().muted_foreground).child(ev.actor.clone()))
                            .child(dim(ev.what.clone()))
                            .children(ev.label.as_ref().map(label_pill))
                            .child(faint(ago(ev.created, t))),
                    );
                }
                Entry::Commit(c) => col = col.child(self.render_commit_line(c, t)),
            }
        }
        if d.next.is_some() {
            col = col.child(div().child(btn::text("gh-more-timeline", if ds.more_loading { "Loading…" } else { "Load more activity" }, false).on_click(go(cx, GhAct::LoadMoreTimeline))));
        }
        if let Some(pr) = &d.pr {
            col = col.child(self.render_merge_box(d, pr, cx));
        }
        col.child(self.render_composer(d, cx))
    }

    pub(super) fn render_commit_line(&self, c: &CommitEntry, t: i64) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(4.0))
            .child(div().text_color(theme().subtle_foreground).child(icon(IconName::GitCommitHorizontal, 14.0)))
            .child(div().font_family(monospace_font()).text_size(crate::panels::scale::px(11.5)).text_color(theme().primary).child(c.short.clone()))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(c.headline.clone()))
            .child(faint(format!("{} · {}", c.author, ago(c.created, t))))
    }

    pub(super) fn render_commits(&self, d: &Detail, t: i64) -> Div {
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

    pub(super) fn render_review(&self, r: &Review, n: usize, t: i64, cx: &mut Context<Self>) -> Div {
        let (verb, color) = match r.state.as_str() {
            "APPROVED" => ("approved these changes", theme().state_open),
            "CHANGES_REQUESTED" => ("requested changes", theme().state_closed),
            "DISMISSED" => ("had a review dismissed", theme().state_neutral),
            "PENDING" => ("has a pending review", theme().state_pending),
            _ => ("reviewed", theme().state_neutral),
        };
        let glyph = match r.state.as_str() {
            "APPROVED" => IconName::CircleCheck,
            "CHANGES_REQUESTED" => IconName::CircleX,
            _ => IconName::Eye,
        };
        let mut col = card::outlined(color.opacity(0.5)).child(
            card::header()
                .child(avatar(&r.author, 20.0))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(r.author.clone()))
                .child(div().flex().items_center().gap(px(4.0)).text_color(color).child(icon(glyph, 13.0)).child(verb))
                .child(faint(ago(r.created, t))),
        );
        if !r.body.trim().is_empty() {
            col = col.child(div().p(px(12.0)).text_size(crate::panels::scale::px(13.5)).child(markdown(format!("md-review-{n}"), &r.body)));
        }
        for (k, c) in r.comments.iter().enumerate() {
            let hunk: Vec<&str> = c.hunk.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
            let mut code = div().flex().flex_col().font_family(monospace_font()).text_size(crate::panels::scale::px(11.5)).bg(theme().inset).py(px(4.0)).overflow_hidden();
            for l in hunk {
                let bg = if l.starts_with('+') { Some(theme().diff_added) } else if l.starts_with('-') { Some(theme().diff_removed) } else { None };
                code = code.child(div().px(px(10.0)).whitespace_nowrap().when_some(bg, |d, c| d.bg(c)).child(if l.is_empty() { " ".to_string() } else { l.to_string() }));
            }
            col = col.child(
                div()
                    .border_t_1()
                    .border_color(theme().border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .px(px(12.0))
                            .py(px(6.0))
                            .child(div().text_color(theme().subtle_foreground).child(icon(IconName::FileCode, 13.0)))
                            .child(div().font_family(monospace_font()).text_size(crate::panels::scale::px(11.5)).text_color(theme().muted_foreground).child(format!("{}{}", c.path, c.line.map(|l| format!(":{l}")).unwrap_or_default())))
                            .children(c.outdated.then(|| badge::badge("Outdated", badge::Variant::Outline))),
                    )
                    .child(div().border_t_1().border_b_1().border_color(theme().border).child(code))
                    .child(div().p(px(12.0)).text_size(crate::panels::scale::px(13.5)).child(markdown(format!("md-rc-{n}-{k}"), &format!("**{}**: {}", c.author, c.body)))),
            );
        }
        let _ = cx;
        col
    }

    // ---------------------------------------------------------------------------------------------- merge box
}

/// "Back to list" over a detail when the pane is too narrow for both.
pub(super) fn back_button(cx: &Context<GithubView>) -> Div {
    div()
        .flex_none()
        .flex()
        .px(px(10.0))
        .pt(px(8.0))
        .child(btn::button("gh-back", Some(IconName::ArrowLeft), "Back to list", Variant::Ghost, Size::Sm).text_color(theme().muted_foreground).on_click(go(cx, GhAct::Back)))
}
