use super::*;

#[test]
#[ignore = "isolated workspace scheduling measurement"]
fn energy_sidebar_scheduling() {
    use futures::{FutureExt, StreamExt};
    struct OtherWindow;
    impl gpui::Render for OtherWindow {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            gpui::div()
        }
    }
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let mut cx = TestAppContext::single();
    cx.update(gpui_kit::init);
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app,
        |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let (other, _) = cx.update(|app| gpui_kit::open_window(Default::default(), app,
        |_, app| app.new(|_| OtherWindow))).unwrap();
    for (visible, active) in [(true, true), (false, true), (true, true), (true, false), (true, true)] {
        if !active {
            cx.update_window(other, |_, window, _| window.activate_window()).unwrap();
            cx.run_until_parked();
        }
        cx.update_window(window, |_, window, app| {
            if active { window.activate_window(); }
            ws.update(app, |ws, cx| {
                ws.store.data.layout.sidebar_visible = visible;
                ws.activity.insert("busy".into(), Activity::Working);
                cx.notify();
            });
            window.draw(app).clear(app);
        }).unwrap();
        cx.run_until_parked();
        let list = ws.read_with(&cx, |ws, _| ws.session_list.clone().unwrap());
        let mut notifications = cx.notifications(&list);
        for _ in 0..5 {
            cx.executor().advance_clock(Duration::from_millis(40));
            cx.run_until_parked();
        }
        let mut count = 0;
        while notifications.next().now_or_never().flatten().is_some() { count += 1; }
        println!("sidebar_visible={visible} active={active} interval_ms=200 spin_notifications={count}");
        assert_eq!(count, if visible && active { 5 } else { 0 });
        assert_eq!(ws.read_with(&cx, |ws, _| ws.spinning), visible && active);
    }
}

