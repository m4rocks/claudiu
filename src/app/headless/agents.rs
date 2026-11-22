use super::*;

/// With no `claude` found, starting a session says so and offers the setup window; asking for it twice shows one.
#[test]
#[ignore]
fn a_missing_claude_offers_the_setup_window() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();

    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            ws.claude_exe = None;
            ws.new_claude(work.path().to_path_buf(), window, cx);
        })
    })
    .unwrap();
    let action = ws.read_with(&cx, |ws, _| ws.toast.as_ref().and_then(|t| t.action.as_ref().map(|(label, act)| (label.clone(), matches!(act, Act::OpenSetup)))));
    assert_eq!(action, Some(("Check setup".to_string(), true)), "the toast offers the setup window");

    for _ in 0..2 {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.act(Act::OpenSetup, window, cx))).unwrap();
        cx.run_until_parked();
    }
    assert_eq!(cx.update(|app| app.windows().len()), 2, "the workspace and one setup window");
}

/// A process that ends by itself closes the session the way the X does. A clean exit says nothing, a non-zero
/// exit is a toast, and the worktree stays: Claude Code already asked on `/exit`, and the X is the path that offers.
#[test]
fn a_process_that_ends_closes_the_session_and_leaves_its_worktree() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let repo_dir = dunce::canonicalize(work.path()).unwrap().join("proj");
    let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&repo_dir).output().unwrap();
    std::fs::create_dir_all(&repo_dir).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"], &["commit", "-q", "--allow-empty", "-m", "one"]] {
        git(args);
    }
    let folder = crate::git::worktree::folder(&repo_dir, "fix");
    let added = git(&["worktree", "add", "-q", "-b", "worktree-fix", &folder.to_string_lossy()]);
    assert!(added.status.success(), "{}", String::from_utf8_lossy(&added.stderr));

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![repo_dir.clone()], cx))))
        .unwrap();
    let project = ws.read_with(&cx, |ws, _| ws.store.data.projects[0].id.clone());
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let exit_spec = |code: i32| -> SpawnSpec {
        let args = if cfg!(windows) {
            vec!["/c".into(), format!("exit {code}")]
        } else {
            vec!["-c".into(), format!("exit {code}")]
        };
        SpawnSpec { program: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.into(), args, cwd: Some(folder.clone()), ..Default::default() }
    };
    let start_one = |cx: &mut TestAppContext, title: &str, spec: SpawnSpec| -> String {
        let title = title.to_string();
        let id = new_id();
        let record = SessionRecord {
            id: id.clone(),
            kind: SessionKind::Claude,
            title,
            custom_title: None,
            auto_title: None,
            project_id: Some(project.clone()),
            cwd: folder.clone(),
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            // Present, so closing it keeps the history record instead of treating it as an empty new session.
            claude: Some(ClaudeMeta { session_id: id.clone(), external: ExternalState::Present, ..Default::default() }),
        };
        step(cx, &|ws, w, cx| ws.start(record.clone(), spec.clone(), true, w, cx));
        id
    };
    // Pump until the view is gone. A late removal dialog is spawned off the UI thread, so keep going after the close.
    let until_closed = |cx: &mut TestAppContext, id: &str| {
        let started = std::time::Instant::now();
        loop {
            step(cx, &|_, _, _| {});
            let gone = ws.read_with(cx, |ws, _| ws.live_view(id).is_none());
            if gone {
                let rest = std::time::Instant::now();
                while rest.elapsed() < Duration::from_secs(1) {
                    step(cx, &|_, _, _| {});
                    cx.executor().advance_clock(Duration::from_millis(50));
                    std::thread::sleep(Duration::from_millis(50));
                    let offered = ws.read_with(cx, |ws, _| ws.modal.is_some());
                    assert!(!offered, "a process that ended by itself offered to remove the worktree");
                }
                return;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "the session stayed open after its process ended");
            cx.executor().advance_clock(Duration::from_millis(50));
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    // Session-only shortcuts are inert on Home, even with a project available as a fallback folder.
    step(&mut cx, &|ws, _, _| {
        ws.main = Main::Home;
        ws.active = None;
        ws.toast = None;
    });
    cx.dispatch_action(window, ToggleEditor);
    cx.dispatch_action(window, OpenGithub);
    cx.run_until_parked();
    assert!(ws.read_with(&cx, |ws, _| !ws.editor_mode && ws.gh.is_empty() && ws.right_gh.is_none() && ws.toast.is_none()), "disabled shortcuts must not open a view or show a toast");

    let clean = start_one(&mut cx, "Fix login", exit_spec(0));
    until_closed(&mut cx, &clean);
    let (code, toast) = ws.read_with(&cx, |ws, _| (ws.store.session(&clean).map(|s| s.exit_code), ws.toast.as_ref().map(|t| t.text.clone())));
    assert_eq!(code, Some(Some(0)), "a clean exit is kept on the history record");
    assert_eq!(toast, None, "a clean exit says nothing");
    assert!(folder.exists(), "the worktree stays");

    let bad = start_one(&mut cx, "Broken", exit_spec(1));
    until_closed(&mut cx, &bad);
    let (code, toast) = ws.read_with(&cx, |ws, _| (ws.store.session(&bad).map(|s| s.exit_code), ws.toast.as_ref().map(|t| t.text.clone())));
    assert_eq!(code, Some(Some(1)));
    assert_eq!(toast.as_deref(), Some("Broken ended (exit code 1)"));

    let missing = new_id();
    let record = SessionRecord {
        id: missing.clone(),
        kind: SessionKind::Claude,
        title: "Missing".into(),
        custom_title: None,
        auto_title: None,
        project_id: Some(project),
        cwd: folder.clone(),
        created_at: now(),
        last_active: now(),
        exit_code: None,
        hidden: false,
        claude: Some(ClaudeMeta { session_id: missing.clone(), external: ExternalState::Present, ..Default::default() }),
    };
    let spec = SpawnSpec { program: "definitely-not-a-real-program-m4code".into(), args: vec![], cwd: Some(folder), ..Default::default() };
    step(&mut cx, &|ws, w, cx| ws.start(record.clone(), spec.clone(), true, w, cx));
    until_closed(&mut cx, &missing);
    let toast = ws.read_with(&cx, |ws, _| ws.toast.as_ref().map(|t| t.text.clone()));
    assert!(toast.as_deref().is_some_and(|t| t.starts_with("Could not start Missing:")), "{toast:?}");
}

/// A turn that changes a file shows in the review: a hunk put back is as it was before the message, and a comment goes
/// back to the agent naming its line.
#[test]
#[ignore = "drives the workspace with a stand-in agent"]
fn reviewing_what_a_turn_changed() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(work.path()).unwrap().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let git = |args: &[&str]| assert!(std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).current_dir(&cwd).status().unwrap().success());
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(cwd.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "one"]);
    let agent = fake_agent(work.path());
    let prompts = PathBuf::from(format!("{}.prompts", agent.1[0]));

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
            draw(cx);
        }
    };
    ws.update(&mut cx, |ws, _| ws.acp_server = Some(agent.clone()));
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.new_claude(cwd.clone(), window, cx))).unwrap();
    draw(&mut cx);
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().expect("a session is shown"));
    let thread = |ws: &Workspace| ws.live_view(&id).and_then(|v| v.thread()).cloned().expect("a thread view");
    wait(&mut cx, "the session to open", &|ws, app| !thread(ws).read(app).is_loading());
    let on_thread = |cx: &mut TestAppContext, f: &dyn Fn(&mut crate::thread::ThreadView, &mut Window, &mut Context<crate::thread::ThreadView>)| {
        cx.update_window(window, |_, window, app| {
            let view = thread(ws.read(app));
            view.update(app, |v, cx| f(v, window, cx));
        })
        .unwrap();
        draw(cx);
    };

    on_thread(&mut cx, &|v, w, cx| v.send("edit the notes".into(), w, cx));
    wait(&mut cx, "the turn", &|ws, app| thread(ws).read(app).can_review());
    assert_eq!(std::fs::read_to_string(cwd.join("notes.txt")).unwrap(), "one\nTWO\nthree\n");
    on_thread(&mut cx, &|v, _, cx| v.open_review(cx));
    wait(&mut cx, "the changes", &|ws, app| thread(ws).read(app).review_files().is_some());
    let files = ws.read_with(&cx, |ws, app| thread(ws).read(app).review_files().unwrap().to_vec());
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, PathBuf::from("notes.txt"));
    let added = files[0].hunks[0].lines.iter().find(|l| l.origin == '+').unwrap();
    assert_eq!((added.text.as_str(), added.new_line), ("TWO", Some(2)));

    // A comment on the changed line goes back to the agent with the review closed.
    on_thread(&mut cx, &|v, w, cx| v.comment_on("notes.txt".into(), 2, "keep it lowercase", w, cx));
    on_thread(&mut cx, &|v, w, cx| v.send_comments(w, cx));
    wait(&mut cx, "the comments to reach the agent", &|_, _| std::fs::read_to_string(&prompts).is_ok_and(|p| p.contains("@notes.txt#L2: keep it lowercase")));

    // That message was a turn too, and it changed nothing, so there is nothing to review. Putting back the earlier
    // hunk is then the only change.
    wait(&mut cx, "the second turn's changes to be read", &|ws, app| thread(ws).read(app).changes_known());
    assert!(!ws.read_with(&cx, |ws, app| thread(ws).read(app).can_review()), "a turn that changed nothing has nothing to review");
    on_thread(&mut cx, &|v, _, cx| v.open_review(cx));
    wait(&mut cx, "the changes", &|ws, app| thread(ws).read(app).review_files().is_some());
    assert!(ws.read_with(&cx, |ws, app| thread(ws).read(app).review_files().unwrap().is_empty()));
    let hunk = files[0].hunks[0].clone();
    on_thread(&mut cx, &|v, _, cx| v.revert_hunk(hunk.clone(), cx));
    wait(&mut cx, "the hunk to go back", &|_, _| std::fs::read_to_string(cwd.join("notes.txt")).unwrap() == "one\ntwo\nthree\n");
    wait(&mut cx, "the review to read again", &|ws, app| thread(ws).read(app).review_files().is_some_and(|f| f.len() == 1));
}

