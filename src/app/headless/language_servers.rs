use super::*;

/// Editor mode with a real rust-analyzer: diagnostics arrive, hover, go-to-definition and completion answer, and a language whose
/// server isn't installed says so. Uses `rust-analyzer` from `M4CODE_TEST_RUST_ANALYZER_DIR` (prepended to PATH),
/// else PATH. `cargo test -- --ignored language_servers`.
fn language_servers_with_rust_analyzer() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe {
        std::env::set_var("M4CODE_DATA_DIR", data.path());
        if let Some(dir) = std::env::var_os("M4CODE_TEST_RUST_ANALYZER_DIR") {
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

