//! The write side of the detail view: merge box, comment composer and the new-item form.

use super::*;

impl GithubView {
    pub(super) fn render_merge_box(&self, d: &Detail, pr: &Pr, cx: &mut Context<Self>) -> Div {
        let it = &d.item;
        let info = self.info.clone().unwrap_or_default();
        let line = |glyph: IconName, color: Hsla, title: String, sub: Option<String>| {
            div()
                .flex()
                .items_start()
                .gap(px(10.0))
                .child(div().size(px(28.0)).flex_none().flex().items_center().justify_center().rounded_full().bg(color.opacity(0.14)).text_color(color).child(icon(glyph, 15.0)))
                .child(div().flex().flex_col().gap(px(1.0)).min_h(px(28.0)).justify_center().child(div().font_weight(FontWeight::MEDIUM).child(title)).children(sub.map(faint)))
        };
        let mut rows = div().flex().flex_col().gap(px(10.0));
        let mut actions = div().flex().flex_wrap().items_center().gap(px(8.0));
        let outline = match it.state {
            State::Merged => theme().state_merged,
            State::Closed => theme().state_closed,
            State::Open => theme().border,
        };
        match it.state {
            State::Merged => {
                rows = rows.child(line(IconName::GitMerge, theme().state_merged, "Pull request merged".into(), pr.merged_by.clone().map(|b| format!("Merged by {b}"))));
                let branch_gone = d.timeline.iter().any(|e| matches!(e, Entry::Event(ev) if ev.what.starts_with("deleted the")));
                if !pr.cross_repo && !branch_gone && !it.head.is_empty() && info.can_write() && !info.delete_branch_on_merge {
                    actions = actions.child(btn::text("gh-del-branch", format!("Delete {}", it.head), false).on_click(go(cx, GhAct::DeleteBranch)));
                }
            }
            State::Closed => {
                rows = rows.child(line(IconName::GitPullRequestClosed, theme().state_closed, "This pull request is closed".into(), None));
                actions = actions.child(btn::text("gh-reopen-pr", "Reopen pull request", false).on_click(go(cx, GhAct::ReopenItem)));
            }
            State::Open => {
                // reviews
                match it.review.as_deref() {
                    Some("APPROVED") => rows = rows.child(line(IconName::CircleCheck, theme().state_open, "Changes approved".into(), None)),
                    Some("CHANGES_REQUESTED") => rows = rows.child(line(IconName::CircleX, theme().state_closed, "Changes requested".into(), Some("A reviewer asked for changes before this can merge.".into()))),
                    Some("REVIEW_REQUIRED") => rows = rows.child(line(IconName::CircleDot, theme().state_pending, "Review required".into(), Some("At least one approving review is required.".into()))),
                    _ => {}
                }
                // checks
                if !pr.checks.is_empty() {
                    let bad = pr.checks.iter().filter(|c| check_color(&c.result) == theme().state_closed).count();
                    let wait = pr.checks.iter().filter(|c| check_color(&c.result) == theme().state_pending).count();
                    let ok = pr.checks.len() - bad - wait;
                    let (g, c, title) = if bad > 0 {
                        (IconName::CircleX, theme().state_closed, format!("{bad} failing"))
                    } else if wait > 0 {
                        (IconName::CircleDot, theme().state_pending, format!("{wait} pending"))
                    } else {
                        (IconName::CircleCheck, theme().state_open, "All checks have passed".to_string())
                    };
                    let mut r = line(g, c, title, Some(format!("{} successful, {} failing, {} pending", ok, bad, wait)));
                    for ck in pr.checks.iter().take(12) {
                        let url = ck.url.clone();
                        r = r.child(
                            div()
                                .id(SharedString::from(format!("gh-check-{}", ck.name)))
                                .ml(px(38.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .rounded(px(4.0))
                                .px(px(4.0))

                                .cursor_pointer()
                                .text_size(crate::panels::scale::px(12.0))
                                .hover(|s| s.bg(theme().row_hover).text_color(theme().foreground))
                                .text_color(theme().muted_foreground)
                                .child(div().text_color(check_color(&ck.result)).child(icon(check_icon(check_color(&ck.result)), 13.0)))
                                .child(ck.name.clone())
                                .on_click(go(cx, GhAct::OpenWeb(url))),
                        );
                    }
                    rows = rows.child(r.flex_col());
                }
                // conflicts / behind
                if pr.mergeable == "CONFLICTING" {
                    rows = rows.child(line(IconName::CircleX, theme().state_closed, "This branch has conflicts".into(), Some("Resolve them before merging (ask an agent, or check the branch out).".into())));
                } else if pr.merge_state == "BEHIND" && info.can_write() {
                    rows = rows.child(line(IconName::CircleDot, theme().state_pending, "This branch is out of date with the base branch".into(), None));
                    actions = actions.child(btn::text("gh-update-branch", "Update branch", false).on_click(go(cx, GhAct::UpdateBranch)));
                }
                if let Some((who, method)) = &pr.auto_merge {
                    rows = rows.child(line(IconName::Clock, theme().state_merged, format!("Auto-merge enabled by {who}"), Some(format!("Will merge automatically ({}) once requirements pass.", method.to_lowercase()))));
                    actions = actions.child(btn::text("gh-auto-off", "Disable auto-merge", false).on_click(go(cx, GhAct::AutoMerge(false))));
                }
                if it.draft {
                    rows = rows.child(line(IconName::GitPullRequestDraft, theme().state_neutral, "This pull request is still a work in progress".into(), Some("Draft pull requests cannot be merged.".into())));
                    if info.can_write() || d.mine {
                        actions = actions.child(btn::text("gh-ready", "Ready for review", true).on_click(go(cx, GhAct::Ready)));
                    }
                } else if info.can_write() && pr.mergeable != "CONFLICTING" {
                    let allowed: Vec<(&'static str, &str)> = [("MERGE", "Create a merge commit", info.allow_merge), ("SQUASH", "Squash and merge", info.allow_squash), ("REBASE", "Rebase and merge", info.allow_rebase)]
                        .into_iter()
                        .filter(|(_, _, ok)| *ok)
                        .map(|(m, l, _)| (m, l))
                        .collect();
                    let method = allowed.iter().find(|(m, _)| *m == self.merge_method).or(allowed.first()).copied();
                    let blocked = pr.merge_state == "BLOCKED";
                    let unstable = pr.merge_state == "UNSTABLE";
                    if let Some((m, label)) = method {
                        let btn_label = if blocked && pr.can_admin_merge {
                            format!("{label} (override)")
                        } else if unstable {
                            format!("{label} anyway")
                        } else {
                            label.to_string()
                        };
                        if blocked && !pr.can_admin_merge {
                            actions = actions.child(btn::disabled("gh-merge-disabled", "Merging is blocked"));
                            if info.allow_auto_merge && pr.can_auto_merge && pr.auto_merge.is_none() {
                                actions = actions.child(btn::text("gh-auto-on", "Enable auto-merge", false).on_click(go(cx, GhAct::AutoMerge(true))));
                            }
                        } else {
                            let main = if blocked || unstable { btn::destructive("gh-merge", btn_label) } else { btn::text("gh-merge", btn_label, true) };
                            actions = actions.child(
                                div()
                                    .flex()
                                    .child(main.on_click(go(cx, GhAct::Merge(m))))
                                    .when(allowed.len() > 1, |d| d.child(icon_button("gh-merge-pick", IconName::ChevronDown).ml(px(2.0)).popup(cx, PopKind::Merge))),
                            );
                        }
                    } else {
                        actions = actions.child(faint("This repository does not allow merging pull requests."));
                    }
                    if blocked {
                        rows = rows.child(line(IconName::CircleDot, theme().state_pending, "Merging is blocked".into(), Some("Required reviews or checks have not been met.".into())));
                    } else if pr.mergeable == "MERGEABLE" {
                        rows = rows.child(line(IconName::CircleCheck, theme().state_open, "No conflicts with the base branch".into(), Some("Merging can be performed automatically.".into())));
                    }
                }
            }
        }
        card::outlined(outline).child(card::content().gap(px(14.0)).p(px(14.0)).child(rows).child(actions))
    }

    // ------------------------------------------------------------------------------------------------ composer

    pub(super) fn render_composer(&self, d: &Detail, cx: &mut Context<Self>) -> Div {
        let it = &d.item;
        let open = it.state == State::Open;
        let can_triage = self.info.as_ref().is_some_and(|i| i.can_triage());
        let can_close = can_triage || d.mine;
        if d.locked && !can_triage {
            return alert::alert(alert::Tone::Default, Some("Conversation locked".into()), "Only collaborators can comment on this conversation.");
        }
        let mut buttons = div().flex().flex_wrap().justify_end().gap(px(6.0));
        if it.kind == Kind::Pull && open && !d.mine {
            buttons = buttons
                .child(btn::button("gh-approve", Some(IconName::CircleCheck), "Approve", Variant::Outline, Size::Sm).on_click(go(cx, GhAct::Review("APPROVE"))))
                .child(btn::button("gh-changes", Some(IconName::CircleX), "Request changes", Variant::Outline, Size::Sm).on_click(go(cx, GhAct::Review("REQUEST_CHANGES"))));
        }
        if can_close && it.state != State::Merged {
            if open {
                if it.kind == Kind::Issue {
                    buttons = buttons
                        .child(btn::text("gh-close", "Close issue", false).on_click(go(cx, GhAct::Comment(Some(true)))))
                        .child(icon_action("gh-close-reason", IconName::ChevronDown, "Close as…").popup(cx, PopKind::CloseReason));
                } else {
                    buttons = buttons.child(btn::text("gh-close", "Close pull request", false).on_click(go(cx, GhAct::Comment(Some(true)))));
                }
            } else if it.kind == Kind::Issue {
                buttons = buttons.child(btn::text("gh-reopen", "Reopen issue", false).on_click(go(cx, GhAct::Comment(Some(false)))));
            }
        }
        buttons = buttons.child(btn::text("gh-comment", "Comment", true).on_click(go(cx, GhAct::Comment(None))));
        card::card()
            .child(card::header().child(icon(IconName::MessageSquare, 13.0)).child(div().font_weight(FontWeight::MEDIUM).child("Add a comment")))
            .child(card::content().child(Textarea::new(&self.composer)))
            .child(card::footer().child(buttons))
    }

    // ------------------------------------------------------------------------------------------------- files

    pub(super) fn render_new(&self, kind: Kind, stacked: bool, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let can_triage = self.info.as_ref().is_some_and(|i| i.can_triage());
        let mut meta = div().flex().flex_col().gap(px(8.0));
        if kind == Kind::Issue && can_triage {
            let mut labels = div().flex().flex_wrap().gap(px(5.0));
            for l in &self.draft_labels {
                labels = labels.child(label_pill(l));
            }
            let mut people = div().flex().flex_wrap().gap(px(8.0));
            for u in &self.draft_assignees {
                people = people.child(dim(u.login.clone()));
            }
            meta = meta
                .child(div().flex().items_center().gap(px(10.0)).child(div().w(px(84.0)).child(section_label("LABELS"))).child(labels).child(btn::button("gh-new-labels", Some(IconName::Plus), "Add", Variant::Outline, Size::Xs).popup(cx, PopKind::Labels)))
                .child(div().flex().items_center().gap(px(10.0)).child(div().w(px(84.0)).child(section_label("ASSIGNEES"))).child(people).child(btn::button("gh-new-assignees", Some(IconName::Plus), "Add", Variant::Outline, Size::Xs).popup(cx, PopKind::Assignees)));
        }
        let field = |name: &str, el: AnyElement| div().flex().flex_col().gap(px(4.0)).child(section_label(name.to_string())).child(el);
        let mut form = div().flex().flex_col().gap(px(14.0)).p(px(16.0));
        form = form.child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().text_size(crate::panels::scale::px(16.0)).font_weight(FontWeight::SEMIBOLD).child(if kind == Kind::Issue { "New issue" } else { "New pull request" }))
                .child(dim(if kind == Kind::Issue { "Describe the problem or the idea. Markdown works." } else { "Pick the branch with your changes and the one it merges into." })),
        );
        if kind == Kind::Pull {
            form = form.child(
                div()
                    .flex()
                    .gap(px(10.0))
                    .child(div().flex_1().child(field("MERGE BRANCH", Input::new(&self.new_head).h(px(30.0)).into_any_element())))
                    .child(div().flex_1().child(field("INTO", Input::new(&self.new_base).h(px(30.0)).into_any_element()))),
            );
        }
        form = form
            .child(field("TITLE", Input::new(&self.new_title).h(px(32.0)).into_any_element()))
            .child(field("DESCRIPTION", Textarea::new(&self.new_body).into_any_element()))
            .child(meta)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(btn::text("gh-new-cancel", "Cancel", false).on_click(go(cx, GhAct::Back)))
                    .child(btn::text("gh-new-submit", if kind == Kind::Issue { "Create issue" } else { "Create pull request" }, true).on_click(go(cx, GhAct::SubmitNew))),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .when(stacked, |d| d.child(super::detail::back_button(cx)))
            .child(self.scrolls[2].wire(div().id("gh-new-scroll").flex_1().min_h_0(), window).child(form))
    }

    // ---------------------------------------------------------------------------------------------- pickers
}