/// A session the agent lists for a folder opens here as a m4code session, loading the agent's own conversation, and
/// picking it up again shows the same record.
#[test]
#[ignore = "drives the workspace with a stand-in agent"]
fn picking_up_a_session_started_elsewhere() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(work.path()).unwrap();
    let agent = fake_agent(work.path());
    let prompts = PathBuf::from(format!("{}.prompts", agent.1[0]));
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
    };
    ws.update(&mut cx, |ws, _| ws.acp_server = Some(agent.clone()));
    let act = |cx: &mut TestAppContext, act: Act| cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.act(act, window, cx))).unwrap();
    act(&mut cx, Act::ListSessions { cwd: cwd.clone() });
    wait(&mut cx, "the list", &|ws, _| ws.menu.is_some());
    let pick = ws.read_with(&cx, |ws, _| {
        let items = &ws.menu.as_ref().unwrap().items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "Started in a terminal");
        items[0].act.clone()
    });
    act(&mut cx, pick.clone());
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().expect("the picked session is shown"));
    wait(&mut cx, "the session to open", &|ws, app| ws.live_view(&id).and_then(|v| v.thread()).is_some_and(|t| !t.read(app).is_loading()));
    assert!(std::fs::read_to_string(&prompts).unwrap().contains("loaded from-terminal"), "the agent's own conversation was loaded");
    ws.read_with(&cx, |ws, _| {
        let rec = ws.store.session(&id).unwrap();
        assert_eq!((rec.claude_id(), rec.title.as_str()), (Some("from-terminal"), "Started in a terminal"));
    });
    act(&mut cx, pick);
    assert_eq!(ws.read_with(&cx, |ws, _| ws.store.data.sessions.iter().filter(|s| s.claude_id() == Some("from-terminal")).count()), 1, "no second record");
}

