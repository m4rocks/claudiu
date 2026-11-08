//! The GitHub pane: issues and pull requests of a project's repository, driven through the user's `gh` CLI.
//! `cli` runs `gh`, `model` parses its answers (no UI), `view` is the pane itself.

pub mod cli;
pub mod model;
pub(crate) mod ui;
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
        assert!(!repo.viewer.is_empty());

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

