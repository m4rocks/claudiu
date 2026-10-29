//! Drives the real workspace on gpui's test platform (no window opens) with a throwaway data folder, to catch
//! hangs and panics when sessions start and switch and a shell panel opens. Spawns the real `claude` and shell.
//! `cargo test -- --ignored headless`.
use super::*;
use gpui::{TestAppContext, VisualTestContext};

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

    // A first message of its own needs the user's yes: a closed dialog is a refusal, and nothing was started.
    let live_before = ws.read_with(&cx, |ws, _| ws.live.len());
    let h = ask(from_session.clone(), "start_code_session", json!({"folder": cwd.to_string_lossy(), "prompt": "do the thing"}));
    serve(&mut cx, take());
    assert!(ws.read_with(&cx, |ws, _| matches!(ws.modal, Some(Modal::Confirm { act: Act::ConfirmStartSession(_), .. }))), "the user was not asked");
    step(&mut cx, &|ws, w, cx| ws.act(Act::Dismiss, w, cx));
    step(&mut cx, &|ws, w, cx| ws.serve_requests(vec![], w, cx));
    assert_eq!(h.join().unwrap().unwrap_err(), "The user declined.");
    assert_eq!(ws.read_with(&cx, |ws, _| ws.live.len()), live_before, "a declined request started a session");

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
        assert!(pane.shown().unwrap().buffer.read(app).focus_handle(app).is_focused(window) && pane.focus.contains_focused(window, app));
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
    // Claude Code first asks whether to trust this new folder. That is a menu, where a pasted line break could pick an
    // answer, so moving there with lines selected puts nothing in (`selection_reference` covers Claude's input box).
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    step(&mut cx, &|ws, w, cx| buffer(ws).update(cx, |b, cx| {
        b.focus(w, cx);
        b.select_all(w, cx);
    }));
    step(&mut cx, &|ws, w, cx| ws.focus_active(w, cx));
    assert!(ws.read_with(&cx, |ws, app| !ws.live_view(&first).unwrap().read(app).tail_text(40).contains("@code.rs")));
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