/// Handing a session to another agent starts a session of that agent in the same folder, whose first message carries
/// the conversation; the first session keeps running.
#[test]
#[ignore = "drives the workspace with a stand-in agent"]
fn handing_a_session_to_another_agent() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(work.path()).unwrap();
    let agent = fake_agent(work.path());
    let prompts = PathBuf::from(format!("{}.prompts", agent.1[0]));
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
    };
    ws.update(&mut cx, |ws, _| ws.acp_server = Some(agent.clone()));
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.new_claude(cwd.clone(), window, cx))).unwrap();
    let first = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    let thread = |ws: &Workspace, id: &str| ws.live_view(id).and_then(|v| v.thread()).cloned();
    wait(&mut cx, "the session to open", &|ws, app| thread(ws, &first).is_some_and(|t| !t.read(app).is_loading()));
    cx.update_window(window, |_, window, app| thread(ws.read(app), &first).unwrap().update(app, |v, cx| v.send("plan the change".into(), window, cx))).unwrap();
    wait(&mut cx, "the reply", &|ws, app| thread(ws, &first).is_some_and(|t| !t.read(app).is_working() && t.read(app).tail_text(10).contains("Agent: ok")));

    // Clicking a model in the picker hands the keyboard back to the message box, which the search box took.
    cx.update_window(window, |_, window, app| thread(ws.read(app), &first).unwrap().update(app, |v, cx| v.toggle_model_picker(window, cx))).unwrap();
    cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
    let mut visual = gpui::VisualTestContext::from_window(window, &cx);
    let row = visual.debug_bounds("thread-model-0").expect("the model picker draws its first row").center();
    visual.simulate_event(gpui::MouseDownEvent { position: row, modifiers: Default::default(), button: gpui::MouseButton::Left, click_count: 1, first_mouse: false });
    visual.simulate_event(gpui::MouseUpEvent { position: row, modifiers: Default::default(), button: gpui::MouseButton::Left, click_count: 1 });
    cx.update_window(window, |_, window, app| {
        let view = thread(ws.read(app), &first).unwrap();
        assert!(view.read(app).picker.is_none(), "picking a model closes the picker");
        assert!(view.focus_handle(app).is_focused(window), "the message box has the keyboard after a pick");
    })
    .unwrap();

    // Pick a model served by Codex from the unified model picker; sending the next message dynamically hands off in place.
    cx.update_window(window, |_, window, app| {
        let view = thread(ws.read(app), &first).unwrap();
        view.update(app, |v, cx| {
            v.pick_model(
                crate::harness::CatalogModel {
                    harness: crate::harness::Harness::Codex,
                    id: "gpt-5.3-codex".into(),
                    name: "GPT-5.3 Codex".into(),
                },
                cx,
            );
            v.send("implement step 2".into(), window, cx);
        });
    })
    .unwrap();
    let after = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    assert_eq!(after, first, "dynamic handoff continues in the same session");
    assert_eq!(
        ws.read_with(&cx, |ws, app| thread(ws, &first).and_then(|t| t.read(app).starting_label())).as_deref(),
        Some("Handing over to Codex…"),
        "the message box says the hand-off is under way while the new agent starts"
    );
    ws.read_with(&cx, |ws, _| {
        let rec = ws.store.session(&first).unwrap();
        assert_eq!((rec.claude.as_ref().unwrap().harness, &rec.cwd), (crate::harness::Harness::Codex, &cwd));
    });
    wait(&mut cx, "the hand-off message", &|_, _| {
        std::fs::read_to_string(&prompts).is_ok_and(|p| {
            p.contains("taking over work from a Claude Code session")
                && p.contains("User: plan the change\n\nAgent: ok")
                && p.contains("User's next message:\nimplement step 2")
        })
    });
}

