//! Drives the real workspace on gpui's test platform (no window opens) with a throwaway data folder, to catch
//! hangs and panics when sessions start and switch and a shell panel opens. Spawns the real `claude` and shell.
//! `cargo test -- --ignored headless`.
use super::*;
use gpui::TestAppContext;

fn run() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
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
    while ws.read_with(&cx, |ws, cx| ws.live_view(&code_id).is_none_or(|v| v.read(cx).tail_text(40).is_empty())) {
        assert!(t.elapsed() < Duration::from_secs(60), "the session drew nothing to read");
        std::thread::sleep(Duration::from_millis(200));
        draw(&mut cx);
    }
    let h = ask(from_session.clone(), "read_code_session", json!({"id": code_id}));
    serve(&mut cx, take());
    assert!(h.join().unwrap().unwrap().contains("latest terminal output"));
    step(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
}

/// Sessions, the shell panel and the GitHub view, with the real `claude` and shell but no model calls.
fn sessions_and_shell() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
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
    step(&mut cx, &|ws, w, cx| ws.close_live(&first, w, cx));
    assert!(ws.read_with(&cx, |ws, _| !ws.shells.contains_key(&first)), "closing the session drops its shell");
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
    // The Visual panel: the first card a session's model draws opens it (wide, sharing GitHub's width), the same call
    // keeps filling its card without reopening it, and a card for a session that is not shown only lists it.
    let shown = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    let stem = datadir::safe_file_stem(&shown);
    let card = |content: &str, done: bool| crate::visual::Card { call: "c1".into(), title: "Plan".into(), content: content.into(), done };
    assert!(ws.read_with(&cx, |ws, _| !ws.visual_shown()));
    step(&mut cx, &|ws, _w, cx| ws.apply_visuals(vec![(stem.clone(), card("# Hi", false))], cx));
    assert!(ws.read_with(&cx, |ws, _| ws.visual_shown() && ws.right_gh.is_none()), "a new card opens the panel");
    assert_eq!(ws.read_with(&cx, |ws, _| ws.right_width(1600.0)), RIGHT_GH_W);
    step(&mut cx, &|ws, _w, cx| ws.hide_visual(false, cx));
    step(&mut cx, &|ws, _w, cx| ws.apply_visuals(vec![(stem.clone(), card("# Hi there", true))], cx));
    assert!(ws.read_with(&cx, |ws, _| !ws.visual_shown() && ws.visual_cards().len() == 1 && ws.visual_cards()[0].done), "the same call updates its card and leaves the panel shut");
    assert_eq!(ws.read_with(&cx, |ws, _| ws.right_width(1600.0)), RIGHT_MENU_W + 40.0);
    step(&mut cx, &|ws, _w, cx| ws.show_visual(cx));
    assert!(ws.read_with(&cx, |ws, _| ws.visual_shown()));
    step(&mut cx, &|ws, _w, cx| ws.hide_visual(true, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.visual_cards().is_empty() && !ws.visual_shown()), "clearing forgets the cards");
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