/// Editor mode with a real rust-analyzer: diagnostics arrive, hover, go-to-definition and completion answer, and a language whose
/// server isn't installed says so. Uses `rust-analyzer` from `CLAUDIU_TEST_RUST_ANALYZER_DIR` (prepended to PATH),
/// else PATH. `cargo test -- --ignored language_servers`.
fn language_servers_with_rust_analyzer() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe {
        std::env::set_var("CLAUDIU_DATA_DIR", data.path());
        if let Some(dir) = std::env::var_os("CLAUDIU_TEST_RUST_ANALYZER_DIR") {
            let mut paths = vec![PathBuf::from(dir)];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
            std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        }
    }
    let work = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(work.path()).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    let code = "fn helper() -> u32 {\n    1\n}\n\nfn main() {\n    let n: u32 = helper();\n    let s: u32 = \"not a number\";\n}\n";
    std::fs::write(root.join("src/main.rs"), code).unwrap();
    std::fs::write(root.join("probe.gleam"), "pub fn main() { 1 }\n").unwrap();

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx))))
        .unwrap();
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        draw(cx);
    };
    let wait = |cx: &mut TestAppContext, what: &str, secs: u64, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(secs), "{what} never happened: {:?}", ws.read_with(cx, |ws, _| ws.active.clone().and_then(|id| ws.server_status(&id))));
            std::thread::sleep(Duration::from_millis(100));
            cx.executor().advance_clock(Duration::from_millis(100));
            draw(cx);
        }
    };
    act(&mut cx, &|ws, w, cx| ws.new_claude(root.clone(), w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    act(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    wait(&mut cx, "the editor pane", 20, &|ws, _| ws.panes.contains_key(&id));

    act(&mut cx, &|ws, w, cx| ws.open_file(&id, root.join("probe.gleam"), None, w, cx));
    wait(&mut cx, "gleam's status", 20, &|ws, _| ws.server_status(&id).is_some_and(|s| !s.contains("starting")));
    if crate::platform::find_executable("gleam").is_none() {
        assert_eq!(ws.read_with(&cx, |ws, _| ws.server_status(&id)).unwrap(), "gleam isn't installed, so this file has no diagnostics or hover.");
    }

    // TypeScript's server comes with the compiler from npm (a `.cmd` on Windows), and is asked for its diagnostics.
    std::fs::write(root.join("probe.ts"), "export function f() {
	const n: number = \"x\";
	return n;
}
").unwrap();
    act(&mut cx, &|ws, w, cx| ws.open_file(&id, root.join("probe.ts"), None, w, cx));
    wait(&mut cx, "typescript's status", 20, &|ws, _| ws.server_status(&id).is_some_and(|s| !s.contains("starting")));
    if crate::platform::find_executable("tsc").is_none() {
        assert_eq!(ws.read_with(&cx, |ws, _| ws.server_status(&id)).unwrap(), "tsc isn't installed, so this file has no diagnostics or hover.");
    } else {
        assert_eq!(ws.read_with(&cx, |ws, _| ws.server_status(&id)).as_deref(), Some("tsc"));
        let ts = ws.read_with(&cx, |ws, _| ws.panes[&id].shown().unwrap().buffer.clone());
        wait(&mut cx, "a TypeScript diagnostic", 60, &|_, app| ts.read(app).diagnostics().is_some_and(|d| d.iter().any(|e| e.message.contains("not assignable"))));
    }

    act(&mut cx, &|ws, w, cx| ws.open_file(&id, root.join("src/main.rs"), None, w, cx));
    wait(&mut cx, "rust-analyzer starting", 60, &|ws, _| ws.server_status(&id).is_some_and(|s| s == "rust-analyzer"));
    let buffer = ws.read_with(&cx, |ws, _| ws.panes[&id].shown().unwrap().buffer.clone());
    wait(&mut cx, "a diagnostic", 240, &|_, app| {
        let messages: Vec<String> = buffer.read(app).diagnostics().map(|d| d.iter().map(|e| e.message.to_string()).collect()).unwrap_or_default();
        messages.iter().any(|m| m.contains("u32"))
    });

    // Ask the editor's own providers, as a hover and an F12 would.
    let at = code.find("helper();").unwrap() + 2;
    let hover: Rc<RefCell<Option<String>>> = Default::default();
    let found: Rc<RefCell<Option<u32>>> = Default::default();
    cx.update_window(window, |_, window, app| {
        let (lsp_hover, lsp_def, text) = {
            let b = buffer.read(app);
            (b.lsp().hover_provider.clone().unwrap(), b.lsp().definition_provider.clone().unwrap(), b.text().clone())
        };
        let (h, d) = (lsp_hover.hover(&text, at, window, app), lsp_def.definitions(&text, at, window, app));
        let (hover, found) = (hover.clone(), found.clone());
        app.spawn(async move |_| {
            let h = h.await.unwrap().map(|h| format!("{:?}", h.contents));
            *hover.borrow_mut() = Some(h.unwrap_or_default());
            *found.borrow_mut() = d.await.unwrap().first().map(|l| l.target_selection_range.start.line);
        })
        .detach();
    })
    .unwrap();
    wait(&mut cx, "hover and definition", 60, &|_, _| found.borrow().is_some());
    assert!(hover.borrow().as_deref().is_some_and(|h| h.contains("fn helper")), "hover: {:?}", hover.borrow());
    assert_eq!(*found.borrow(), Some(0), "helper is defined on the first line");

    // And as Ctrl+Space would: the word typed so far ("hel") is completed by the function.
    let labels: Rc<RefCell<Option<Vec<String>>>> = Default::default();
    cx.update_window(window, |_, window, app| {
        let (provider, text) = {
            let b = buffer.read(app);
            (b.lsp().completion_provider.clone().unwrap(), b.text().clone())
        };
        let context = lsp_types::CompletionContext { trigger_kind: lsp_types::CompletionTriggerKind::INVOKED, trigger_character: None };
        let reply = provider.completions(&text, at + 1, context, window, app);
        let labels = labels.clone();
        app.spawn(async move |_| {
            let items = match reply.await.unwrap() {
                lsp_types::CompletionResponse::Array(items) => items,
                lsp_types::CompletionResponse::List(list) => list.items,
            };
            *labels.borrow_mut() = Some(items.into_iter().map(|i| i.label).collect());
        })
        .detach();
    })
    .unwrap();
    wait(&mut cx, "completions", 60, &|_, _| labels.borrow().is_some());
    assert!(labels.borrow().as_ref().unwrap().iter().any(|l| l.starts_with("helper")), "completions: {:?}", labels.borrow());

    // Ending the session ends its servers with it.
    act(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
    assert!(ws.read_with(&cx, |ws, _| !ws.panes.contains_key(&id)));
}

#[test]
#[ignore]
fn language_servers() {
    let t = std::thread::spawn(language_servers_with_rust_analyzer);
    let start = std::time::Instant::now();
    while !t.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(300), "the workspace froze");
        std::thread::sleep(Duration::from_millis(100));
    }
    t.join().unwrap();
}

/// Lines selected in the editor go into Claude's input as `@file#L..` when the keyboard moves to Claude, and come back
/// out when it leaves without typing. Runs the real `claude` in this repository, which Claude Code already trusts (a
/// new folder stops at its trust and import questions), on a file under `target`. `cargo test -- --ignored selection_reference`.
fn selection_reference_in_claudes_input() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
    let root = dunce::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
    let work = tempfile::tempdir_in(root.join("target")).unwrap();
    let code = work.path().join("code.rs");
    std::fs::write(&code, "fn a() {}
fn b() {}
fn c() {}
").unwrap();
    let mention = format!("@{}#L2", crate::app::picker::relative(&dunce::canonicalize(&code).unwrap(), &root));

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx))))
        .unwrap();
    // Focus changes are only reported in an active window.
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        draw(cx);
    };
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(60), "{what} never happened");
            std::thread::sleep(Duration::from_millis(100));
            draw(cx);
        }
    };
    act(&mut cx, &|ws, w, cx| ws.new_claude(root.clone(), w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    let screen = |ws: &Workspace, app: &App| ws.live_view(&id).unwrap().read(app).tail_text(40);
    wait(&mut cx, "Claude's input box", &|ws, app| screen(ws, app).contains('❯') && !ws.live_view(&id).unwrap().read(app).loading);
    act(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    wait(&mut cx, "the editor pane", &|ws, _| ws.panes.contains_key(&id));
    act(&mut cx, &|ws, w, cx| ws.open_file(&id, dunce::canonicalize(&code).unwrap(), None, w, cx));
    wait(&mut cx, "opening the file", &|ws, _| ws.panes[&id].shown().is_some());
    let buffer = ws.read_with(&cx, |ws, _| ws.panes[&id].shown().unwrap().buffer.clone());
    act(&mut cx, &|_, w, cx| buffer.update(cx, |b, cx| {
        b.focus(w, cx);
        b.set_selected_range(10..20, cx);
    }));

    act(&mut cx, &|ws, w, cx| ws.focus_active(w, cx));
    wait(&mut cx, "the reference in Claude's input", &|ws, app| screen(ws, app).contains(&mention));
    act(&mut cx, &|_, w, cx| buffer.update(cx, |b, cx| b.focus(w, cx)));
    wait(&mut cx, "the untouched reference taken back", &|ws, app| !screen(ws, app).contains(&mention));

    // Typed after: it stays, and coming back with the same lines selected doesn't add it again.
    act(&mut cx, &|ws, w, cx| ws.focus_active(w, cx));
    wait(&mut cx, "the reference again", &|ws, app| screen(ws, app).contains(&mention));
    act(&mut cx, &|ws, _w, cx| ws.live_view(&id).unwrap().update(cx, |v, cx| v.paste_text("explain", cx)));
    wait(&mut cx, "the typed request", &|ws, app| screen(ws, app).contains("explain"));
    act(&mut cx, &|_, w, cx| buffer.update(cx, |b, cx| b.focus(w, cx)));
    act(&mut cx, &|ws, w, cx| ws.focus_active(w, cx));
    std::thread::sleep(Duration::from_secs(1));
    draw(&mut cx);
    assert_eq!(ws.read_with(&cx, |ws, app| screen(ws, app).matches(&mention).count()), 1, "kept once: {}", ws.read_with(&cx, |ws, app| screen(ws, app)));

    // After text the user typed first, the reference follows it on the same line.
    act(&mut cx, &|_, w, cx| buffer.update(cx, |b, cx| {
        b.focus(w, cx);
        b.set_selected_range(0..20, cx);
    }));
    act(&mut cx, &|ws, w, cx| ws.focus_active(w, cx));
    let both = format!("@{}#L1-2", crate::app::picker::relative(&dunce::canonicalize(&code).unwrap(), &root));
    wait(&mut cx, "the second reference", &|ws, app| screen(ws, app).contains(&both));
    let lines = ws.read_with(&cx, |ws, app| screen(ws, app));
    // After the user's text, on the same line with a space: the user starts a new line themselves when they want one.
    // Words only: Claude Code wraps a long input line onto the next row, padding the row it leaves.
    let words = lines.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words.contains(&format!("explain {both}:")), "after the typed text: {lines}");
    act(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
}

#[test]
#[ignore]
fn selection_reference() {
    let t = std::thread::spawn(selection_reference_in_claudes_input);
    let start = std::time::Instant::now();
    while !t.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(180), "the workspace froze");
        std::thread::sleep(Duration::from_millis(100));
    }
    t.join().unwrap();
}

