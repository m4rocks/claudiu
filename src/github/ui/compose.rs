//! The write side of the detail view: merge box, comment composer and the new-item form.

use super::*;

impl GithubView {
    pub(super) fn render_merge_box(&self, d: &Detail, pr: &Pr) -> El {
        let it = &d.item;
        let info = self.info.clone().unwrap_or_default();
        let line = |glyph: &'static rune::Icon, color: u32, title: String, sub: Option<String>| {
            let mut words = col().grow(1.0).shrink().child(text(title).font(font(13.0, 500.0)).color(rgb(theme::TEXT)));
            if let Some(s) = sub {
                words = words.child(faint(s).wrap());
            }
            row().w_full().gap(10.0).child(el().w(16.0).child(icon(glyph, 16.0, color))).child(words)
        };
        let mut rows = col().w_full().gap(10.0);
        let mut actions = row().w_full().flex_wrap().center_y().gap(8.0);
        let outline = match it.state {
            State::Merged => PURPLE,
            State::Closed => RED,
            State::Open => CARD_BORDER,
        };
        let id = &self.id;
        match it.state {
            State::Merged => {
                rows = rows.child(line(&icons::GIT_MERGE, PURPLE, "Pull request merged".into(), pr.merged_by.clone().map(|b| format!("Merged by {b}"))));
                let branch_gone = d.timeline.iter().any(|e| matches!(e, Entry::Event(ev) if ev.what.starts_with("deleted the")));
                if !pr.cross_repo && !branch_gone && !it.head.is_empty() && info.can_write() && !info.delete_branch_on_merge {
                    actions = actions.child(button(format!("Delete {}", it.head), false).key("gh-del-branch").on_press_tx(go(id, GhAct::DeleteBranch)));
                }
            }
            State::Closed => {
                rows = rows.child(line(&icons::GIT_PULL_REQUEST_CLOSED, RED, "This pull request is closed".into(), None));
                actions = actions.child(button("Reopen pull request", false).key("gh-reopen-pr").on_press_tx(go(id, GhAct::ReopenItem)));
            }
            State::Open => {
                // reviews
                match it.review.as_deref() {
                    Some("APPROVED") => rows = rows.child(line(&icons::CIRCLE_CHECK, GREEN, "Changes approved".into(), None)),
                    Some("CHANGES_REQUESTED") => rows = rows.child(line(&icons::CIRCLE_X, RED, "Changes requested".into(), Some("A reviewer asked for changes before this can merge.".into()))),
                    Some("REVIEW_REQUIRED") => rows = rows.child(line(&icons::CIRCLE_DOT, YELLOW, "Review required".into(), Some("At least one approving review is required.".into()))),
                    _ => {}
                }
                // checks
                if !pr.checks.is_empty() {
                    let bad = pr.checks.iter().filter(|c| check_color(&c.result) == RED).count();
                    let wait = pr.checks.iter().filter(|c| check_color(&c.result) == YELLOW).count();
                    let ok = pr.checks.len() - bad - wait;
                    let (g, c, title) = if bad > 0 {
                        (&icons::CIRCLE_X, RED, format!("{bad} failing"))
                    } else if wait > 0 {
                        (&icons::CIRCLE_DOT, YELLOW, format!("{wait} pending"))
                    } else {
                        (&icons::CIRCLE_CHECK, GREEN, "All checks have passed".to_string())
                    };
                    let mut r = col().w_full().gap(4.0).child(line(g, c, title, Some(format!("{ok} successful, {bad} failing, {wait} pending"))));
                    for ck in pr.checks.iter().take(12) {
                        let url = ck.url.clone();
                        let cc = check_color(&ck.result);
                        r = r.child(
                            row()
                                .key(format!("gh-check-{}", ck.name))
                                .ml(26.0)
                                .gap(6.0)
                                .center_y()
                                .cursor(Cursor::Pointer)
                                .role("link")
                                .child(icon(check_icon(cc), 13.0, cc))
                                .child(label(ck.name.clone(), 12.0, theme::TEXT_DIM))
                                .on_press_tx(go(id, GhAct::OpenWeb(url))),
                        );
                    }
                    rows = rows.child(r);
                }
                // conflicts / behind
                if pr.mergeable == "CONFLICTING" {
                    rows = rows.child(line(&icons::CIRCLE_X, RED, "This branch has conflicts".into(), Some("Resolve them before merging (ask Claude, or check the branch out).".into())));
                } else if pr.merge_state == "BEHIND" && info.can_write() {
                    rows = rows.child(line(&icons::CIRCLE_DOT, YELLOW, "This branch is out of date with the base branch".into(), None));
                    actions = actions.child(button("Update branch", false).key("gh-update-branch").on_press_tx(go(id, GhAct::UpdateBranch)));
                }
                if let Some((who, method)) = &pr.auto_merge {
                    rows = rows.child(line(&icons::CLOCK, PURPLE, format!("Auto-merge enabled by {who}"), Some(format!("Will merge automatically ({}) once requirements pass.", method.to_lowercase()))));
                    actions = actions.child(button("Disable auto-merge", false).key("gh-auto-off").on_press_tx(go(id, GhAct::AutoMerge(false))));
                }
                if it.draft {
                    rows = rows.child(line(&icons::GIT_PULL_REQUEST_DRAFT, GRAY, "This pull request is still a work in progress".into(), Some("Draft pull requests cannot be merged.".into())));
                    if info.can_write() || d.mine {
                        actions = actions.child(button("Ready for review", true).key("gh-ready").on_press_tx(go(id, GhAct::Ready)));
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
                    if let Some((m, l)) = method {
                        let btn_label = if blocked && pr.can_admin_merge {
                            format!("{l} (override)")
                        } else if unstable {
                            format!("{l} anyway")
                        } else {
                            l.to_string()
                        };
                        if blocked && !pr.can_admin_merge {
                            actions = actions.child(button_disabled("Merging is blocked").key("gh-merge-disabled"));
                            if info.allow_auto_merge && pr.can_auto_merge && pr.auto_merge.is_none() {
                                actions = actions.child(button("Enable auto-merge", false).key("gh-auto-on").on_press_tx(go(id, GhAct::AutoMerge(true))));
                            }
                        } else {
                            let main = if blocked || unstable { danger_button(btn_label) } else { button(btn_label, true) };
                            let mut split = row().gap(2.0).child(main.key("gh-merge").on_press_tx(go(id, GhAct::Merge(m))));
                            if allowed.len() > 1 {
                                split = split.child(popup_trigger(icon_button(&icons::CHEVRON_DOWN), id, "gh-merge-pick".into(), PopKind::Merge));
                            }
                            actions = actions.child(split);
                        }
                    } else {
                        actions = actions.child(faint("This repository does not allow merging pull requests."));
                    }
                    if blocked {
                        rows = rows.child(line(&icons::CIRCLE_DOT, YELLOW, "Merging is blocked".into(), Some("Required reviews or checks have not been met.".into())));
                    } else if pr.mergeable == "MERGEABLE" {
                        rows = rows.child(line(&icons::CIRCLE_CHECK, GREEN, "No conflicts with the base branch".into(), Some("Merging can be performed automatically.".into())));
                    }
                }
            }
        }
        col().key("gh-merge-box").w_full().radius(8.0).border(1.0, rgb(outline)).fill(rgb(CARD_BG)).padding(14.0).gap(12.0).motion(motion::REFLOW).child(rows).child(actions)
    }

