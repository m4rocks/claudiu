//! The GitHub pane: issues and pull requests of a project's repository, driven through the user's `gh` CLI.
//! `cli` runs `gh`, `model` parses its answers (no GPUI), `view` is the pane itself.

pub mod cli;
pub mod model;
mod ui;
pub mod view;

#[cfg(test)]
mod live {
    //! Checks the queries against real GitHub through the developer's own `gh`. `cargo test -- --ignored live`.
    use super::cli::*;
    use super::model::*;
    use serde_json::json;

    fn gh() -> Gh {
        Gh::new(locate().expect("gh is installed"), std::env::current_dir().unwrap())
    }

    const REPO: (&str, &str) = ("zed-industries", "zed");

    #[test]
    #[ignore]
    fn repo_list_and_detail_parse() {
        let gh = gh();
        let own = gh.repo().expect("this checkout has a GitHub remote");
        assert!(!own.owner.is_empty());

        let repo = parse_repo(&gh.graphql(REPO_QUERY, json!({"owner": REPO.0, "name": REPO.1})).unwrap()).unwrap();
        assert_eq!(repo.viewer.is_empty(), false);

        for kind in [Kind::Issue, Kind::Pull] {
            let q = scoped("zed-industries/zed", kind, "is:open");
            let (qa, qb) = (scoped("zed-industries/zed", kind, "is:open"), scoped("zed-industries/zed", kind, "is:closed"));
            let data = gh.graphql(LIST_QUERY, json!({"q": q, "qa": qa, "qb": qb, "after": null})).unwrap();
            let page = parse_page(&data);
            assert!(!page.items.is_empty() && page.open > 0 && page.closed > 0, "{kind:?}: {page:?}");
            let first = &page.items[0];
            let d = gh
                .graphql(detail_query_text(), json!({"owner": REPO.0, "name": REPO.1, "number": first.number, "after": null}))
                .unwrap();
            let detail = parse_detail(&d).expect("detail parses");
            assert_eq!(detail.item.number, first.number);
            assert_eq!(detail.pr.is_some(), kind == Kind::Pull);
        }

        let labels = parse_options_labels(&gh.graphql(LABELS_QUERY, json!({"owner": REPO.0, "name": REPO.1})).unwrap());
        assert!(!labels.is_empty());
        let users = parse_options_users(&gh.graphql(USERS_QUERY, json!({"owner": REPO.0, "name": REPO.1})).unwrap());
        let _ = users;
        let _ = gh.graphql(MILESTONES_QUERY, json!({"owner": REPO.0, "name": REPO.1})).unwrap();
    }

    #[test]
    #[ignore]
    fn pull_files_parse() {
        let gh = gh();
        let repo = RepoRef { owner: REPO.0.into(), name: REPO.1.into(), url: String::new(), host: None };
        let page = parse_page(&gh.graphql(LIST_QUERY, json!({"q": "repo:zed-industries/zed is:pr is:open", "qa": "repo:zed-industries/zed is:pr is:open", "qb": "repo:zed-industries/zed is:pr is:closed", "after": null})).unwrap());
        let files = parse_files(&pull_files(&gh, &repo, page.items[0].number, 1).unwrap());
        assert!(!files.is_empty());
    }
}

#[cfg(test)]
mod render {
    //! Draws the pane headless (gpui's test platform; no window opens) with real GitHub data, to catch panics in
    //! `ui.rs` such as a doubled `hover`. Needs a signed-in `gh`: `cargo test -- --ignored render`.
    use super::cli::*;
    use super::model::*;
    use super::view::*;
    use gpui::{AppContext, TestAppContext};
    use serde_json::json;