/// Creating, renaming and deleting from the file tree, with open tabs following along. Runs the real `claude` in a new
/// temporary folder (not under `target`, which Git ignores, so the tree would list nothing); it waits at the folder
/// trust question, which is enough for the editor.
/// `cargo test -- --ignored file_operations`.
fn file_operations_in_the_tree() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(work.path()).unwrap();
    std::fs::write(root.join("code.rs"), "fn a() {}
").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/inner.rs"), "fn b() {}
").unwrap();

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx))))
        .unwrap();
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        draw(cx);
    };
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(60), "{what} never happened");
            std::thread::sleep(Duration::from_millis(100));
            draw(cx);
        }
    };
    let name = |cx: &mut TestAppContext, purpose: NameFor, name: &'static str| -> Result<(), String> {
        let id = ws.read_with(cx, |ws, _| ws.active.clone().unwrap());
        let out = std::cell::RefCell::new(Ok(()));
        act(cx, &|ws, w, cx| *out.borrow_mut() = ws.apply_file_name(&id, &purpose, name, w, cx));
        out.into_inner()
    };
    let shown = |ws: &Workspace, id: &Id| ws.panes[id].shown().map(|t| t.path.clone());

    act(&mut cx, &|ws, w, cx| ws.new_claude(root.clone(), w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    wait(&mut cx, "the session running", &|ws, app| ws.is_running(&id, app));
    act(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    wait(&mut cx, "the files listed", &|ws, _| ws.panes.get(&id).is_some_and(|p| !p.nodes.is_empty()));

    // A new file opens in the editor; a bad or taken name is refused and nothing is written.
    name(&mut cx, NameFor::FileIn(root.join("sub")), "made.rs").unwrap();
    wait(&mut cx, "the new file open", &|ws, _| shown(ws, &id) == Some(root.join("sub/made.rs")));
    assert!(name(&mut cx, NameFor::FileIn(root.clone()), "a/b.rs").is_err());
    assert!(name(&mut cx, NameFor::FileIn(root.clone()), "code.rs").is_err());
    assert_eq!(std::fs::read_to_string(root.join("code.rs")).unwrap(), "fn a() {}
");

    // A new folder shows in the tree; a taken name is refused.
    name(&mut cx, NameFor::FolderIn(root.join("sub")), "deeper").unwrap();
    assert!(root.join("sub/deeper").is_dir());
    wait(&mut cx, "the new folder listed", &|ws, _| ws.panes[&id].nodes.iter().filter(|n| n.path == root.join("sub")).flat_map(|n| n.children.iter().flatten()).any(|n| n.path == root.join("sub/deeper")));
    assert!(name(&mut cx, NameFor::FolderIn(root.join("sub")), "deeper").is_err());

    // A rename keeps the tab; one that changes the language reopens it.
    act(&mut cx, &|ws, w, cx| ws.open_file(&id, root.join("code.rs"), None, w, cx));
    wait(&mut cx, "code.rs open", &|ws, _| shown(ws, &id) == Some(root.join("code.rs")));
    name(&mut cx, NameFor::Rename(root.join("code.rs")), "renamed.rs").unwrap();
    wait(&mut cx, "the tab following the rename", &|ws, _| shown(ws, &id) == Some(root.join("renamed.rs")));
    name(&mut cx, NameFor::Rename(root.join("renamed.rs")), "renamed.txt").unwrap();
    wait(&mut cx, "the tab reopened as text", &|ws, _| {
        ws.panes[&id].shown().is_some_and(|t| t.path == root.join("renamed.txt") && t.language.is_none())
    });
    assert_eq!(ws.read_with(&cx, |ws, _| ws.panes[&id].tabs.iter().filter(|t| t.path.ends_with("renamed.txt")).count()), 1);

    // Renaming a folder takes its open tabs along.
    act(&mut cx, &|ws, w, cx| ws.open_file(&id, root.join("sub/inner.rs"), None, w, cx));
    wait(&mut cx, "inner.rs open", &|ws, _| shown(ws, &id) == Some(root.join("sub/inner.rs")));
    name(&mut cx, NameFor::Rename(root.join("sub")), "moved").unwrap();
    wait(&mut cx, "the tab following the folder", &|ws, _| shown(ws, &id) == Some(root.join("moved/inner.rs")));

    // Deleting a folder closes the tabs inside it.
    act(&mut cx, &|ws, _w, cx| ws.delete_path(id.clone(), root.join("moved"), cx));
    wait(&mut cx, "the folder's tabs closed", &|ws, _| ws.panes[&id].tabs.iter().all(|t| !t.path.starts_with(root.join("moved"))));
    assert!(!root.join("moved").exists());
    assert!(root.join("renamed.txt").exists(), "only the folder went");
    act(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
}

#[test]
#[ignore]
fn file_operations() {
    let t = std::thread::spawn(file_operations_in_the_tree);
    let start = std::time::Instant::now();
    while !t.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(180), "the workspace froze");
        std::thread::sleep(Duration::from_millis(100));
    }
    t.join().unwrap();
}

/// The line actions on their real keys in an open file: they act on the line, one undo step each, and cut and copy
/// keep working on a selection; Ctrl+W closes tabs. Runs the real `claude` in a new temporary folder, like `file_operations`.
/// `cargo test -- --ignored line_actions`.
fn line_actions_on_their_keys() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(work.path()).unwrap();
    std::fs::write(root.join("lines.txt"), "one\ntwo\nthree\nfour\n").unwrap();

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
        app.bind_keys(super::line_action_bindings());
    });
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx))))
        .unwrap();
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        draw(cx);
    };
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(60), "{what} never happened");
            std::thread::sleep(Duration::from_millis(100));
            draw(cx);
        }
    };
    act(&mut cx, &|ws, w, cx| ws.new_claude(root.clone(), w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    wait(&mut cx, "the session running", &|ws, app| ws.is_running(&id, app));
    act(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    wait(&mut cx, "the editor pane", &|ws, _| ws.panes.contains_key(&id));
    act(&mut cx, &|ws, w, cx| ws.open_file(&id, root.join("lines.txt"), None, w, cx));
    wait(&mut cx, "the file open", &|ws, _| ws.panes[&id].shown().is_some());
    let buffer = ws.read_with(&cx, |ws, _| ws.panes[&id].shown().unwrap().buffer.clone());
    act(&mut cx, &|_, w, cx| buffer.update(cx, |b, cx| {
        b.focus(w, cx);
        b.set_selected_range(5..5, cx);
    }));
    let secondary = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    let press = |cx: &mut TestAppContext, keys: &str| {
        cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse(&keys.replace("secondary", secondary)).unwrap());
        draw(cx);
    };
    let text = |cx: &TestAppContext| buffer.read_with(cx, |b, _| b.value().to_string());
    let selected = |cx: &TestAppContext| buffer.read_with(cx, |b, _| b.selected_range());
    let clipboard = |cx: &mut TestAppContext| cx.update(|app| app.read_from_clipboard()).and_then(|c| c.text());

    // Cut a line: it goes, with its newline, to the clipboard.
    press(&mut cx, "secondary-x");
    assert_eq!(text(&cx), "one\nthree\nfour\n");
    assert_eq!(clipboard(&mut cx).as_deref(), Some("two\n"));

    // Move it down, and one undo puts it back.
    press(&mut cx, "alt-down");
    assert_eq!(text(&cx), "one\nfour\nthree\n");
    press(&mut cx, "secondary-z");
    assert_eq!(text(&cx), "one\nthree\nfour\n", "moving a line is one undo step");

    // Duplicate upwards, then select the line whole.
    press(&mut cx, "shift-alt-up");
    assert_eq!(text(&cx), "one\nthree\nthree\nfour\n");
    press(&mut cx, "secondary-shift-up");
    assert_eq!(selected(&cx), 4..10, "the line, whole, with its newline");

    // With a selection, copy and cut are the editor's own.
    press(&mut cx, "secondary-c");
    assert_eq!(clipboard(&mut cx).as_deref(), Some("three\n"));
    press(&mut cx, "secondary-x");
    assert_eq!(text(&cx), "one\nthree\nfour\n");

    press(&mut cx, "secondary-shift-k");
    assert_eq!(text(&cx), "one\nfour\n");

    // Ctrl+W closes the shown tab and hands the keyboard to the one now shown, so it can be pressed again.
    std::fs::write(root.join("second.txt"), "two\n").unwrap();
    act(&mut cx, &|ws, w, cx| ws.open_file(&id, root.join("second.txt"), None, w, cx));
    wait(&mut cx, "the second file open", &|ws, _| ws.panes[&id].tabs.len() == 2);
    cx.update(|app| app.bind_keys([gpui::KeyBinding::new("secondary-w", super::CloseTab, Some("EditorPane"))]));
    press(&mut cx, "secondary-w");
    assert_eq!(ws.read_with(&cx, |ws, _| ws.panes[&id].tabs.iter().map(|t| t.path.clone()).collect::<Vec<_>>()), [root.join("lines.txt")]);
    let focused = ws.read_with(&cx, |ws, app| ws.panes[&id].shown().unwrap().buffer.read(app).focus_handle(app));
    assert!(cx.update_window(window, |_, w, _| focused.is_focused(w)).unwrap(), "the tab now shown has the keyboard");
    press(&mut cx, "secondary-w");
    // lines.txt has unsaved edits, so closing it asks first.
    assert!(ws.read_with(&cx, |ws, _| ws.modal.is_some() && ws.panes[&id].tabs.len() == 1));
    act(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
}

#[test]
#[ignore]
fn line_actions() {
    let t = std::thread::spawn(line_actions_on_their_keys);
    let start = std::time::Instant::now();
    while !t.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(180), "the workspace froze");
        std::thread::sleep(Duration::from_millis(100));
    }
    t.join().unwrap();
}