    // ------------------------------------------------------------------------------------------------ composer

    pub(super) fn render_composer(&self, d: &Detail) -> El {
        let it = &d.item;
        let id = &self.id;
        let open = it.state == State::Open;
        let can_triage = self.info.as_ref().is_some_and(|i| i.can_triage());
        let can_close = can_triage || d.mine;
        if d.locked && !can_triage {
            return el().padding(12.0).child(faint("This conversation is locked."));
        }
        let mut buttons = row().w_full().flex_wrap().justify_end().gap(6.0);
        if it.kind == Kind::Pull && open && !d.mine {
            buttons = buttons
                .child(button("Approve", false).key("gh-approve").on_press_tx(go(id, GhAct::Review("APPROVE"))))
                .child(button("Request changes", false).key("gh-changes").on_press_tx(go(id, GhAct::Review("REQUEST_CHANGES"))));
        }
        if can_close && it.state != State::Merged {
            if open {
                if it.kind == Kind::Issue {
                    buttons = buttons
                        .child(button("Close issue", false).key("gh-close").on_press_tx(go(id, GhAct::Comment(Some(true)))))
                        .child(popup_trigger(icon_button(&icons::CHEVRON_DOWN), id, "gh-close-reason".into(), PopKind::CloseReason));
                } else {
                    buttons = buttons.child(button("Close pull request", false).key("gh-close").on_press_tx(go(id, GhAct::Comment(Some(true)))));
                }
            } else if it.kind == Kind::Issue {
                buttons = buttons.child(button("Reopen issue", false).key("gh-reopen").on_press_tx(go(id, GhAct::Comment(Some(false)))));
            }
        }
        buttons = buttons.child(button("Comment", true).key("gh-comment").on_press_tx(go(id, GhAct::Comment(None))));
        col()
            .w_full()
            .gap(8.0)
            .child(section_label("ADD A COMMENT"))
            .child(gh_input(id, "gh-composer", &self.composer, |v| &mut v.composer, "Leave a comment (Markdown)", true, None))
            .child(buttons)
    }