    #[test]
    #[ignore]
    fn pane_draws_every_state() {
        let gh = Gh::new(locate().expect("gh is installed"), std::env::current_dir().unwrap());
        let (owner, name) = ("zed-industries", "zed");
        let info = parse_repo(&gh.graphql(REPO_QUERY, json!({"owner": owner, "name": name})).unwrap()).unwrap();
        let mut lists = vec![];
        for kind in [Kind::Issue, Kind::Pull] {
            let (q, qa, qb) = (scoped("zed-industries/zed", kind, "is:open"), scoped("zed-industries/zed", kind, "is:open"), scoped("zed-industries/zed", kind, "is:closed"));
            lists.push(parse_page(&gh.graphql(LIST_QUERY, json!({"q": q, "qa": qa, "qb": qb, "after": null})).unwrap()));
        }
        let detail = |n: u32| parse_detail(&gh.graphql(detail_query_text(), json!({"owner": owner, "name": name, "number": n, "after": null})).unwrap()).unwrap();
        let (issue, pull) = (detail(lists[0].items[0].number), detail(lists[1].items[0].number));
        let repo = RepoRef { owner: owner.into(), name: name.into(), url: String::new(), host: None };
        let files = parse_files(&pull_files(&gh, &repo, pull.item.number, 1).unwrap());

        let mut cx = TestAppContext::single();
        cx.update(|app| {
            gpui_kit::init(app);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
        });
        let (window, view) = cx
            .update(|app| {
                gpui_kit::open_window(Default::default(), app, |window, app| {
                    app.new(|cx| GithubView::new(".".into(), "zed".into(), None, window, cx))
                })
            })
            .unwrap();
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        };
        let set = |cx: &mut TestAppContext, f: &dyn Fn(&mut GithubView)| view.update(cx, |v, _| f(v));

        for phase in [Phase::Loading, Phase::NoCli, Phase::SignedOut, Phase::NotGithub, Phase::Failed("boom".into())] {
            set(&mut cx, &|v| v.phase = phase.clone());
            draw(&mut cx);
        }
        for width in [1100.0, 620.0] {
            set(&mut cx, &|v| {
                v.phase = Phase::Ready;
                v.width = width;
                v.info = Some(info.clone());
                v.repo = Some(repo.clone());
                for (i, p) in lists.iter().enumerate() {
                    let l = &mut v.lists[i];
                    (l.items, l.total, l.open, l.closed, l.loaded) = (p.items.clone(), p.total, p.open, p.closed, true);
                }
            });
            draw(&mut cx);
            for (kind, d) in [(Kind::Issue, &issue), (Kind::Pull, &pull)] {
                for tab in [DetailTab::Conversation, DetailTab::Commits, DetailTab::Files] {
                    set(&mut cx, &|v| {
                        v.kind = kind;
                        let mut s = DetailState::new(kind, d.item.number);
                        (s.data, s.loading, s.tab) = (Some(d.clone()), false, tab);
                        s.files = Some(files.clone());
                        s.expanded = files.iter().map(|f| f.path.clone()).collect();
                        v.detail = Some(s);
                    });
                    draw(&mut cx);
                }
            }
            set(&mut cx, &|v| v.mode = Mode::New(Kind::Issue));
            draw(&mut cx);
            set(&mut cx, &|v| v.mode = Mode::New(Kind::Pull));
            draw(&mut cx);
            set(&mut cx, &|v| v.mode = Mode::Browse);
        }
    }
}

#[cfg(test)]
mod schema {
    //! Sends every mutation with made-up ids: GitHub validates the query text (variable types, field names)
    //! before it looks anything up, so a type mismatch fails here while a good query only reports an unknown
    //! node. Changes nothing. Needs a signed-in `gh`: `cargo test -- --ignored schema`.
    use super::cli::*;
    use serde_json::{Value, json};