/// Double-clicking a word in the editor selects it. Runs the real `claude` in a new temporary folder, like
/// `file_operations`. `cargo test -- --ignored double_click`.
fn double_click_in_the_editor() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(work.path()).unwrap();
    std::fs::write(root.join("words.txt"), "hello brave world\nsecond line\n").unwrap();

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx))))
        .unwrap();
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let act = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        draw(cx);
    };
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            assert!(t.elapsed() < Duration::from_secs(60), "{what} never happened");
            std::thread::sleep(Duration::from_millis(100));
            draw(cx);
        }
    };
    act(&mut cx, &|ws, w, cx| ws.new_claude(root.clone(), w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    wait(&mut cx, "the session running", &|ws, app| ws.is_running(&id, app));
    act(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    wait(&mut cx, "the editor pane", &|ws, _| ws.panes.contains_key(&id));
    act(&mut cx, &|ws, w, cx| ws.open_file(&id, root.join("words.txt"), None, w, cx));
    wait(&mut cx, "the file open", &|ws, _| ws.panes[&id].shown().is_some());
    let buffer = ws.read_with(&cx, |ws, _| ws.panes[&id].shown().unwrap().buffer.clone());
    act(&mut cx, &|ws, w, cx| { let f = ws.panes[&id].focus.clone(); w.focus(&f, cx); });
    draw(&mut cx);

    // The middle of "brave", as a person would click it.
    // The panes slide into place; click once the text has stopped moving.
    let place = |cx: &mut TestAppContext| buffer.read_with(cx, |b, _| b.range_to_bounds(&(8..9)));
    let mut bounds = place(&mut cx).expect("the text is laid out");
    for _ in 0..200 {
        std::thread::sleep(Duration::from_millis(50));
        cx.executor().advance_clock(Duration::from_millis(50));
        draw(&mut cx);
        let now = place(&mut cx).expect("the text is laid out");
        if now == bounds {
            break;
        }
        bounds = now;
    }
    let at = bounds.center();
    let mut visual = gpui::VisualTestContext::from_window(window.into(), &cx);
    let none = gpui::Modifiers::default();
    for click_count in [1, 2] {
        visual.simulate_event(gpui::MouseDownEvent { position: at, modifiers: none, button: gpui::MouseButton::Left, click_count, first_mouse: false });
        visual.simulate_event(gpui::MouseUpEvent { position: at, modifiers: none, button: gpui::MouseButton::Left, click_count });
    }
    assert_eq!(buffer.read_with(&visual, |b, _| b.selected_range()), 6..11, "the word under the pointer");

    // Holding the second press and dragging selects whole words, forwards and backwards.
    let centre = |cx: &VisualTestContext, offset: usize| buffer.read_with(cx, |b, _| b.range_to_bounds(&(offset..offset + 1))).unwrap().center();
    for (from, to, selected) in [(8, 14, 6..17), (14, 2, 0..17)] {
        let (start, end) = (centre(&visual, from), centre(&visual, to));
        visual.simulate_event(gpui::MouseDownEvent { position: start, modifiers: none, button: gpui::MouseButton::Left, click_count: 2, first_mouse: false });
        visual.simulate_event(gpui::MouseMoveEvent { position: end, modifiers: none, pressed_button: Some(gpui::MouseButton::Left) });
        visual.simulate_event(gpui::MouseUpEvent { position: end, modifiers: none, button: gpui::MouseButton::Left, click_count: 2 });
        assert_eq!(buffer.read_with(&visual, |b, _| b.selected_range()), selected, "double-click on offset {from}, drag to {to}");
    }
    act(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
}

#[test]
#[ignore]
fn double_click() {
    let t = std::thread::spawn(double_click_in_the_editor);
    let start = std::time::Instant::now();
    while !t.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(180), "the workspace froze");
        std::thread::sleep(Duration::from_millis(100));
    }
    t.join().unwrap();
}
/// A changed file in a repository gets marks in the editor's gutter (see `panels::editor_pane::change_marks`).
#[test]
#[ignore]
fn git_change_marks_are_painted() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(work.path()).unwrap();
    let repo = git2::Repository::init(&cwd).unwrap();
    std::fs::create_dir_all(cwd.join("src")).unwrap();
    let file = cwd.join("src").join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\nfour\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "c", &tree, &[]).unwrap();
    }
    std::fs::write(&file, "one\nTWO\nthree\nfour\nfive\n").unwrap();
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
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        draw(cx);
        draw(cx);
    };
    draw(&mut cx);
    step(&mut cx, &|ws, w, cx| ws.new_claude(cwd.clone(), w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    step(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    step(&mut cx, &|ws, w, cx| ws.open_file(&id, file.clone(), None, w, cx));
    for _ in 0..100 {
        cx.executor().advance_clock(Duration::from_millis(50));
        draw(&mut cx);
        std::thread::sleep(Duration::from_millis(50));
    }
    // Each change is painted as a bar at the editor's left edge, above everything the editor drew.
    let (marks, bars) = cx
        .update_window(window, |_, window, app| {
            let marks = ws.read(app).panes[&id].shown().unwrap().marks.clone();
            let bars: Vec<_> = window.painted_quads().into_iter().filter(|q| q.bounds.size.width.0 < 8.0 && q.bounds.size.height.0 > 8.0).collect();
            (marks, bars)
        })
        .unwrap();
    assert_eq!(marks.len(), 2, "{marks:?}");
    assert_eq!(bars.len(), 2, "one bar for the changed line and one for the added line");
}

/// Scrolled down a long file, only the changes on screen get a bar, each beside its own line.
#[test]
#[ignore]
fn git_change_marks_follow_scrolling() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(work.path()).unwrap();
    let repo = git2::Repository::init(&cwd).unwrap();
    std::fs::create_dir_all(cwd.join("src")).unwrap();
    let file = cwd.join("src").join("a.txt");
    let base: String = (1..=150).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&file, &base).unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "c", &tree, &[]).unwrap();
    }
    let mut edited = base.replace("line 2
", "TWO
").replace("line 5
", "
").replace("line 120
", "CHANGED
").replace("line 121
", "");
    edited.push_str("extra
");
    std::fs::write(&file, edited).unwrap();
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
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        draw(cx);
        draw(cx);
    };
    draw(&mut cx);
    step(&mut cx, &|ws, w, cx| ws.new_claude(cwd.clone(), w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    step(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    step(&mut cx, &|ws, w, cx| ws.open_file(&id, file.clone(), Some(119), w, cx));
    for _ in 0..100 {
        cx.executor().advance_clock(Duration::from_millis(50));
        draw(&mut cx);
        std::thread::sleep(Duration::from_millis(50));
    }
    // Each change is painted as a bar at the editor's left edge, above everything the editor drew.
    let (marks, bars) = cx
        .update_window(window, |_, window, app| {
            let marks = ws.read(app).panes[&id].shown().unwrap().marks.clone();
            let bars: Vec<_> = window.painted_quads().into_iter().filter(|q| q.bounds.size.width.0 < 8.0 && q.bounds.size.height.0 > 8.0).collect();
            (marks, bars)
        })
        .unwrap();
    assert_eq!(marks.len(), 4, "{marks:?}");
    assert_eq!(bars.len(), 1, "only the change at line 120 is on screen: {bars:?}");
}

/// A wheel notch over the editor text moves it a little each frame instead of all at once.
#[test]
#[ignore]
fn the_editors_text_eases_to_a_wheel_notch() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
    let work = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(work.path()).unwrap();
    let repo = git2::Repository::init(&cwd).unwrap();
    std::fs::create_dir_all(cwd.join("src")).unwrap();
    let file = cwd.join("src").join("a.txt");
    let base: String = (1..=150).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&file, &base).unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "c", &tree, &[]).unwrap();
    }
    let mut edited = base.replace("line 2
", "TWO
").replace("line 5
", "
").replace("line 120
", "CHANGED
").replace("line 121
", "");
    edited.push_str("extra
");
    std::fs::write(&file, edited).unwrap();
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
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        draw(cx);
        draw(cx);
    };
    draw(&mut cx);
    step(&mut cx, &|ws, w, cx| ws.new_claude(cwd.clone(), w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());
    step(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    step(&mut cx, &|ws, w, cx| ws.open_file(&id, file.clone(), None, w, cx));
    for _ in 0..40 {
        cx.executor().advance_clock(Duration::from_millis(50));
        draw(&mut cx);
        std::thread::sleep(Duration::from_millis(20));
    }
    let buffer = ws.read_with(&cx, |ws, _| ws.panes[&id].shown().unwrap().buffer.clone());
    let offset = |cx: &TestAppContext| f32::from(buffer.read_with(cx, |b, _| b.scroll_offset().y));
    assert_eq!(offset(&cx), 0.0, "a file opens at its top");
    cx.update_window(window, |_, window, app| {
        let at = gpui::point(gpui::px(1000.0), gpui::px(400.0));
        window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position: at, pressed_button: None, modifiers: Default::default() }), app);
        let wheel = gpui::ScrollWheelEvent { position: at, delta: gpui::ScrollDelta::Lines(gpui::point(0.0, -3.0)), modifiers: Default::default(), touch_phase: gpui::TouchPhase::Moved };
        window.dispatch_event(gpui::PlatformInput::ScrollWheel(wheel), app);
    })
    .unwrap();
    let mut seen = vec![];
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(16));
        draw(&mut cx);
        seen.push(offset(&cx));
    }
    let end = *seen.last().unwrap();
    assert!(end < -20.0, "the text never scrolled: {seen:?}");
    let first_move = seen.iter().copied().find(|&o| o != 0.0).unwrap();
    assert!(first_move.abs() < end.abs() / 2.0, "the first frame jumped to {first_move} of {end}: {seen:?}");
}