/// A session of each agent through ACP (Claude Code through the SDK bridge): it starts, names itself, and answers a message in the thread.
/// Each spends one short turn on the signed-in account. `cargo test -- --ignored session_answers`.
#[test]
#[ignore = "runs the real Claude Code"]
fn a_claude_code_session_answers_in_its_thread() {
    a_session_answers_in_its_thread(crate::harness::Harness::Claude);
}

#[test]
#[ignore = "runs the real Codex"]
fn a_codex_session_answers_in_its_thread() {
    a_session_answers_in_its_thread(crate::harness::Harness::Codex);
}

#[test]
#[ignore = "runs the real OpenCode"]
fn an_opencode_session_answers_in_its_thread() {
    a_session_answers_in_its_thread(crate::harness::Harness::OpenCode);
}

fn a_session_answers_in_its_thread(harness: crate::harness::Harness) {
    use crate::harness::thread::Entry;
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let wait = |cx: &mut TestAppContext, what: &str, secs: u64, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
            draw(cx);
        }
    };
    let cwd = dunce::canonicalize(work.path()).unwrap();
    ws.update(&mut cx, |ws, _| ws.store.data.settings.harness = harness);
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.new_claude(cwd.clone(), window, cx))).unwrap();
    draw(&mut cx);
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().expect("a session is shown"));
    let thread = |ws: &Workspace| ws.live_view(&id).and_then(|v| v.thread()).cloned().expect("a thread view");
    wait(&mut cx, "the session to open", 120, &|ws, app| !thread(ws).read(app).is_loading() || !ws.is_running(&id, app));
    assert!(ws.read_with(&cx, |ws, app| ws.is_running(&id, app)), "the harness stopped: {:?}", ws.read_with(&cx, |ws, app| thread(ws).read(app).start_error()));
    assert!(ws.read_with(&cx, |ws, _| ws.store.session(&id).and_then(|r| r.claude_id()).is_some()), "the session's id was kept");
    cx.update_window(window, |_, window, app| {
        let view = ws.read(app).live_view(&id).and_then(|v| v.thread()).cloned().unwrap();
        view.update(app, |v, cx| v.send("Reply with exactly one word: pong".into(), window, cx));
    })
    .unwrap();
    wait(&mut cx, "the reply", 180, &|ws, app| {
        let v = thread(ws);
        let v = v.read(app);
        !v.is_working() && v.thread.entries.iter().any(|e| matches!(e, Entry::Agent(t) if t.to_lowercase().contains("pong")))
    });
}

