use super::*;

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
            ws.store.data.layout.recent_open = true;
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