    // ------------------------------------------------------------------------------------------------ new item

    pub(super) fn render_new(&self, kind: Kind, stacked: bool) -> El {
        let id = &self.id;
        let can_triage = self.info.as_ref().is_some_and(|i| i.can_triage());
        let mut meta = col().w_full().gap(8.0);
        if kind == Kind::Issue && can_triage {
            let mut labels = row().flex_wrap().gap(5.0).grow(1.0).shrink();
            for l in &self.draft_labels {
                labels = labels.child(label_pill(l));
            }
            let mut people = row().flex_wrap().gap(8.0).grow(1.0).shrink();
            for u in &self.draft_assignees {
                people = people.child(dim(u.login.clone()));
            }
            meta = meta
                .child(row().w_full().center_y().gap(10.0).child(el().w(84.0).child(section_label("LABELS"))).child(labels).child(popup_trigger(button("＋ Add", false), id, "gh-new-labels".into(), PopKind::Labels)))
                .child(row().w_full().center_y().gap(10.0).child(el().w(84.0).child(section_label("ASSIGNEES"))).child(people).child(popup_trigger(button("＋ Add", false), id, "gh-new-assignees".into(), PopKind::Assignees)));
        }
        let field = |name: &str, e: El| col().w_full().gap(4.0).child(section_label(name.to_string())).child(e);
        let mut form = col().w_full().gap(12.0).padding(16.0).child(text(if kind == Kind::Issue { "New issue" } else { "New pull request" }).font(font(16.0, 600.0)).color(rgb(theme::TEXT)));
        if kind == Kind::Pull {
            form = form.child(
                row()
                    .w_full()
                    .gap(10.0)
                    .child(col().grow(1.0).shrink().child(field("MERGE BRANCH", gh_input(id, "gh-new-head", &self.new_head, |v| &mut v.new_head, "Branch with your changes", false, None))))
                    .child(col().grow(1.0).shrink().child(field("INTO", gh_input(id, "gh-new-base", &self.new_base, |v| &mut v.new_base, "Branch to merge into", false, None)))),
            );
        }
        form = form
            .child(field("TITLE", gh_input(id, "gh-new-title", &self.new_title, |v| &mut v.new_title, "Title", false, None)))
            .child(field("DESCRIPTION", gh_input(id, "gh-new-body", &self.new_body, |v| &mut v.new_body, "Describe it (Markdown)", true, None)))
            .child(meta)
            .child(
                row()
                    .w_full()
                    .justify_end()
                    .gap(8.0)
                    .child(button("Cancel", false).key("gh-new-cancel").on_press_tx(go(id, GhAct::Back)))
                    .child(button(if kind == Kind::Issue { "Create issue" } else { "Create pull request" }, true).key("gh-new-submit").on_press_tx(go(id, GhAct::SubmitNew))),
            );
        let mut c = col().w_full().h_full();
        if stacked {
            c = c.child(row().px(12.0).pt(8.0).child(button_leading(&icons::ARROW_LEFT, "Back to list", false).key("gh-back").on_press_tx(go(id, GhAct::Back))));
        }
        c.child(col().key("gh-new-scroll").w_full().grow(1.0).shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(form))
    }
}
