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