    #[test]
    #[ignore]
    fn every_mutation_passes_schema_validation() {
        let gh = Gh::new(locate().expect("gh is installed"), std::env::current_dir().unwrap());
        let oid = "0000000000000000000000000000000000000000";
        let cases: Vec<(&str, &str, Value)> = vec![
            ("M_COMMENT", M_COMMENT, json!({"id": "x", "body": "b"})),
            ("M_EDIT_COMMENT", M_EDIT_COMMENT, json!({"id": "x", "body": "b"})),
            ("M_DELETE_COMMENT", M_DELETE_COMMENT, json!({"id": "x"})),
            ("M_CLOSE_ISSUE", M_CLOSE_ISSUE, json!({"id": "x", "reason": "COMPLETED"})),
            ("M_REOPEN_ISSUE", M_REOPEN_ISSUE, json!({"id": "x"})),
            ("M_CLOSE_PR", M_CLOSE_PR, json!({"id": "x"})),
            ("M_REOPEN_PR", M_REOPEN_PR, json!({"id": "x"})),
            ("M_EDIT_ISSUE", M_EDIT_ISSUE, json!({"id": "x", "title": "t", "body": "b", "milestone": null})),
            ("M_EDIT_PR", M_EDIT_PR, json!({"id": "x", "title": "t", "body": "b", "milestone": null})),
            ("M_ADD_LABEL", M_ADD_LABEL, json!({"id": "x", "ids": ["y"]})),
            ("M_REMOVE_LABEL", M_REMOVE_LABEL, json!({"id": "x", "ids": ["y"]})),
            ("M_ADD_ASSIGNEE", M_ADD_ASSIGNEE, json!({"id": "x", "ids": ["y"]})),
            ("M_REMOVE_ASSIGNEE", M_REMOVE_ASSIGNEE, json!({"id": "x", "ids": ["y"]})),
            ("M_REQUEST_REVIEW", M_REQUEST_REVIEW, json!({"id": "x", "ids": ["y"]})),
            ("M_CREATE_ISSUE", M_CREATE_ISSUE, json!({"repo": "x", "title": "t", "body": "b", "labels": ["y"], "assignees": ["z"]})),
            ("M_CREATE_PR", M_CREATE_PR, json!({"repo": "x", "base": "main", "head": "h", "title": "t", "body": "b", "draft": false})),
            ("M_MERGE", M_MERGE, json!({"id": "x", "method": "SQUASH", "head": oid, "headline": "h", "body": "b"})),
            ("M_AUTO_MERGE", M_AUTO_MERGE, json!({"id": "x", "method": "MERGE"})),
            ("M_NO_AUTO_MERGE", M_NO_AUTO_MERGE, json!({"id": "x"})),
            ("M_READY", M_READY, json!({"id": "x"})),
            ("M_DRAFT", M_DRAFT, json!({"id": "x"})),
            ("M_UPDATE_BRANCH", M_UPDATE_BRANCH, json!({"id": "x"})),
            ("M_REVIEW", M_REVIEW, json!({"id": "x", "event": "APPROVE", "body": "b"})),
            ("M_REACT", M_REACT, json!({"id": "x", "content": "THUMBS_UP"})),
            ("M_UNREACT", M_UNREACT, json!({"id": "x", "content": "THUMBS_UP"})),
            ("M_LOCK", M_LOCK, json!({"id": "x"})),
            ("M_UNLOCK", M_UNLOCK, json!({"id": "x"})),
        ];
        let mut bad = vec![];
        for (name, query, vars) in cases {
            if let Err(e) = gh.graphql(query, vars) {
                // Past validation, the only complaint should be the made-up id.
                if !(e.msg.contains("Could not resolve") || e.msg.contains("NOT_FOUND")) {
                    bad.push(format!("{name}: {}", e.msg));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

#[cfg(test)]
mod e2e {
    //! Every feature of the pane against a real, throwaway private repository on the signed-in account, driven
    //! through the view's own actions (`GithubView::act`) on gpui's test platform, so no window opens.
    //! `cargo test -- --ignored e2e --nocapture`. It CREATES a private repo (named `claudiu-gh-pane-test-<time>`)
    //! and tries to delete it at the end, which needs the `delete_repo` scope; if that is missing, delete it by hand.
    use super::cli::*;
    use super::model::*;
    use super::view::*;
    use gpui::{AnyWindowHandle, AppContext, Entity, Point, TestAppContext, Window};
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Rig {
        cx: TestAppContext,
        window: AnyWindowHandle,
        view: Entity<GithubView>,
        toasts: Rc<RefCell<Vec<(String, bool)>>>,
        _sub: gpui::Subscription,
    }

    impl Rig {
        fn new(dir: std::path::PathBuf, name: &str, gh: Gh) -> Self {
            let cx = TestAppContext::single();
            cx.update(|app| {
                gpui_kit::init(app);
                gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
            });
            let name = name.to_string();
            let (window, view) = cx
                .update(|app| {
                    gpui_kit::open_window(Default::default(), app, |window, app| {
                        app.new(|cx| GithubView::new(dir, name, Some(gh), window, cx))
                    })
                })
                .unwrap();
            let toasts = Rc::new(RefCell::new(vec![]));
            let sink = toasts.clone();
            let _sub = cx.update(|app| {
                app.subscribe(&view, move |_, ev: &GhEvent, _| {
                    if let GhEvent::Toast { text, error } = ev {
                        sink.borrow_mut().push((text.clone(), *error));
                    }
                })
            });
            Rig { cx, window, view, toasts, _sub }
        }

        fn with_window<R>(&mut self, f: impl FnOnce(&mut GithubView, &mut Window, &mut gpui::Context<GithubView>) -> R) -> R {
            let view = self.view.clone();
            self.cx.update_window(self.window, |_, w, app| view.update(app, |v, cx| f(v, w, cx))).unwrap()
        }

        fn read<R>(&self, f: impl FnOnce(&GithubView) -> R) -> R {
            self.view.read_with(&self.cx, |v, _| f(v))
        }

        /// Run until every background request has landed.
        fn settle(&mut self) {
            for _ in 0..60 {
                self.cx.run_until_parked();
                let idle = self.read(|v| {
                    v.busy.is_none()
                        && v.lists.iter().all(|l| !l.loading)
                        && v.detail.as_ref().is_none_or(|d| !d.loading && !d.files_loading && !d.more_loading)
                });
                if idle {
                    // One more pass: a finished request may have started the next one.
                    self.cx.run_until_parked();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            panic!("the pane never went idle");
        }

        /// Do `a`, wait, and assert that an error was shown only if `expect_error`.
        fn act(&mut self, step: &str, a: GhAct, expect_error: bool) {
            self.toasts.borrow_mut().clear();
            self.with_window(|v, w, cx| v.act(a, w, cx));
            self.settle();
            let errors: Vec<String> = self.toasts.borrow().iter().filter(|t| t.1).map(|t| t.0.clone()).collect();
            eprintln!("  {step}{}", if errors.is_empty() { String::new() } else { format!("  -> {errors:?}") });
            if expect_error {
                assert!(!errors.is_empty(), "{step}: expected an error");
            } else {
                assert!(errors.is_empty(), "{step}: {errors:?}");
            }
        }

        fn text(&mut self, which: &str, text: &str) {
            let text = text.to_string();
            let which = which.to_string();
            self.with_window(move |v, w, cx| match which.as_str() {
                "composer" => v.composer.update(cx, |s, cx| s.set_value(text, w, cx)),
                "edit_body" => v.edit_body.update(cx, |s, cx| s.set_value(text, w, cx)),
                "edit_title" => v.edit_title.update(cx, |s, cx| s.set_value(text, w, cx)),
                "new_title" => v.new_title.update(cx, |s, cx| s.set_value(text, w, cx)),
                "new_body" => v.new_body.update(cx, |s, cx| s.set_value(text, w, cx)),
                "new_head" => v.new_head.update(cx, |s, cx| s.set_value(text, w, cx)),
                "new_base" => v.new_base.update(cx, |s, cx| s.set_value(text, w, cx)),
                _ => unreachable!(),
            });
        }

        /// GitHub's search index lags a few seconds behind changes: refresh until `cond` holds.
        fn until(&mut self, what: &str, cond: impl Fn(&GithubView) -> bool) {
            for _ in 0..15 {
                if self.read(&cond) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
                self.act("refresh (waiting for the search index)", GhAct::Refresh, false);
            }
            panic!("never became true: {what}");
        }

        fn detail(&self) -> Detail {
            self.read(|v| v.detail.as_ref().and_then(|d| d.data.clone())).expect("a detail is open")
        }
    }

    fn cmd(exe: &std::path::Path, dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new(exe).args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "gh {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    #[ignore]
    fn every_feature_against_a_scratch_repo() {
        let exe = locate().expect("gh is installed");
        let work = tempfile::tempdir().unwrap();
        let setup = Gh::new(exe.clone(), work.path().to_path_buf());
        let me = setup.rest("GET", "user", None).unwrap()["login"].as_str().unwrap().to_string();
        let name = format!(
            "claudiu-gh-pane-test-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        );
        let full = format!("{me}/{name}");
        eprintln!("scratch repository: {full}");
        setup.rest("POST", "user/repos", Some(json!({"name": name, "private": true, "auto_init": true, "allow_auto_merge": true}))).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(3));

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&exe, work.path(), &setup, &me, &name, &full)));

        match setup.rest("DELETE", &format!("repos/{full}"), None) {
            Ok(_) => eprintln!("scratch repository deleted"),
            Err(e) => eprintln!("could not delete {full} ({}). Delete it by hand: gh repo delete {full} --yes", e.msg),
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    fn run(exe: &std::path::Path, work: &std::path::Path, setup: &Gh, me: &str, name: &str, full: &str) {
        // Branches f1..f5 off main, each adding its own file, so nothing conflicts.
        let main_sha = setup.rest("GET", &format!("repos/{full}/git/ref/heads/main"), None).unwrap()["object"]["sha"].as_str().unwrap().to_string();
        for i in 1..=5 {
            setup.rest("POST", &format!("repos/{full}/git/refs"), Some(json!({"ref": format!("refs/heads/f{i}"), "sha": main_sha}))).unwrap();
            setup
                .rest(
                    "PUT",
                    &format!("repos/{full}/contents/f{i}.txt"),
                    Some(json!({"message": format!("add f{i}"), "content": "aGVsbG8K", "branch": format!("f{i}")})),
                )
                .unwrap();
        }
        setup.rest("POST", &format!("repos/{full}/milestones"), Some(json!({"title": "M1"}))).unwrap();
        setup.rest("PATCH", &format!("repos/{full}"), Some(json!({"allow_auto_merge": true}))).unwrap();
        cmd(exe, work, &["repo", "clone", full, "clone"]);
        let dir = work.join("clone");

        let mut r = Rig::new(dir.clone(), name, Gh::new(exe.to_path_buf(), dir.clone()));
        r.settle();
        assert_eq!(r.read(|v| v.phase.clone()), Phase::Ready, "connected");
        assert!(r.read(|v| v.info.as_ref().is_some_and(|i| i.can_write())), "write access");
        assert_eq!(r.read(|v| v.info.as_ref().map(|i| i.viewer.clone())).as_deref(), Some(me));
        let pos = Point::default();

        eprintln!("issues");
        r.act("new issue form", GhAct::New(Kind::Issue), false);
        r.text("new_title", "Issue A");
        r.text("new_body", "body of A");
        r.act("create issue", GhAct::SubmitNew, false);
        let d = r.detail();
        assert_eq!((d.item.title.as_str(), d.body.as_str(), d.item.state), ("Issue A", "body of A", State::Open));
        let issue_id = d.item.id.clone();
        r.text("composer", "first comment");
        r.act("comment", GhAct::Comment(None), false);
        let first_comment = |r: &Rig| r.detail().timeline.iter().find_map(|e| if let Entry::Comment(c) = e { Some(c.clone()) } else { None });
        let comment = first_comment(&r).expect("comment shown");
        assert_eq!(comment.body, "first comment");
        r.act("edit comment: start", GhAct::EditStart(Edit::Comment(comment.id.clone())), false);
        r.text("edit_body", "edited comment");
        r.act("edit comment: save", GhAct::EditSave, false);
        let edited = first_comment(&r).unwrap();
        assert!(edited.body == "edited comment" && edited.edited);
        r.act("react to comment", GhAct::React(comment.id.clone(), "THUMBS_UP".into(), false), false);
        assert!(first_comment(&r).unwrap().reactions.iter().any(|x| x.content == "THUMBS_UP" && x.mine));
        r.act("react to issue", GhAct::React(issue_id.clone(), "HEART".into(), false), false);
        assert!(r.detail().reactions.iter().any(|x| x.content == "HEART" && x.mine));
        r.act("unreact", GhAct::React(issue_id.clone(), "HEART".into(), true), false);
        assert!(r.detail().reactions.iter().all(|x| x.content != "HEART" || x.count == 0));

        r.act("labels picker", GhAct::Popup(PopKind::Labels, pos), false);
        let label = r.read(|v| v.options.labels.clone()).expect("labels loaded").into_iter().find(|l| l.name == "bug").expect("bug label");
        r.act("add label", GhAct::ToggleLabel(label.clone()), false);
        assert!(r.detail().item.labels.iter().any(|l| l.name == "bug"));
        r.act("remove label", GhAct::ToggleLabel(label.clone()), false);
        assert!(r.detail().item.labels.is_empty());
        r.act("add label again", GhAct::ToggleLabel(label.clone()), false);

        r.act("people picker", GhAct::Popup(PopKind::Assignees, pos), false);
        let me_user = r.read(|v| v.options.users.clone()).expect("users loaded").into_iter().find(|u| u.login == me).expect("me among assignable users");
        r.act("assign me", GhAct::ToggleAssignee(me_user.clone()), false);
        assert!(r.detail().item.assignees.iter().any(|u| u.login == me));
        r.act("unassign", GhAct::ToggleAssignee(me_user.clone()), false);
        assert!(r.detail().item.assignees.is_empty());

        r.act("milestone picker", GhAct::Popup(PopKind::Milestone, pos), false);
        let ms = r.read(|v| v.options.milestones.clone()).expect("milestones loaded").into_iter().next().expect("M1");
        r.act("set milestone", GhAct::SetMilestone(Some(ms.clone())), false);
        assert_eq!(r.detail().item.milestone.as_ref().map(|m| m.1.as_str()), Some("M1"));
        r.act("clear milestone", GhAct::SetMilestone(None), false);
        assert!(r.detail().item.milestone.is_none());

        r.act("edit title: start", GhAct::EditStart(Edit::Title), false);
        r.text("edit_title", "Issue A (renamed)");
        r.act("edit title: save", GhAct::EditSave, false);
        assert_eq!(r.detail().item.title, "Issue A (renamed)");
        assert_eq!(r.detail().body, "body of A", "editing the title leaves the body alone");
        r.act("edit body: start", GhAct::EditStart(Edit::Body), false);
        r.text("edit_body", "new body");
        r.act("edit body: save", GhAct::EditSave, false);
        assert_eq!((r.detail().body.as_str(), r.detail().item.title.as_str()), ("new body", "Issue A (renamed)"));

        r.act("lock", GhAct::Lock(true), false);
        assert!(r.detail().locked);
        r.act("unlock", GhAct::Lock(false), false);
        assert!(!r.detail().locked);
        r.act("close as not planned", GhAct::CloseItem("NOT_PLANNED"), false);
        assert!(r.detail().item.state == State::Closed && r.detail().item.not_planned);
        r.act("reopen", GhAct::ReopenItem, false);
        assert_eq!(r.detail().item.state, State::Open);
        r.text("composer", "closing note");
        r.act("comment and close", GhAct::Comment(Some(true)), false);
        assert_eq!(r.detail().item.state, State::Closed);
        r.act("reopen (2)", GhAct::ReopenItem, false);
        r.act("delete comment", GhAct::DeleteComment(comment.id.clone()), false);
        assert!(r.detail().timeline.iter().all(|e| !matches!(e, Entry::Comment(c) if c.id == comment.id)));

        eprintln!("search and lists");
        r.act("back to list", GhAct::Back, false);
        r.act("search by label", GhAct::SetQuery("label:bug".into()), false);
        assert!(r.read(|v| v.lists[0].items.iter().any(|i| i.title.starts_with("Issue A"))));
        r.act("search no match", GhAct::SetQuery("label:enhancement".into()), false);
        assert!(r.read(|v| v.lists[0].items.is_empty()));
        r.act("text search", GhAct::SetQuery("renamed in:title".into()), false);
        assert_eq!(r.read(|v| v.lists[0].total), 1);
        r.act("closed tab", GhAct::SetState(Some("closed")), false);
        r.act("reset search", GhAct::SetQuery("is:open".into()), false);

        eprintln!("pull requests");
        r.act("pull requests tab", GhAct::SetKind(Kind::Pull), false);
        let mut numbers = vec![];
        for i in 1..=5 {
            r.act("new pr form", GhAct::New(Kind::Pull), false);
            r.text("new_title", &format!("PR {i}"));
            r.text("new_body", "pr body");
            r.text("new_head", &format!("f{i}"));
            r.text("new_base", "main");
            r.act(&format!("create PR {i}"), GhAct::SubmitNew, false);
            let d = r.detail();
            assert!(d.pr.is_some() && d.item.head == format!("f{i}") && d.item.base == "main");
            numbers.push(d.item.number);
        }
        r.until("five open PRs", |v| v.lists[1].open == 5);

        r.act("open PR 1", GhAct::Open(Kind::Pull, numbers[0]), false);
        r.act("files tab", GhAct::Tab(DetailTab::Files), false);
        let files = r.read(|v| v.detail.as_ref().and_then(|d| d.files.clone())).expect("files loaded");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "f1.txt");
        assert!(files[0].lines.iter().any(|l| l.kind == DiffKind::Add));
        r.act("toggle file", GhAct::ToggleFile("f1.txt".into()), false);
        r.act("commits tab", GhAct::Tab(DetailTab::Commits), false);
        r.act("conversation tab", GhAct::Tab(DetailTab::Conversation), false);
        assert!(r.detail().timeline.iter().any(|e| matches!(e, Entry::Commit(_))));

        r.act("to draft", GhAct::ToDraft, false);
        assert!(r.detail().item.draft);
        r.act("ready for review", GhAct::Ready, false);
        assert!(!r.detail().item.draft);
        r.text("composer", "review note");
        r.act("review: comment", GhAct::Review("COMMENT"), false);
        assert!(r.detail().timeline.iter().any(|e| matches!(e, Entry::Review(rv) if rv.state == "COMMENTED")));
        r.act("review: approving your own PR is refused", GhAct::Review("APPROVE"), true);
        r.act("pr label", GhAct::ToggleLabel(label.clone()), false);
        assert!(r.detail().item.labels.iter().any(|l| l.name == "bug"));
        r.act("pr assignee", GhAct::ToggleAssignee(me_user.clone()), false);
        r.act("pr milestone", GhAct::SetMilestone(Some(ms.clone())), false);
        r.act("pr title", GhAct::EditStart(Edit::Title), false);
        r.text("edit_title", "PR 1 renamed");
        r.act("pr title save", GhAct::EditSave, false);
        assert_eq!(r.detail().item.title, "PR 1 renamed");
        r.act("close pr", GhAct::CloseItem(""), false);
        assert_eq!(r.detail().item.state, State::Closed);
        r.act("reopen pr", GhAct::ReopenItem, false);
        assert_eq!(r.detail().item.state, State::Open);

        r.act("merge PR 1 (squash)", GhAct::Merge("SQUASH"), false);
        assert_eq!(r.detail().item.state, State::Merged);
        r.act("delete merged branch", GhAct::DeleteBranch, false);
        assert!(setup.rest("GET", &format!("repos/{full}/branches/f1"), None).is_err(), "branch f1 is gone");

        // PR 2 is now behind main.
        r.act("open PR 2", GhAct::Open(Kind::Pull, numbers[1]), false);
        r.act("update branch", GhAct::UpdateBranch, false);
        r.act("merge PR 2 (merge commit)", GhAct::Merge("MERGE"), false);
        assert_eq!(r.detail().item.state, State::Merged);
        r.act("open PR 3", GhAct::Open(Kind::Pull, numbers[2]), false);
        r.act("update branch (3)", GhAct::UpdateBranch, false);
        r.act("merge PR 3 (rebase)", GhAct::Merge("REBASE"), false);
        assert_eq!(r.detail().item.state, State::Merged);

        r.act("open PR 4", GhAct::Open(Kind::Pull, numbers[3]), false);
        // Free private repositories cannot use auto-merge ("not allowed"), and with no required checks it is pointless ("clean status"); either answer proves the call is well formed.
        r.toasts.borrow_mut().clear();
        r.with_window(|v, w, cx| v.act(GhAct::AutoMerge(true), w, cx));
        r.settle();
        eprintln!("  auto-merge on: {:?}", r.toasts.borrow());
        assert!(r.toasts.borrow().iter().all(|t| !t.1 || t.0.contains("clean status") || t.0.contains("Auto merge is not allowed")), "{:?}", r.toasts.borrow());

        let pr4 = r.detail().item.id.clone();
        r.act("react to PR", GhAct::React(pr4, "ROCKET".into(), false), false);
        r.act("lock PR", GhAct::Lock(true), false);
        r.act("unlock PR", GhAct::Lock(false), false);
        r.act("ask claude prompt", GhAct::Claude("review"), false);
        assert!(Gh::new(exe.to_path_buf(), dir.clone()).checkout(numbers[3]).is_ok(), "gh pr checkout");

        eprintln!("merged and open lists");
        r.act("back", GhAct::Back, false);
        r.act("merged PRs", GhAct::SetQuery("is:merged".into()), false);
        r.until("three merged PRs", |v| v.lists[1].items.len() == 3);
        r.act("open PRs", GhAct::SetQuery("is:open".into()), false);
        r.until("two open PRs", |v| v.lists[1].items.len() == 2);
        r.act("refresh", GhAct::Refresh, false);
        r.act("escape", GhAct::Escape, false);
    }
}