/// The session list builds only the rows in view, yet scrolling a long list brings each row to where the pointer finds
/// it, and the row under the pointer is the one marked for its buttons.
#[test]
#[ignore]
fn the_session_list_draws_rows_where_scrolling_brings_them() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
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
    draw(&mut cx);
    // Newer than any real session the startup scan may add, so these come first, s000 at the top.
    cx.update(|app| {
        ws.update(app, |ws, cx| {
            for i in 0..300 {
                ws.store.data.sessions.push(crate::store::SessionRecord {
                    id: format!("s{i:03}"),
                    kind: SessionKind::Claude,
                    title: format!("Session {i}"),
                    custom_title: None,
                    auto_title: None,
                    cwd: PathBuf::from("/nowhere"),
                    project_id: None,
                    created_at: 0,
                    last_active: 4_000_000_000 - i,
                    exit_code: None,
                    hidden: false,
                    claude: None,
                });
            }
            ws.store.data.layout.older_open = true;
            cx.notify();
        })
    });
    draw(&mut cx);
    let at = gpui::point(gpui::px(120.0), gpui::px(400.0));
    // The pointer may sit in the gap between two rows, so a second spot a little lower is tried. It moves a pixel
    // sideways too: a row drawn under a still pointer learns it is hovered from the next move.
    let row_under_pointer = |cx: &mut TestAppContext| {
        let mut key = None;
        for (x, y) in [(121.0, 400.0), (120.0, 400.0), (121.0, 406.0), (120.0, 406.0)] {
            if key.is_some() {
                break;
            }
            let position = gpui::point(gpui::px(x), gpui::px(y));
            cx.update_window(window, |_, window, app| {
                window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent { position, pressed_button: None, modifiers: Default::default() }), app);
            })
            .unwrap();
            draw(cx);
            key = ws.read_with(cx, |ws, _| ws.hovered_row.clone());
        }
        let key = key.expect("a row under the pointer");
        key.strip_prefix("row-rec-s").and_then(|n| n.parse::<usize>().ok()).unwrap_or_else(|| panic!("not one of the test's rows: {key}"))
    };
    let before = row_under_pointer(&mut cx);
    cx.update_window(window, |_, window, app| {
        let wheel = gpui::ScrollWheelEvent { position: at, delta: gpui::ScrollDelta::Lines(gpui::point(0.0, -40.0)), modifiers: Default::default(), touch_phase: gpui::TouchPhase::Moved };
        window.dispatch_event(gpui::PlatformInput::ScrollWheel(wheel), app);
    })
    .unwrap();
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(16));
        cx.update_window(window, |_, window, app| window.simulate_next_frame(app)).unwrap();
        draw(&mut cx);
    }
    let after = row_under_pointer(&mut cx);
    assert!(after > before + 20, "row {before} before scrolling, {after} after");
}

