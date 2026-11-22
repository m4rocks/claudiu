use super::*;

/// The comment button beside the editor's selection opens a box whose Enter adds `@file#L..: comment` to the message
/// box, on a line of its own after what is typed there. Moving the keyboard to the chat adds nothing by itself. Runs the
/// real `claude` in this repository on a file under `target`. `cargo test -- --ignored line_comment`.
fn line_comment_into_the_message_box() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let root = dunce::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
    let work = tempfile::tempdir_in(root.join("target")).unwrap();
    let code = work.path().join("code.rs");
    std::fs::write(&code, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    let path = crate::app::picker::relative(&dunce::canonicalize(&code).unwrap(), &root);

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
    let draft = |ws: &Workspace, app: &App| ws.live_view(&id).unwrap().thread().unwrap().read(app).draft(app);
    wait(&mut cx, "the session to open", &|ws, app| !ws.live_view(&id).unwrap().is_loading(app));
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
    assert_eq!(ws.read_with(&cx, |ws, app| draft(ws, app)), "", "moving to the chat adds nothing");

    act(&mut cx, &|ws, w, cx| ws.live_view(&id).unwrap().thread().unwrap().clone().update(cx, |v, cx| v.paste_text("look at these", w, cx)));
    act(&mut cx, &|ws, w, cx| ws.start_line_comment(0, gpui::point(gpui::px(100.0), gpui::px(100.0)), w, cx));
    let input = ws.read_with(&cx, |ws, _| ws.panes[&id].comment.as_ref().expect("the comment box is up").input.clone());
    act(&mut cx, &|_, w, cx| input.update(cx, |i, cx| i.set_value("why two?", w, cx)));
    cx.simulate_keystrokes(window, "enter");
    draw(&mut cx);
    assert!(ws.read_with(&cx, |ws, _| ws.panes[&id].comment.is_none()), "Enter closes the box");
    assert_eq!(ws.read_with(&cx, |ws, app| draft(ws, app)), format!("look at these\n@{path}#L2: why two?"), "the selection, not the line clicked");
    act(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
}

#[test]
#[ignore]
fn line_comment() {
    let t = std::thread::spawn(line_comment_into_the_message_box);
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
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
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
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
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
        cx.dispatch_keystroke(window, gpui::Keystroke::parse(&keys.replace("secondary", secondary)).unwrap());
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
    let focused = ws.read_with(&cx, |ws, app| ws.panes[&id].shown().unwrap().buffer.focus_handle(app));
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
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
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
    let mut visual = gpui::VisualTestContext::from_window(window, &cx);
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