fn run() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx
        .update(|app| {
            gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))
        })
        .unwrap();
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        let t = std::time::Instant::now();
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        let acted = t.elapsed();
        draw(cx);
        draw(cx);
        // Starting a process must never run on the UI thread (a big `claude.exe` took a second to create).
        assert!(acted < Duration::from_millis(400), "the window was blocked for {acted:?}");
    };
    let cwd = work.path().to_path_buf();
    draw(&mut cx);
    step(&mut cx, &|ws, w, cx| ws.new_claude(cwd.clone(), w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().expect("a session is shown"));
    // Cross-session tools are answered on the spot, and one that can't work says why.
    use serde_json::{Value, json};
    let ask = |from: String, tool: &'static str, args: Value| std::thread::spawn(move || crate::claude::bridge::ask(&from, tool, &args));
    let take = || loop {
        if let Some(r) = crate::claude::bridge::take_requests().pop() {
            break r;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let serve = |cx: &mut TestAppContext, req: crate::claude::bridge::Request| step(cx, &|ws, w, cx| ws.serve_requests(vec![req.clone()], w, cx));
    let from_session = format!("code:{id}");

    let h = ask(from_session.clone(), "list_code_sessions", json!({}));
    serve(&mut cx, take());
    let text = h.join().unwrap().unwrap();
    assert!(text.contains(&id) && text.contains("this session"), "{text}");

    // A request whose asker gave up is dropped.
    let stale = crate::claude::bridge::Request { id: "late".into(), from: from_session.clone(), tool: "start_code_session".into(), args: json!({"folder": cwd.to_string_lossy()}), deadline: now() - 1 };
    let before = ws.read_with(&cx, |ws, _| ws.live.len());
    serve(&mut cx, stale);
    assert_eq!(ws.read_with(&cx, |ws, _| ws.live.len()), before, "an expired request was carried out");

    let h = ask(from_session.clone(), "start_code_session", json!({"folder": "no-such-project"}));
    serve(&mut cx, take());
    assert!(h.join().unwrap().unwrap_err().contains("No project or folder"));

    let live_before = ws.read_with(&cx, |ws, _| ws.live.len());
    let h = ask(from_session.clone(), "start_code_session", json!({"folder": cwd.to_string_lossy()}));
    serve(&mut cx, take());
    let text = h.join().unwrap().unwrap();
    assert_eq!(ws.read_with(&cx, |ws, _| ws.live.len()), live_before + 1);
    let code_id = text.split("(id ").nth(1).and_then(|t| t.split(')').next()).unwrap().to_string();
    let t = std::time::Instant::now();
    while ws.read_with(&cx, |ws, cx| ws.live_view(&code_id).is_none_or(|v| v.is_loading(cx))) {
        assert!(t.elapsed() < Duration::from_secs(120), "the session never opened");
        std::thread::sleep(Duration::from_millis(200));
        draw(&mut cx);
    }
    let h = ask(from_session.clone(), "read_code_session", json!({"id": code_id}));
    serve(&mut cx, take());
    assert!(h.join().unwrap().unwrap().contains("latest conversation"));
    step(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
    // Closing goes to the home screen; the ended session is picked from the sidebar.
    step(&mut cx, &|ws, w, cx| ws.act(Act::Select(id.clone()), w, cx));
}

/// Sessions, the shell panel and the GitHub view, with the real `claude` and shell but no model calls.
fn sessions_and_shell() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx
        .update(|app| {
            gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))
        })
        .unwrap();
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        let t = std::time::Instant::now();
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        let acted = t.elapsed();
        for _ in 0..2 {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
        assert!(acted < Duration::from_millis(400), "the window was blocked for {acted:?}");
    };
    let cwd = work.path().to_path_buf();
    step(&mut cx, &|ws, w, cx| ws.new_claude(cwd.clone(), w, cx));
    let first = ws.read_with(&cx, |ws, _| ws.active.clone().expect("a session is shown"));
    // A filter on a project that no longer exists filters nothing; the list still draws.
    step(&mut cx, &|ws, w, cx| ws.act(Act::FilterProject(Some("gone".into())), w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.project_filter().is_none()));
    step(&mut cx, &|ws, w, cx| ws.act(Act::FilterProject(None), w, cx));
    // The shell under a Claude session: starts on first use, survives switching away, goes with its session.
    step(&mut cx, &|ws, w, cx| ws.toggle_shell(&first, w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.shells.get(&first).is_some_and(|p| p.open)), "shell opened");
    step(&mut cx, &|ws, w, cx| ws.new_claude(cwd.clone(), w, cx));
    let second = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    assert_ne!(first, second);
    assert!(ws.read_with(&cx, |ws, _| ws.shells.contains_key(&first) && !ws.shells.contains_key(&second)), "the shell stays with its own session");
    step(&mut cx, &|ws, w, cx| ws.toggle_shell(&first, w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.active.as_deref() == Some(first.as_str()) && ws.shells[&first].open), "from another session, it shows the session and its shell");
    step(&mut cx, &|ws, w, cx| ws.toggle_shell(&first, w, cx));
    assert!(ws.read_with(&cx, |ws, _| !ws.shells[&first].open), "toggled shut");
    step(&mut cx, &|ws, w, cx| ws.toggle_shell(&first, w, cx));
    // Dragging the divider resizes the panel within bounds.
    step(&mut cx, &|ws, _w, _cx| {
        ws.shell_drag = Some(ShellDrag { id: first.clone(), y: 500.0, height: SHELL_HEIGHT });
        ws.drag_shell(400.0, 900.0);
        assert_eq!(ws.shells[&first].height, SHELL_HEIGHT + 100.0);
        ws.drag_shell(10.0, 900.0);
        assert_eq!(ws.shells[&first].height, 700.0, "keeps room for the terminal");
        ws.drag_shell(5000.0, 900.0);
        assert_eq!(ws.shells[&first].height, SHELL_MIN);
        ws.shell_drag = None;
    });
    // Editor mode: the shown session keeps its terminal and process, its folder is listed, and files open beside it.
    std::fs::write(cwd.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(cwd.join("big.txt"), "let x = 1; // a line of ordinary code\n".repeat(crate::files::MAX_LINES)).unwrap();
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(20), "{what} never happened");
            // Timers (find in files waits for typing to pause) only fire as the test clock moves.
            cx.executor().advance_clock(Duration::from_millis(50));
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
    };
    let term = ws.read_with(&cx, |ws, _| ws.live_view(&first).unwrap().entity_id());
    let (live, sidebars) = ws.read_with(&cx, |ws, _| (ws.live.len(), (ws.store.data.layout.sidebar_visible, ws.store.data.layout.right_hidden)));
    step(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    wait(&mut cx, "listing the folder", &|ws, _| ws.panes.get(&first).is_some_and(|p| p.nodes.iter().any(|n| n.name == "notes.md")));
    assert!(ws.read_with(&cx, |ws, app| ws.editor_mode
        && ws.live.len() == live
        && ws.live_view(&first).unwrap().entity_id() == term
        && ws.is_running(&first, app)), "the terminal keeps running in editor mode");
    step(&mut cx, &|ws, w, cx| ws.open_file(&first, cwd.join("notes.md"), None, w, cx));
    wait(&mut cx, "opening a file", &|ws, _| ws.panes[&first].shown().is_some());
    // The keyboard moves to the opened file, inside the pane, so the pane's shortcuts (Ctrl+P) reach it.
    cx.update_window(window, |_, window, app| {
        let pane = &ws.read(app).panes[&first];
        assert!(pane.shown().unwrap().buffer.focus_handle(app).is_focused(window) && pane.focus.contains_focused(window, app));
    })
    .unwrap();
    assert_eq!(ws.read_with(&cx, |ws, app| ws.panes[&first].shown().unwrap().buffer.read(app).value().to_string()), "# Notes\n");
    let t = std::time::Instant::now();
    step(&mut cx, &|ws, w, cx| ws.open_file(&first, cwd.join("big.txt"), None, w, cx));
    wait(&mut cx, "opening a large file", &|ws, _| ws.panes[&first].shown().is_some_and(|t| t.path.ends_with("big.txt")));
    eprintln!("a {}-line file opened and drew in {:?}", crate::files::MAX_LINES, t.elapsed());
    assert!(ws.read_with(&cx, |ws, _| ws.panes[&first].shown().unwrap().note.is_none()), "a file at the line limit opens");
    // Quick open finds a file by name; find in files finds a line and opens the file there.
    let typed = |cx: &mut TestAppContext, kind: crate::app::picker::PickerKind, text: &'static str| {
        step(cx, &|ws, w, cx| {
            ws.open_picker(kind, w, cx);
            let input = ws.panes[&first].picker.as_ref().unwrap().input.clone();
            input.update(cx, |i, cx| i.set_value(text, w, cx));
            ws.update_picker(&first, cx);
        });
    };
    typed(&mut cx, crate::app::picker::PickerKind::Files, "notmd");
    wait(&mut cx, "quick open", &|ws, _| ws.panes[&first].picker.as_ref().is_some_and(|p| !p.hits.is_empty()));
    assert_eq!(ws.read_with(&cx, |ws, _| ws.panes[&first].picker.as_ref().unwrap().hits[0].text.clone()), "notes.md");
    step(&mut cx, &|ws, w, cx| ws.choose_picked(w, cx));
    wait(&mut cx, "opening the picked file", &|ws, _| ws.panes[&first].shown().is_some_and(|t| t.path.ends_with("notes.md")));
    assert!(ws.read_with(&cx, |ws, _| ws.panes[&first].picker.is_none()), "choosing closes the picker");
    std::fs::write(cwd.join("code.rs"), "fn a() {}\nfn needle() {}\n").unwrap();
    step(&mut cx, &|ws, _w, cx| ws.refresh_files(cx));
    wait(&mut cx, "listing the new file", &|ws, _| ws.panes[&first].nodes.iter().any(|n| n.name == "code.rs"));
    typed(&mut cx, crate::app::picker::PickerKind::Text, "NEEDLE");
    wait(&mut cx, "find in files", &|ws, _| ws.panes[&first].picker.as_ref().is_some_and(|p| !p.hits.is_empty()));
    assert_eq!(ws.read_with(&cx, |ws, _| ws.panes[&first].picker.as_ref().unwrap().hits[0].line), 1);
    step(&mut cx, &|ws, w, cx| ws.choose_picked(w, cx));
    wait(&mut cx, "opening the found file", &|ws, _| ws.panes[&first].shown().is_some_and(|t| t.path.ends_with("code.rs")));
    assert_eq!(ws.read_with(&cx, |ws, app| ws.panes[&first].shown().unwrap().buffer.read(app).cursor_position().line), 1, "the cursor is on the found line");
    // Each file opened has a tab; opening one again shows its tab, and closing a tab shows its neighbour.
    let tabs = |ws: &Workspace| ws.panes[&first].tabs.iter().map(|t| t.path.file_name().unwrap().to_string_lossy().to_string()).collect::<Vec<_>>();
    assert_eq!(ws.read_with(&cx, |ws, _| tabs(ws)), ["notes.md", "big.txt", "code.rs"]);
    step(&mut cx, &|ws, w, cx| ws.open_file(&first, cwd.join("notes.md"), None, w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.panes[&first].tabs.len() == 3 && ws.panes[&first].active == Some(0)), "reopening shows the open tab");
    step(&mut cx, &|ws, _w, cx| ws.close_tab(&first, 0, cx));
    assert_eq!(ws.read_with(&cx, |ws, _| (tabs(ws), ws.panes[&first].active)), (vec!["big.txt".to_string(), "code.rs".into()], Some(0)));
    // Editing: an edit marks the tab, saving writes it; a change on disk reloads a clean tab, and with unsaved edits it
    // becomes a conflict that never overwrites either side until the user picks.
    let code = cwd.join("code.rs");
    let buffer = |ws: &Workspace| ws.panes[&first].shown().unwrap().buffer.clone();
    let shown_tab = |ws: &Workspace, f: &dyn Fn(&crate::app::Tab) -> bool| ws.panes[&first].shown().is_some_and(f);
    step(&mut cx, &|ws, w, cx| ws.open_file(&first, code.clone(), None, w, cx));
    step(&mut cx, &|ws, w, cx| buffer(ws).update(cx, |b, cx| b.replace_all("fn mine() {}\n", w, cx)));
    assert!(ws.read_with(&cx, |ws, _| shown_tab(ws, &|t| t.dirty)), "an edit marks the tab");
    step(&mut cx, &|ws, _w, cx| ws.save_shown(cx));
    wait(&mut cx, "saving", &|ws, _| shown_tab(ws, &|t| !t.dirty));
    assert_eq!(std::fs::read_to_string(&code).unwrap(), "fn mine() {}\n");
    std::fs::write(&code, "fn by_claude() {}\n// a longer file than before\n").unwrap();
    wait(&mut cx, "reloading a clean tab", &|ws, app| {
        ws.panes[&first].shown().unwrap().buffer.read(app).value().starts_with("fn by_claude")
    });
    step(&mut cx, &|ws, w, cx| buffer(ws).update(cx, |b, cx| b.replace_all("fn unsaved() {}\n", w, cx)));
    std::fs::write(&code, "fn by_claude_again() {}\n").unwrap();
    wait(&mut cx, "the conflict", &|ws, _| shown_tab(ws, &|t| t.conflict == Some(crate::app::editing::Conflict::Changed)));
    assert_eq!(ws.read_with(&cx, |ws, app| buffer(ws).read(app).value().to_string()), "fn unsaved() {}\n", "unsaved edits stay");
    step(&mut cx, &|ws, _w, cx| ws.save_shown(cx));
    wait(&mut cx, "the refused save", &|ws, _| ws.toast.as_ref().is_some_and(|t| t.text.starts_with("Not saved")));
    assert_eq!(std::fs::read_to_string(&code).unwrap(), "fn by_claude_again() {}\n", "Claude's change is not overwritten");
    step(&mut cx, &|ws, _w, cx| ws.keep_mine(&first, &code, cx));
    step(&mut cx, &|ws, _w, cx| ws.save_shown(cx));
    wait(&mut cx, "saving over the change", &|ws, _| shown_tab(ws, &|t| !t.dirty && t.stamp.is_some()));
    assert_eq!(std::fs::read_to_string(&code).unwrap(), "fn unsaved() {}\n", "keeping mine and saving writes mine");
    // Unsaved edits are never dropped without asking: closing the tab, or ending its session, asks first.
    step(&mut cx, &|ws, w, cx| buffer(ws).update(cx, |b, cx| b.replace_all("fn later() {}\n", w, cx)));
    step(&mut cx, &|ws, w, cx| {
        let ix = ws.panes[&first].active.unwrap();
        ws.request_close_tab(&first, ix, w, cx);
    });
    assert!(ws.read_with(&cx, |ws, _| matches!(ws.modal, Some(Modal::Confirm { act: Act::DiscardTab(..), .. })) && ws.panes[&first].tabs.len() == 2), "closing a dirty tab asks");
    step(&mut cx, &|ws, w, cx| ws.act(Act::Dismiss, w, cx));
    step(&mut cx, &|ws, w, cx| ws.act(Act::CloseLive(first.clone()), w, cx));
    assert!(ws.read_with(&cx, |ws, _| matches!(ws.modal, Some(Modal::Confirm { act: Act::ConfirmCloseLive(_), .. })) && ws.live_view(&first).is_some()), "ending the session asks");
    step(&mut cx, &|ws, w, cx| ws.act(Act::Dismiss, w, cx));
    assert_eq!(std::fs::read_to_string(&code).unwrap(), "fn unsaved() {}\n", "nothing was saved by asking");
    // Moving the keyboard to the chat with lines selected puts nothing in its message box; the comment button does that.
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    step(&mut cx, &|ws, w, cx| buffer(ws).update(cx, |b, cx| {
        b.focus(w, cx);
        b.select_all(w, cx);
    }));
    step(&mut cx, &|ws, w, cx| ws.focus_active(w, cx));
    assert!(ws.read_with(&cx, |ws, app| !ws.live_view(&first).unwrap().tail_text(40, app).contains("@code.rs")));
    step(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    assert!(ws.read_with(&cx, |ws, _| !ws.editor_mode && (ws.store.data.layout.sidebar_visible, ws.store.data.layout.right_hidden) == sidebars), "leaving editor mode brings the sidebars back as they were");
    // Asking for a sidebar in editor mode leaves it and shows that sidebar, even one that was hidden before.
    step(&mut cx, &|ws, w, cx| {
        ws.store.data.layout.sidebar_visible = false;
        ws.toggle_editor(w, cx);
        ws.toggle_left(cx);
    });
    assert!(ws.read_with(&cx, |ws, _| !ws.editor_mode && ws.store.data.layout.sidebar_visible), "the sidebar shows");
    step(&mut cx, &|ws, w, cx| ws.close_live(&first, w, cx));
    assert!(ws.read_with(&cx, |ws, _| !ws.shells.contains_key(&first)), "closing the session drops its shell");
    assert!(ws.read_with(&cx, |ws, _| !ws.panes.contains_key(&first)), "closing the session drops its editor pane");
    // The GitHub view lives in the right sidebar: one per folder, it widens the sidebar, "Ask Claude" starts a session.
    assert_eq!(ws.read_with(&cx, |ws, _| ws.right_width(1600.0)), RIGHT_MENU_W, "the menu starts at its default width");
    // Both sidebars can be dragged; the menu and the GitHub view each remember their own width.
    step(&mut cx, &|ws, _w, _cx| {
        ws.right_drag = Some(SideDrag { x: 1000.0, width: RIGHT_MENU_W });
        ws.drag_right(960.0, 1600.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_MENU_W + 40.0);
        ws.drag_right(5000.0, 1600.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_MENU_MIN);
        ws.drag_right(-5000.0, 1600.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_MENU_MAX);
        ws.drag_right(960.0, 1600.0);
        ws.right_drag = None;
        assert_eq!(ws.left_width(1600.0), SIDEBAR_W);
        ws.left_drag = Some(SideDrag { x: 300.0, width: SIDEBAR_W });
        ws.drag_left(350.0, 1600.0);
        assert_eq!(ws.left_width(1600.0), SIDEBAR_W + 50.0);
        ws.drag_left(0.0, 1600.0);
        assert_eq!(ws.left_width(1600.0), SIDEBAR_MIN);
        ws.drag_left(5000.0, 1600.0);
        assert_eq!(ws.left_width(1600.0), SIDEBAR_MAX);
        assert_eq!(ws.left_width(700.0), SIDEBAR_MIN, "keeps room for the session in a small window");
        ws.left_drag = None;
    });
    // A card reaches the terminal of its session, and a card for a session that is not running is dropped.
    let shown = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    let stem = datadir::safe_file_stem(&shown);
    let card = |call: &str| crate::visual::Card { call: call.into(), content: "# Hi".into() };
    step(&mut cx, &|ws, _w, cx| ws.apply_visuals(vec![(stem.clone(), card("c1")), ("no-such-session".into(), card("c2"))], cx));
    assert!(ws.read_with(&cx, |ws, cx| {
        let view = ws.live_view(&shown).unwrap().thread().unwrap().read(cx);
        view.has_card("c1") && !view.has_card("c2")
    }));
    step(&mut cx, &|ws, w, cx| ws.open_github(cwd.clone(), w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.gh.len() == 1 && ws.right_gh.is_some()), "github shown in the right sidebar");
    assert_eq!(ws.read_with(&cx, |ws, _| ws.right_width(1600.0)), RIGHT_GH_W, "GitHub keeps its own width");
    step(&mut cx, &|ws, _w, _cx| {
        ws.right_drag = Some(SideDrag { x: 1000.0, width: RIGHT_GH_W });
        ws.drag_right(900.0, 1600.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_GH_W + 100.0);
        ws.drag_right(5000.0, 1600.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_GH_MIN);
        ws.drag_right(-5000.0, 1600.0);
        assert_eq!(ws.right_width(1600.0), 1040.0, "keeps room for the session");
        ws.right_drag = None;
    });
    step(&mut cx, &|ws, w, cx| ws.toggle_right(w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.store.data.layout.right_hidden), "hidden");
    step(&mut cx, &|ws, w, cx| ws.toggle_right(w, cx));
    assert!(ws.read_with(&cx, |ws, _| !ws.store.data.layout.right_hidden), "shown again");
    step(&mut cx, &|ws, w, cx| ws.open_github(cwd.clone(), w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.gh.len() == 1), "one github pane per folder");
    // A running Claude session in the folder takes the request itself, so close the leftovers to see a new one start.
    let leftovers: Vec<Id> = ws.read_with(&cx, |ws, _| ws.live.iter().map(|l| l.id.clone()).collect());
    for id in leftovers {
        step(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
    }
    let before = ws.read_with(&cx, |ws, _| ws.live.len());
    step(&mut cx, &|ws, _w, cx| {
        let view = ws.gh[0].view.clone();
        view.update(cx, |_, cx| cx.emit(crate::github::view::GhEvent::AskClaude("explain issue 1".into())));
    });
    assert!(ws.read_with(&cx, |ws, _| ws.live.len() == before + 1 && ws.active.is_some()), "Ask Claude opens a session");
    assert!(ws.read_with(&cx, |ws, _| ws.right_gh.is_some() && ws.gh.len() == 1), "the right sidebar keeps following the same project");
    // A new session in another folder: the open GitHub view moves to that folder.
    let other = tempfile::tempdir().unwrap();
    let other_cwd = other.path().to_path_buf();
    step(&mut cx, &|ws, w, cx| ws.new_claude(other_cwd.clone(), w, cx));
    assert!(
        ws.read_with(&cx, |ws, _| ws.gh.len() == 2 && ws.right_gh.as_ref().and_then(|id| ws.gh_pane(id)).is_some_and(|p| platform::path_key(&p.cwd) == platform::path_key(&other_cwd))),
        "GitHub follows a new session"
    );
}

#[test]
#[ignore]
fn sessions_and_the_shell_panel() {
    let t = std::thread::spawn(sessions_and_shell);
    let start = std::time::Instant::now();
    while !t.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(120), "the workspace froze");
        std::thread::sleep(Duration::from_millis(100));
    }
    t.join().unwrap();
}

#[test]
#[ignore]
fn starting_sessions_does_not_hang_or_panic() {
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        run();
        tx.send(()).ok();
    });
    if rx.recv_timeout(std::time::Duration::from_secs(480)).is_err() {
        // Either it froze (the bug) or the run panicked (the sender dropped).
        match t.is_finished() {
            true => t.join().unwrap(),
            false => panic!("the workspace froze while starting sessions"),
        }
    }
}