/// With no `claude` found, starting a session says so and offers the setup window; asking for it twice shows one.
#[test]
#[ignore]
fn a_missing_claude_offers_the_setup_window() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
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

/// The worktree dialog and the switch of a session between worktrees, with a stand-in for `claude`, so what Claude Code
/// does in the new folder is not checked.
fn switching_between_worktrees_in_the_workspace() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    // Canonical, because the worktrees go below it and git reports their folders that way.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", dunce::canonicalize(data.path()).unwrap()) };
    // Claude Code's data directory, where the conversation of a session is looked for.
    let home = tempfile::tempdir().unwrap();
    // SAFETY: as above.
    unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", home.path()) };
    let work = tempfile::tempdir().unwrap();
    let repo_dir = dunce::canonicalize(work.path()).unwrap().join("proj");
    let repo = git2::Repository::init(&repo_dir).unwrap();
    {
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree = repo.find_tree(repo.index().unwrap().write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
    }
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let initial = vec![repo_dir.clone()];
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, initial, cx))))
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

    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace, &App) -> bool| {
        let t = std::time::Instant::now();
        let mut said = None;
        while !ws.read_with(cx, |ws, app| done(ws, app)) {
            said = ws.read_with(cx, |ws, _| ws.toast.as_ref().map(|t| t.text.clone())).or(said);
            assert!(t.elapsed() < Duration::from_secs(30), "{what} never happened; the last toast said {said:?}");
            std::thread::sleep(Duration::from_millis(100));
            cx.executor().advance_clock(Duration::from_millis(100));
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
    };
    let active = |ws: &Workspace| ws.store.session(ws.active.as_ref().unwrap()).unwrap().clone();
    // A stand-in for `claude` that shows a line and stays up whatever it is asked, since a real one would end at once on
    // a conversation that is not its own.
    let fake = work.path().join(if cfg!(windows) { "claude.cmd" } else { "claude" });
    std::fs::write(&fake, if cfg!(windows) { "@echo off\r\necho ready\r\nping -n 120 127.0.0.1 >nul\r\n" } else { "#!/bin/sh\necho ready\nsleep 120\n" }).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    ws.update(&mut cx, |ws, _| ws.claude_exe = Some(fake.clone()));

    // A new session starts in the main checkout. The menu is there but cannot switch until a session runs.
    step(&mut cx, &|ws, w, cx| ws.act(Act::NewClaude { cwd: repo_dir.clone() }, w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.modal.is_none() && ws.live.len() == 1));
    let session = ws.read_with(&cx, |ws, _| active(ws));
    assert_eq!(session.cwd, repo_dir);
    // The conversation exists once Claude Code has had a prompt; the test stands one in, in Claude Code's own place.
    let claude_id = session.claude_id().unwrap().to_string();
    let transcript = home.path().join("projects/p").join(format!("{claude_id}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "{\"type\":\"user\"}
").unwrap();
    wait(&mut cx, "the first screen", &|ws, app| ws.live.iter().all(|l| !l.view.read(app).is_loading() && !l.view.read(app).is_working()));

    // A bad name keeps the dialog open; a typed one moves the session into the worktree Claude Code will make.
    step(&mut cx, &|ws, w, cx| ws.act(Act::NewWorktree(repo_dir.clone()), w, cx));
    step(&mut cx, &|ws, w, cx| {
        let Some(Modal::NewWorktree { input, .. }) = &ws.modal else { panic!("no dialog") };
        input.clone().update(cx, |i, cx| i.set_value("bad name", w, cx));
        ws.submit_modal(w, cx);
    });
    assert!(ws.read_with(&cx, |ws, _| matches!(ws.modal, Some(Modal::NewWorktree { .. }))), "a bad name is refused");
    step(&mut cx, &|ws, w, cx| {
        let Some(Modal::NewWorktree { input, .. }) = &ws.modal else { panic!("no dialog") };
        input.clone().update(cx, |i, cx| i.set_value("fix-login", w, cx));
        ws.act(Act::PickBase(crate::git::worktree::Base::Current), w, cx);
        ws.submit_modal(w, cx);
    });
    let worktree = crate::git::worktree::folder(&crate::git::worktree::root(), &repo_dir, "fix-login");
    // The grid the pane draws terminals on. A new terminal starts at a default size until it is first laid out.
    let grid = ws.read_with(&cx, |ws, app| ws.live_view(&session.id).unwrap().read(app).terminal.size());
    assert_ne!(grid, (120, 32), "the window must not happen to match the default size");
    // Until the new process is up, the session stays shown and running behind its skeleton, never an empty pane, and
    // the skeleton sits on the pane's grid from the first frame on.
    let moved_to = |folder: &std::path::Path, ws: &Workspace, app: &App| {
        let loading = ws.live_view(&session.id).is_some_and(|v| v.read(app).is_loading());
        assert_eq!(ws.live_view(&session.id).map(|v| v.read(app).terminal.size()), Some(grid), "mid-switch the skeleton is on another grid");
        assert!(ws.active.as_ref() == Some(&session.id) && ws.is_running(&session.id, app), "mid-switch the session is not shown running");
        let moved = ws.store.session(&session.id).is_some_and(|r| r.cwd == folder);
        assert!(loading || moved, "mid-switch the pane has no skeleton");
        moved && !loading
    };
    wait(&mut cx, "the switch", &|ws, app| moved_to(&worktree, ws, app));
    let branch = std::process::Command::new("git").args(["branch", "--show-current"]).current_dir(&worktree).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "fix-login", "the branch is named as typed");
    assert_eq!(ws.read_with(&cx, |ws, _| ws.live.len()), 1, "the same session, not a new one");
    assert_eq!(ws.read_with(&cx, |ws, _| active(ws).id), session.id);
    assert_eq!(ws.read_with(&cx, |ws, _| active(ws).claude_id().map(str::to_string)), Some(claude_id.clone()), "the same conversation");
    assert_eq!(std::fs::read_to_string(&transcript).unwrap().lines().count(), 1, "making a worktree writes nothing to the conversation");

    // The worktree is in Claudiu's data folder, so the repository has nothing new for git to list and nothing to offer.
    assert!(worktree.starts_with(dunce::canonicalize(data.path()).unwrap().join("worktrees")), "{worktree:?}");
    let status = std::process::Command::new("git").args(["status", "--porcelain"]).current_dir(&repo_dir).output().unwrap();
    assert!(status.stdout.is_empty(), "the repository is untouched: {}", String::from_utf8_lossy(&status.stdout));
    assert!(ws.read_with(&cx, |ws, _| ws.toast.as_ref().is_none_or(|t| t.action.is_none())), "no toast offers an action");

    // Back to the main checkout, as a session that `claude --worktree` put in the worktree: Claude Code's record of the
    // worktree would win over the folder, so one line is appended.
    std::fs::write(worktree.join("wip.txt"), "x").unwrap();
    std::fs::write(&transcript, "{\"type\":\"user\"}
{\"type\":\"worktree-state\",\"worktreeSession\":{\"worktreeName\":\"fix-login\"},\"sessionId\":\"x\"}
").unwrap();
    wait(&mut cx, "the first screen again", &|ws, app| ws.live.iter().all(|l| !l.view.read(app).is_loading() && !l.view.read(app).is_working()));
    step(&mut cx, &|ws, w, cx| ws.act(Act::SwitchWorktree { folder: repo_dir.clone(), base: Default::default() }, w, cx));
    // The work file stays in the worktree the session leaves, so it asks first.
    wait(&mut cx, "the question", &|ws, _| matches!(&ws.modal, Some(Modal::Confirm { body, .. }) if body.contains("1 uncommitted file stays")));
    step(&mut cx, &|ws, w, cx| ws.submit_modal(w, cx));
    wait(&mut cx, "the switch back", &|ws, app| moved_to(&repo_dir, ws, app));
    assert!(!crate::claude::worktree_recorded(&transcript).unwrap(), "the transcript no longer names the worktree");
    assert!(std::fs::read_to_string(&transcript).unwrap().lines().count() == 3);

    // A restart (a switch, a hand-off) puts the new terminal on the pane's grid before its first frame, which is drawn
    // before the terminal is laid out: on a default grid the skeleton's prompt would jump to the middle for a frame.
    step(&mut cx, &|ws, w, cx| {
        ws.restart_session(&session.id, w, cx);
        assert_eq!(ws.live_view(&session.id).map(|v| v.read(cx).terminal.size()), Some(grid), "the new terminal starts on another grid");
    });
    wait(&mut cx, "the restart", &|ws, app| ws.live_view(&session.id).is_some_and(|v| !v.read(app).is_loading()));

    // Mid-turn nothing changes: a session that has not shown its first screen is not idle.
    step(&mut cx, &|ws, w, cx| ws.act(Act::SwitchWorktree { folder: worktree.clone(), base: Default::default() }, w, cx));
    // Either the check ran before the screen was ready (told to wait) or after (switched); it never half-switches.
    wait(&mut cx, "a settled state", &|ws, app| ws.is_running(&session.id, app) && ws.live_view(&session.id).is_some_and(|v| !v.read(app).is_loading()));

    // A project without commits has a disabled "New worktree…".
    let bare = dunce::canonicalize(work.path()).unwrap().join("bare");
    git2::Repository::init(&bare).unwrap();
    let bare_project = ws.update(&mut cx, |ws, _| ws.store.add_project(&bare));
    let bare_cwd = bare.clone();
    let at = crate::panels::widgets::Dropdown { pos: gpui::point(gpui::px(100.0), gpui::px(30.0)), min_w: gpui::px(0.0) };
    step(&mut cx, &|ws, _w, cx| ws.open_worktree_menu(bare_project.clone(), bare_cwd.clone(), crate::panels::widgets::Dropdown { pos: at.pos, min_w: at.min_w }, cx));
    let last = ws.read_with(&cx, |ws, _| ws.menu.as_ref().and_then(|m| m.items.last().map(|i| (i.label.clone(), i.disabled, i.hint.clone()))));
    assert!(matches!(&last, Some((label, true, Some(hint))) if label == "New worktree…" && hint.contains("no commits")), "{last:?}");
}

#[test]
#[ignore]
fn switching_between_worktrees() {
    let t = std::thread::spawn(switching_between_worktrees_in_the_workspace);
    let start = std::time::Instant::now();
    while !t.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(120), "the workspace froze");
        std::thread::sleep(Duration::from_millis(100));
    }
    t.join().unwrap();
}

/// A running session refuses the removal of its worktree. Ending it offers the removal, and confirming removes the folder
/// (with a real `claude` running in it until the session ends, which on Windows holds the folder until the process is gone).
fn removing_a_worktree_when_its_session_ends() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    // Canonical, because the worktrees go below it and git reports their folders that way.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", dunce::canonicalize(data.path()).unwrap()) };
    let work = tempfile::tempdir().unwrap();
    let repo_dir = dunce::canonicalize(work.path()).unwrap().join("proj");
    let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&repo_dir).output().unwrap();
    std::fs::create_dir_all(&repo_dir).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"], &["commit", "-q", "--allow-empty", "-m", "one"]] {
        git(args);
    }
    let folder = crate::git::worktree::folder(&crate::git::worktree::root(), &repo_dir, "fix");
    git(&["worktree", "add", "-q", "-b", "worktree-fix", &folder.to_string_lossy()]);

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let initial = vec![repo_dir.clone()];
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, initial, cx))))
        .unwrap();
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        for _ in 0..2 {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
    };
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, _| done(ws)) {
            assert!(t.elapsed() < Duration::from_secs(30), "{what} never happened: {:?}", ws.read_with(cx, |ws, _| ws.toast.as_ref().map(|t| t.text.clone())));
            std::thread::sleep(Duration::from_millis(100));
            cx.executor().advance_clock(Duration::from_millis(100));
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }
    };
    // The worktree is known to the project once git has been read again.
    step(&mut cx, &|ws, _w, _cx| ws.store.refresh_git());
    // Settings lists the worktree folders in the data folder: the one git lists can be removed with git, a leftover
    // folder of a repository that is gone only deleted.
    let orphan = crate::git::worktree::root().join("old-00000001").join("stale");
    std::fs::create_dir_all(&orphan).unwrap();
    step(&mut cx, &|ws, w, cx| ws.act(Act::OpenSettings, w, cx));
    wait(&mut cx, "the worktree rows", &|ws| ws.worktree_rows.len() == 2);
    let rows = ws.read_with(&cx, |ws, _| ws.worktree_rows.clone());
    let listed = rows.iter().find(|r| r.folder == folder).expect("the worktree git lists");
    assert_eq!(listed.main_checkout.as_deref(), Some(repo_dir.as_path()));
    assert_eq!(listed.label, repo_dir.display().to_string());
    let stale = rows.iter().find(|r| r.folder == orphan).expect("the leftover folder");
    assert_eq!((stale.main_checkout.as_ref(), stale.label.as_str()), (None, "old"));
    step(&mut cx, &|ws, w, cx| ws.act(Act::ConfirmDeleteWorktreeFolder(orphan.clone()), w, cx));
    wait(&mut cx, "the leftover folder to go", &|_| !orphan.exists());
    step(&mut cx, &|ws, _w, _cx| ws.modal = None);
    // A new session never starts inside a worktree; Fix-with-Claude is the one starter that keeps the folder it is given.
    step(&mut cx, &|ws, w, cx| ws.new_claude_with(folder.clone(), vec![], crate::claude::defaults::Starter::FixGit, w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());

    // Any other way of starting a session, asked to start inside the worktree, starts in the main checkout.
    step(&mut cx, &|ws, w, cx| ws.new_claude(folder.clone(), w, cx));
    let (other, started_in) = ws.read_with(&cx, |ws, _| (ws.active.clone().unwrap(), ws.store.session(ws.active.as_ref().unwrap()).unwrap().cwd.clone()));
    assert_eq!(started_in, repo_dir);
    step(&mut cx, &|ws, w, cx| ws.close_live(&other, w, cx));
    step(&mut cx, &|ws, w, cx| ws.act(Act::Select(id.clone()), w, cx));

    // The top-bar menu offers the removal for the session's own worktree, and not in the main checkout.
    let project = ws.read_with(&cx, |ws, _| ws.store.data.projects[0].id.clone());
    let menu_of = |cx: &mut TestAppContext, cwd: PathBuf| {
        let at = crate::panels::widgets::Dropdown { pos: gpui::point(gpui::px(100.0), gpui::px(30.0)), min_w: gpui::px(0.0) };
        step(cx, &|ws, _w, cx| ws.open_worktree_menu(project.clone(), cwd.clone(), crate::panels::widgets::Dropdown { pos: at.pos, min_w: at.min_w }, cx));
        // Each row's label and whether it has an x.
        ws.read_with(cx, |ws, _| ws.menu.as_ref().map(|m| m.items.iter().map(|i| (i.label.trim().to_string(), i.remove.is_some())).collect::<Vec<_>>()).unwrap_or_default())
    };
    for cwd in [folder.clone(), repo_dir.clone()] {
        let rows = menu_of(&mut cx, cwd);
        assert_eq!(rows[0].0, "Worktrees");
        assert!(rows[1].0.contains("main checkout") && !rows[1].1, "the main checkout has no x: {rows:?}");
        assert!(rows[2].0.contains("fix") && rows[2].1, "the worktree has an x: {rows:?}");
        assert!(rows.last().is_some_and(|r| r.0 == "New worktree…" && !r.1), "{rows:?}");
    }

    // The x on the worktree a session is running in is refused, naming the session.
    let own_x = ws.read_with(&cx, |ws, _| ws.menu.as_ref().and_then(|m| m.items[2].remove.clone()));
    let Some(x) = own_x else { panic!("no x on the worktree's row") };
    step(&mut cx, &|ws, w, cx| ws.act(x.clone(), w, cx));
    let toast = ws.read_with(&cx, |ws, _| ws.toast.as_ref().map(|t| t.text.clone()));
    assert!(toast.as_deref().is_some_and(|t| t.contains("is in fix. Switch it to main first.")), "{toast:?}");
    assert!(folder.exists() && ws.read_with(&cx, |ws, _| ws.modal.is_none() && ws.live.len() == 1), "nothing changes");

    // Ending the session by itself offers the removal.
    step(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
    wait(&mut cx, "the offer", &|ws| matches!(&ws.modal, Some(Modal::Confirm { act: Act::ConfirmRemoveWorktree { .. }, .. })));
    step(&mut cx, &|ws, w, cx| ws.submit_modal(w, cx));
    wait(&mut cx, "the folder to go", &|_| !folder.exists());
    let toast = ws.read_with(&cx, |ws, _| ws.toast.as_ref().map(|t| t.text.clone()));
    assert!(toast.as_deref().is_some_and(|t| t.contains("Removed worktree fix")), "{toast:?}");
    assert!(!git(&["rev-parse", "--verify", "-q", "worktree-fix"]).status.success(), "the merged branch went with it");

    // The branch menu has a title and an x on every local branch but the current one.
    assert!(git(&["branch", "spare"]).status.success());
    let at = crate::panels::widgets::Dropdown { pos: gpui::point(gpui::px(100.0), gpui::px(30.0)), min_w: gpui::px(0.0) };
    step(&mut cx, &|ws, _w, cx| ws.open_branch_menu(repo_dir.clone(), crate::panels::widgets::Dropdown { pos: at.pos, min_w: at.min_w }, cx));
    wait(&mut cx, "the branch menu", &|ws| ws.menu.is_some());
    let rows = ws.read_with(&cx, |ws, _| ws.menu.as_ref().map(|m| m.items.iter().map(|i| (i.label.trim().to_string(), i.remove.clone())).collect::<Vec<_>>()).unwrap_or_default());
    assert_eq!(rows[0].0, "Branches");
    let spare = rows.iter().find(|r| r.0 == "spare").and_then(|r| r.1.clone());
    assert!(spare.is_some(), "a local branch has an x: {rows:?}");
    assert!(rows.iter().find(|r| r.0.starts_with('✓')).is_some_and(|r| r.1.is_none()), "the current branch has none: {rows:?}");

    let x = spare.unwrap();
    step(&mut cx, &|ws, w, cx| ws.act(x.clone(), w, cx));
    wait(&mut cx, "the question", &|ws| matches!(&ws.modal, Some(Modal::Confirm { confirm, .. }) if confirm == "Delete"));
    step(&mut cx, &|ws, w, cx| ws.submit_modal(w, cx));
    wait(&mut cx, "the branch to go", &|_| !git(&["rev-parse", "--verify", "-q", "spare"]).status.success());
}

#[test]
#[ignore]
fn removing_a_worktree() {
    let t = std::thread::spawn(removing_a_worktree_when_its_session_ends);
    let start = std::time::Instant::now();
    while !t.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(120), "the workspace froze");
        std::thread::sleep(Duration::from_millis(100));
    }
    t.join().unwrap();
}
