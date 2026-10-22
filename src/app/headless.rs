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
    // Chat: the Chat tab lists chats; a chat runs the real adapter (installed into the throwaway data folder
    // on first use), offers the agent's skills, answers, and comes back with its history after being closed.
    step(&mut cx, &|ws, w, cx| {
        ws.new_chat(None, None, w, cx);
    });
    let (id, view) = ws.read_with(&cx, |ws, _| (ws.chats[0].id.clone(), ws.chats[0].view.clone()));
    let wait = |cx: &mut TestAppContext, view: &Entity<crate::chat::view::ChatView>, what: &str, ok: &dyn Fn(&crate::chat::view::ChatView) -> bool| {
        let t = std::time::Instant::now();
        while !view.read_with(cx, |v, _| ok(v)) {
            assert!(t.elapsed() < Duration::from_secs(150), "timed out waiting for {what}: {:?}", view.read_with(cx, |v, _| v.probe()));
            std::thread::sleep(Duration::from_millis(100));
            draw(cx);
        }
    };
    wait(&mut cx, &view, "the chat to be ready", &|v| v.probe().0 == crate::chat::view::Phase::Ready);
    assert!(view.read_with(&cx, |v, _| v.probe().2) > 0, "the agent lists skills and commands");
    let rec = ws.read_with(&cx, |ws, _| ws.chat_store.get(&id).cloned()).unwrap();
    assert!(!is_tied(&rec.cwd), "a new chat belongs to no project");
    // Tie it to a project before the first message: a new conversation in that folder.
    let before = view.read_with(&cx, |v, _| v.epoch());
    step(&mut cx, &|_, w, cx| view.update(cx, |v, cx| v.set_project(Some(cwd.clone()), w, cx)));
    assert_eq!(view.read_with(&cx, |v, _| v.epoch()), before + 1, "a different folder restarts the conversation");
    wait(&mut cx, &view, "the chat to restart in the project", &|v| v.probe().0 == crate::chat::view::Phase::Ready);
    assert!(ws.read_with(&cx, |ws, _| ws.chat_store.get(&id).is_some_and(|r| is_tied(&r.cwd) && r.session_id.is_some())));
    step(&mut cx, &|_, w, cx| view.update(cx, |v, cx| v.send_text("say only the word pong", w, cx)));
    wait(&mut cx, &view, "the answer", &|v| !v.is_busy() && v.probe().1.iter().any(|i| matches!(i, crate::chat::model::Item::Agent(_))));
    let reply = view.read_with(&cx, |v, _| format!("{:?}", v.probe().1)).to_lowercase();
    assert!(reply.contains("pong"), "{reply}");
    // Image paste: a picture on the clipboard attaches, is sent with the message, and Claude sees it.
    let png = {
        let mut out = std::io::Cursor::new(vec![]);
        ::image::RgbaImage::from_pixel(48, 48, ::image::Rgba([255, 0, 0, 255])).write_to(&mut out, ::image::ImageFormat::Png).unwrap();
        out.into_inner()
    };
    cx.update(|app| app.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(gpui::ImageFormat::Png, png))));
    step(&mut cx, &|_, w, cx| view.update(cx, |v, cx| v.focus_input(w, cx)));
    step(&mut cx, &|_, w, cx| assert!(view.update(cx, |v, cx| v.paste(w, cx)), "an image on the clipboard is taken"));
    assert_eq!(view.read_with(&cx, |v, _| v.attached()), 1);
    step(&mut cx, &|_, w, cx| view.update(cx, |v, cx| v.send_text("What single color fills this image? Answer with one lowercase word.", w, cx)));
    assert_eq!(view.read_with(&cx, |v, _| v.attached()), 0, "the attachment is sent");
    wait(&mut cx, &view, "the color", &|v| !v.is_busy() && v.probe().1.iter().rev().any(|i| matches!(i, crate::chat::model::Item::Agent(t) if t.to_lowercase().contains("red"))));
    // Account switch: the chat restarts under the active account and loads the same conversation.
    let before = view.read_with(&cx, |v, _| v.epoch());
    step(&mut cx, &|ws, w, cx| ws.relaunch_chats(w, cx));
    assert_eq!(view.read_with(&cx, |v, _| v.epoch()), before + 1, "the agent was restarted");
    wait(&mut cx, &view, "the chat to reload", &|v| v.probe().0 == crate::chat::view::Phase::Ready && v.probe().1.len() >= 4);
    let rec = ws.read_with(&cx, |ws, _| ws.chat_store.get(&id).cloned()).unwrap();
    assert!(rec.session_id.is_some() && rec.title != DEFAULT_TITLE, "{rec:?}");
    step(&mut cx, &|ws, w, cx| ws.close_live(&id, w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.chats.is_empty() && ws.chat_store.get(&id).is_some()), "closing keeps the history");
    // Nothing may keep the view (and with it the adapter and its `claude`) alive once the chat is closed.
    let weak = view.downgrade();
    drop(view);
    draw(&mut cx);
    draw(&mut cx);
    assert!(weak.upgrade().is_none(), "a closed chat's view is still alive");
    step(&mut cx, &|ws, w, cx| ws.open_chat(&id, w, cx));
    let view = ws.read_with(&cx, |ws, _| ws.chats[0].view.clone());
    wait(&mut cx, &view, "the history to load", &|v| {
        let (phase, items, _) = v.probe();
        phase == crate::chat::view::Phase::Ready && items.iter().any(|i| matches!(i, crate::chat::model::Item::User(t) if t.contains("pong")))
    });
    // Cross-session tools are answered on the spot, and one that can't work says why.
    use serde_json::{Value, json};
    let ask = |from: String, tool: &'static str, args: Value| std::thread::spawn(move || crate::bridge::ask(&from, tool, &args));
    let take = || loop {
        if let Some(r) = crate::bridge::take_requests().pop() {
            break r;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let serve = |cx: &mut TestAppContext, req: crate::bridge::Request| step(cx, &|ws, w, cx| ws.serve_requests(vec![req.clone()], w, cx));
    let from_chat = format!("chat:{id}");

    let h = ask(from_chat.clone(), "list_chats", json!({}));
    serve(&mut cx, take());
    let text = h.join().unwrap().unwrap();
    assert!(text.contains(&id) && text.contains("this chat"), "{text}");

    // A request whose asker gave up is dropped.
    let stale = crate::bridge::Request { id: "late".into(), from: from_chat.clone(), tool: "start_chat".into(), args: json!({}), deadline: now() - 1 };
    serve(&mut cx, stale);
    assert_eq!(ws.read_with(&cx, |ws, _| ws.chats.len()), 1, "an expired request was carried out");

    let h = ask(from_chat.clone(), "read_chat", json!({"id": id}));
    serve(&mut cx, take());
    let text = h.join().unwrap().unwrap();
    assert!(text.contains("pong"), "read_chat returns the messages: {text}");

    let h = ask(from_chat.clone(), "start_code_session", json!({"folder": "no-such-project"}));
    serve(&mut cx, take());
    assert!(h.join().unwrap().unwrap_err().contains("No project or folder"));

    let h = ask("code:someone".into(), "start_chat", json!({"prompt": "say only the word ping"}));
    serve(&mut cx, take());
    h.join().unwrap().unwrap();
    assert_eq!(ws.read_with(&cx, |ws, _| ws.chats.len()), 2);
    let (second_id, second) = ws.read_with(&cx, |ws, _| (ws.chats[1].id.clone(), ws.chats[1].view.clone()));
    assert!(!ws.read_with(&cx, |ws, _| is_tied(&ws.chat_store.get(&second_id).unwrap().cwd)), "a chat opened by a session has no project either");
    wait(&mut cx, &second, "the first message to be answered", &|v| !v.is_busy() && v.probe().1.iter().any(|i| matches!(i, crate::chat::model::Item::Agent(t) if t.to_lowercase().contains("ping"))));

    let live_before = ws.read_with(&cx, |ws, _| ws.live.len());
    let h = ask(from_chat.clone(), "start_code_session", json!({"folder": cwd.to_string_lossy()}));
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
    let h = ask(from_chat.clone(), "read_code_session", json!({"id": code_id}));
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
    assert_eq!(ws.read_with(&cx, |ws, _| ws.right_width(1600.0)), RIGHT_MENU_W, "the menu has a fixed width");
    step(&mut cx, &|ws, w, cx| ws.open_github(cwd.clone(), w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.gh.len() == 1 && ws.right_gh.is_some()), "github shown in the right sidebar");
    assert_eq!(ws.read_with(&cx, |ws, _| ws.right_width(1600.0)), RIGHT_GH_W);
    step(&mut cx, &|ws, _w, _cx| {
        ws.right_drag = Some(RightDrag { x: 1000.0, width: RIGHT_GH_W });
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
