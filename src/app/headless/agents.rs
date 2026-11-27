use super::*;

#[test]
fn handoffs_reuse_provider_sessions_and_restore_the_source_after_failure() {
    use crate::harness::thread::Entry;
    let _dirs = crate::datadir::use_throwaway_folders();
    let work = tempfile::tempdir().unwrap();
    let failed = tempfile::tempdir().unwrap();
    let server = fake_agent(work.path());
    let failed_server = fake_agent(failed.path());
    let failed_script = PathBuf::from(&failed_server.1[0]);
    let contents = std::fs::read_to_string(&failed_script).unwrap().replace(r#""result": {"sessionId": "fake-session"}"#, r#""error": {"code": -32603, "message": "handshake failed"}"#);
    std::fs::write(failed_script, contents).unwrap();
    let script = PathBuf::from(&server.1[0]);
    let contents = std::fs::read_to_string(&script).unwrap().replace(
        "elif method == \"session/load\":\n        cwd = m[\"params\"][\"cwd\"]",
        "elif method == \"session/load\":\n        cwd = m[\"params\"][\"cwd\"]\n        send({\"jsonrpc\": \"2.0\", \"method\": \"session/update\", \"params\": {\"sessionId\": m[\"params\"][\"sessionId\"], \"update\": {\"sessionUpdate\": \"user_message_chunk\", \"content\": {\"type\": \"text\", \"text\": \"native replay\"}}}})",
    );
    let contents = contents
        .replace("for line in sys.stdin:", r#"adapter = sys.argv[1]
modes = {
    "claude": ["default", "plan", "auto", "bypassPermissions"],
    "opencode": ["plan", "build"],
    "antigravity": ["default", "plan", "auto"],
}[adapter]
mode = modes[0]
def session_modes():
    return {"currentModeId": mode, "availableModes": [{"id": name, "name": name} for name in modes]}
for line in sys.stdin:"#)
        .replace(r#""result": {"sessionId": "fake-session"}"#, r#""result": {"sessionId": "fake-session", "modes": session_modes()}"#)
        .replace(r#"    elif method == "session/load":"#, r#"    elif method in ["session/load", "session/resume"]:"#)
        .replace(r#"        send({"jsonrpc": "2.0", "id": i, "result": {}})
    else:"#,
r#"        send({"jsonrpc": "2.0", "id": i, "result": {"modes": session_modes()}})
    elif method == "session/set_mode":
        mode = m["params"]["modeId"]
        send({"jsonrpc": "2.0", "id": i, "result": {}})
    else:"#)
        .replace(r#"        text = m["params"]["prompt"][0]["text"]"#, r#"        text = m["params"]["prompt"][0]["text"]
        with open(__file__ + ".permissions", "a", newline="\n") as log:
            log.write(f"{adapter}:{mode}:{'--dangerously-skip-permissions' in sys.argv}\n")"#);
    std::fs::write(&script, contents).unwrap();
    let server_for = |adapter: &str| {
        let mut server = server.clone();
        server.1.push(adapter.into());
        server
    };
    let state = tempfile::tempdir().unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        cx.run_until_parked();
    };
    step(&mut cx, &|ws, window, cx| {
        // Other parallel workspace tests persist their open sessions to the shared default store.
        // Keep deferred restore_open from restoring one of their adapters into this fixture.
        ws.store = crate::store::Store::load(state.path().join("state.json"));
        ws.acp_server = Some(server_for("claude"));
        ws.claude_exe = Some("unused-by-the-fake-agent".into());
        assert!(ws.new_session_from_home(Harness::Claude, None, work.path().to_path_buf(), Some("initial task".into()), None, Some(Permission::Auto), window, cx));
    });
    let id = ws.read_with(&cx, |ws, _| ws.active_id().unwrap().clone());
    let wait = |cx: &mut TestAppContext, harness| {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            if ws.read_with(cx, |ws, app| {
                ws.pending_handoffs.is_empty() && ws.live_view(&id).and_then(|v| v.thread()).is_some_and(|view| {
                    let view = view.read(app);
                    view.harness == harness && !view.is_loading() && !view.is_working() && view.thread.entries.iter().any(|e| matches!(e, Entry::Agent(text) if text == "ok"))
                })
            }) { break; }
            if std::time::Instant::now() >= until {
                ws.read_with(cx, |ws, app| {
                    let view = ws.live_view(&id).and_then(SessionView::thread).map(|view| view.read(app));
                    panic!("handoff to {harness:?} never settled: pending={}, view={:?}, toast={:?}",
                        ws.pending_handoffs.contains_key(&id),
                        view.map(|view| (view.harness, view.is_loading(), view.is_working(), view.start_error(), &view.thread.entries)),
                        ws.toast.as_ref().map(|toast| &toast.text));
                });
            }
            std::thread::yield_now();
        }
    };
    wait(&mut cx, Harness::Claude);
    for (harness, message, expected) in [
        (Harness::OpenCode, "continue on B", Permission::FullAccess),
        (Harness::Claude, "continue on A", Permission::Auto),
    ] {
        step(&mut cx, &|ws, window, cx| {
            ws.acp_server = Some(server_for(harness.key()));
            ws.dynamic_handoff(&id, harness, None, message.into(), false, None, window, cx);
        });
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        let mut visual = VisualTestContext::from_window(window, &cx);
        assert!(visual.debug_bounds("thread-mode").is_some(), "the permission chip stays visible through the handoff");
        wait(&mut cx, harness);
        ws.read_with(&cx, |ws, app| {
            let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
            assert_eq!(view.permission(), Some(expected));
        });
        if harness == Harness::OpenCode {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            let mut visual = VisualTestContext::from_window(window, &cx);
            let chip = visual.debug_bounds("thread-mode").unwrap().center();
            visual.simulate_event(gpui::MouseDownEvent { position: chip, modifiers: Default::default(), button: gpui::MouseButton::Left, click_count: 1, first_mouse: false });
            visual.simulate_event(gpui::MouseUpEvent { position: chip, modifiers: Default::default(), button: gpui::MouseButton::Left, click_count: 1 });
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            assert!(visual.debug_bounds("perm-mode-0").is_some());
            let auto = visual.debug_bounds("perm-mode-1").expect("Auto is visible even on OpenCode").center();
            assert!(visual.debug_bounds("perm-mode-2").is_some());
            visual.simulate_event(gpui::MouseDownEvent { position: auto, modifiers: Default::default(), button: gpui::MouseButton::Left, click_count: 1, first_mouse: false });
            visual.simulate_event(gpui::MouseUpEvent { position: auto, modifiers: Default::default(), button: gpui::MouseButton::Left, click_count: 1 });
            ws.read_with(&cx, |ws, app| {
                let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
                assert_eq!(view.picker, Some(crate::thread::ThreadPicker::Permission), "disabled Auto cannot be selected");
            });
        }
    }
    assert_eq!(
        std::fs::read_to_string(work.path().join("fake_agent.py.permissions")).unwrap(),
        "claude:auto:False\nopencode:build:False\nclaude:auto:False\n",
        "the destination must apply the permission before each prompt, including after a resume"
    );
    ws.read_with(&cx, |ws, app| {
        let meta = ws.store.session(&id).unwrap().agent.as_ref().unwrap();
        assert_eq!(meta.provider_sessions.len(), 2);
        assert_eq!(meta.session_id, "fake-session");
        let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
        let users: Vec<_> = view.thread.entries.iter().filter_map(|e| match e { Entry::User(text) => Some(text.as_str()), _ => None }).collect();
        assert_eq!(users, ["initial task", "continue on B", "continue on A"], "provider replay must not replace or duplicate m4code's history");
        let prompts = std::fs::read_to_string(work.path().join("fake_agent.py.prompts")).unwrap();
        let last = prompts.split("\n---\n").filter(|text| !text.is_empty()).last().unwrap();
        assert!(last.contains("continue on B") && last.contains("continue on A"));
        assert!(!last.contains("initial task"), "the returning provider already has its earlier context");
        assert!(prompts.contains("loaded fake-session"));
    });
    step(&mut cx, &|ws, window, cx| {
        ws.acp_server = Some(failed_server.clone());
        ws.dynamic_handoff(&id, Harness::Codex, None, "keep this draft".into(), false, None, window, cx);
    });
    wait(&mut cx, Harness::Claude);
    ws.read_with(&cx, |ws, app| {
        assert_eq!(ws.store.session(&id).unwrap().agent.as_ref().unwrap().harness, Harness::Claude);
        let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
        assert_eq!(view.draft(app), "keep this draft");
        assert!(view.is_running());
        assert_eq!(view.thread.entries.iter().filter(|entry| matches!(entry,
            Entry::Notice { text, error: true, .. } if text.contains("handshake failed"))).count(), 1);
        assert!(view.thread.active_notice(crate::harness::thread::NoticeKind::Handoff).is_none());
    });
    step(&mut cx, &|ws, window, cx| {
        ws.acp_server = Some(server_for("antigravity"));
        ws.dynamic_handoff(&id, Harness::Antigravity, None, "continue on C".into(), false, None, window, cx);
    });
    wait(&mut cx, Harness::Antigravity);
    for permission in [Permission::FullAccess, Permission::ReadOnly] {
        step(&mut cx, &|ws, _, cx| {
            let view = ws.live_view(&id).unwrap().thread().unwrap().clone();
            view.update(cx, |view, cx| view.set_permission(permission, cx));
        });
        wait(&mut cx, Harness::Antigravity);
        step(&mut cx, &|ws, window, cx| {
            let meta = ws.store.session(&id).unwrap().agent.as_ref().unwrap();
            assert_eq!(meta.session_id, "fake-session");
            assert_eq!(meta.launched_full_access, permission == Permission::FullAccess);
            let view = ws.live_view(&id).unwrap().thread().unwrap().clone();
            assert_eq!(view.read(cx).permission(), Some(permission));
            assert_eq!(view.read(cx).draft(cx), "keep this draft");
            view.update(cx, |view, cx| view.send("check permission".into(), window, cx));
        });
        wait(&mut cx, Harness::Antigravity);
    }
    let permissions = std::fs::read_to_string(work.path().join("fake_agent.py.permissions")).unwrap();
    assert!(permissions.ends_with("antigravity:auto:False\nantigravity:default:True\nantigravity:plan:False\n"));
    step(&mut cx, &|ws, window, cx| ws.end_live(&id, window, cx));
}

#[test]
fn compaction_preserves_the_session_and_never_becomes_a_user_message() {
    use crate::harness::thread::Entry;
    use serde_json::Value;
    let _dirs = crate::datadir::use_throwaway_folders();
    for harness in [Harness::Codex, Harness::Antigravity] {
        let work = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        cx.update(|app| {
            gpui_kit::init(app);
            crate::panels::theme::install(app);
        });
        let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
        let state = tempfile::tempdir().unwrap();
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| {
            ws.store = crate::store::Store::load(state.path().join("state.json"));
            ws.acp_server = Some(fake_agent(work.path()));
            ws.new_session_from_home(harness, None, work.path().to_path_buf(), Some("fill context".into()), None, None, window, cx);
        })).unwrap();
        let id = ws.read_with(&cx, |ws, _| ws.active_id().unwrap().clone());
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let done = ws.read_with(&cx, |ws, app| {
                let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
                !view.is_loading() && !view.is_working() && view.thread.context.is_some()
            });
            if done { break; }
            if std::time::Instant::now() >= until {
                ws.read_with(&cx, |ws, app| {
                    let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
                    panic!("{} never settled after the context update: context={:?}, loading={}, working={}, error={:?}",
                        harness.name(), view.thread.context, view.is_loading(), view.is_working(), view.start_error());
                });
            }
            std::thread::yield_now();
        }
        ws.read_with(&cx, |ws, app| {
            let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
            let users: Vec<_> = view.thread.entries.iter().filter_map(|entry| match entry { Entry::User(text) => Some(text.as_str()), _ => None }).collect();
            assert_eq!(users, ["fill context"]);
            let turns: Vec<Value> = std::fs::read_to_string(work.path().join("fake_agent.py.turns")).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
            assert_eq!(turns.iter().map(|turn| turn["prompt"].as_str().unwrap()).collect::<Vec<_>>(), ["fill context", "/compact"]);
            assert_eq!(turns[0]["pid"], turns[1]["pid"], "compaction must keep the provider process");
            assert_eq!(view.thread.context, Some((200, 1000)));
        });
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.end_live(&id, window, cx))).unwrap();
    }
}

#[test]
fn restart_reuses_provider_session_respects_capabilities_and_records_cache_usage() {
    use crate::harness::thread::Entry;
    let _dirs = crate::datadir::use_throwaway_folders();
    let work = tempfile::tempdir().unwrap();
    let server = fake_agent(work.path());
    let script = PathBuf::from(&server.1[0]);
    let contents = std::fs::read_to_string(&script)
        .unwrap()
        .replace(
            r#"send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "end_turn"}})"#,
            r#"send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "end_turn", "usage": {"inputTokens": 10000, "cachedReadTokens": 8000, "cachedWriteTokens": 500, "outputTokens": 200}}})"#,
        )
        .replace(
            "    elif method == \"session/load\":\n        cwd = m[\"params\"][\"cwd\"]",
            "    elif method == \"session/load\":\n        cwd = m[\"params\"][\"cwd\"]\n        send({\"jsonrpc\": \"2.0\", \"method\": \"session/update\", \"params\": {\"sessionId\": m[\"params\"][\"sessionId\"], \"update\": {\"sessionUpdate\": \"user_message_chunk\", \"content\": {\"type\": \"text\", \"text\": \"first turn\"}}}})\n        send({\"jsonrpc\": \"2.0\", \"method\": \"session/update\", \"params\": {\"sessionId\": m[\"params\"][\"sessionId\"], \"update\": {\"sessionUpdate\": \"agent_message_chunk\", \"content\": {\"type\": \"text\", \"text\": \"ok\"}}}})",
        )
        .replace(
            "    else:\n        send({\"jsonrpc\": \"2.0\", \"id\": i, \"result\": {}})",
            "    elif method == \"session/resume\":\n        with open(__file__ + \".prompts\", \"a\", newline=\"\\n\") as log:\n            log.write(\"resumed \" + m[\"params\"][\"sessionId\"] + \"\\n---\\n\")\n        send({\"jsonrpc\": \"2.0\", \"id\": i, \"result\": {}})\n    else:\n        send({\"jsonrpc\": \"2.0\", \"id\": i, \"result\": {}})",
        );
    assert!(contents.contains("cachedReadTokens"), "usage patch did not match fake_agent.py");
    std::fs::write(&script, contents).unwrap();

    let resume_only_dir = tempfile::tempdir().unwrap();
    let resume_only_server = fake_agent(resume_only_dir.path());
    let resume_only_script = PathBuf::from(&resume_only_server.1[0]);
    let resume_contents = std::fs::read_to_string(&resume_only_script)
        .unwrap()
        .replace(
            r#""agentCapabilities": {"loadSession": True, "sessionCapabilities": {"resume": {}}}"#,
            r#""agentCapabilities": {"loadSession": False, "sessionCapabilities": {"resume": {}}}"#,
        )
        .replace(
            "    else:\n        send({\"jsonrpc\": \"2.0\", \"id\": i, \"result\": {}})",
            "    elif method == \"session/resume\":\n        with open(__file__ + \".prompts\", \"a\", newline=\"\\n\") as log:\n            log.write(\"resumed \" + m[\"params\"][\"sessionId\"] + \"\\n---\\n\")\n        send({\"jsonrpc\": \"2.0\", \"id\": i, \"result\": {}})\n    else:\n        send({\"jsonrpc\": \"2.0\", \"id\": i, \"result\": {}})",
        );
    std::fs::write(&resume_only_script, resume_contents).unwrap();

    let no_reopen_dir = tempfile::tempdir().unwrap();
    let no_reopen_server = fake_agent(no_reopen_dir.path());
    let no_reopen_script = PathBuf::from(&no_reopen_server.1[0]);
    let no_reopen_contents = std::fs::read_to_string(&no_reopen_script)
        .unwrap()
        .replace(
            r#""agentCapabilities": {"loadSession": True, "sessionCapabilities": {"resume": {}}}"#,
            r#""agentCapabilities": {}"#,
        );
    std::fs::write(&no_reopen_script, no_reopen_contents).unwrap();

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        cx.run_until_parked();
    };
    let state = tempfile::tempdir().unwrap();
    step(&mut cx, &|ws, window, cx| {
        ws.store = crate::store::Store::load(state.path().join("state.json"));
        ws.acp_server = Some(server.clone());
        ws.claude_exe = Some("unused-by-the-fake-agent".into());
        ws.new_session_from_home(Harness::Claude, None, work.path().to_path_buf(), Some("first turn".into()), None, None, window, cx);
    });
    let id = ws.read_with(&cx, |ws, _| ws.active_id().unwrap().clone());
    let wait_ready = |cx: &mut TestAppContext, expect_ok: bool| {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let done = ws.read_with(cx, |ws, app| {
                ws.pending_handoffs.is_empty()
                    && ws.live_view(&id).and_then(|v| v.thread()).is_some_and(|view| {
                        let view = view.read(app);
                        !view.is_loading() && !view.is_working() && (!expect_ok || view.thread.entries.iter().any(|e| matches!(e, Entry::Agent(t) if t == "ok")))
                    })
            });
            if done { break; }
            assert!(std::time::Instant::now() < until, "the session never settled");
            std::thread::yield_now();
        }
    };
    wait_ready(&mut cx, true);
    ws.read_with(&cx, |ws, app| {
        let meta = ws.store.session(&id).unwrap().agent.as_ref().unwrap();
        let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
        assert_eq!(meta.session_id, "fake-session");
        assert_eq!(view.thread.cache_usage.turns, 1);
        assert_eq!(meta.cache_usage.turns, 1);
        assert_eq!(meta.cache_usage.cached_read_tokens, 8_000);
        assert_eq!(meta.cache_usage.summary().as_deref(), Some("80% hit · 8k read"));
        assert_eq!(view.thread.cache_usage.cached_read_tokens, 8_000);
    });

    // Restarting reuses the saved provider session via session/load and preserves cache metrics and history.
    step(&mut cx, &|ws, window, cx| ws.restart_session(&id, window, cx));
    wait_ready(&mut cx, true);
    ws.read_with(&cx, |ws, app| {
        let meta = ws.store.session(&id).unwrap().agent.as_ref().unwrap();
        assert_eq!(meta.session_id, "fake-session");
        assert_eq!(meta.cache_usage.turns, 1, "restart preserves accumulated cache metrics");
        let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
        let users: Vec<_> = view.thread.entries.iter().filter_map(|e| match e { Entry::User(t) => Some(t.as_str()), _ => None }).collect();
        assert_eq!(users, ["first turn"], "session/load replay must not duplicate history");
        let prompts = std::fs::read_to_string(work.path().join("fake_agent.py.prompts")).unwrap();
        assert!(prompts.contains("loaded fake-session"));
    });

    // Reopening without a live view loads history asynchronously and must still suppress duplicate replay.
    step(&mut cx, &|ws, window, cx| {
        ws.end_live(&id, window, cx);
        ws.show_session(&id, window, cx);
    });
    wait_ready(&mut cx, true);
    ws.read_with(&cx, |ws, app| {
        let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
        let users: Vec<_> = view.thread.entries.iter().filter_map(|e| match e { Entry::User(t) => Some(t.as_str()), _ => None }).collect();
        assert_eq!(users, ["first turn"], "asynchronously restored history must not duplicate provider replay");
    });

    // After a handoff, restarting keeps m4code's canonical history instead of letting provider replay clobber it.
    step(&mut cx, &|ws, window, cx| ws.dynamic_handoff(&id, Harness::Codex, None, "second turn".into(), false, None, window, cx));
    wait_ready(&mut cx, true);
    step(&mut cx, &|ws, window, cx| ws.restart_session(&id, window, cx));
    wait_ready(&mut cx, true);
    ws.read_with(&cx, |ws, app| {
        let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
        let users: Vec<_> = view.thread.entries.iter().filter_map(|e| match e { Entry::User(t) => Some(t.as_str()), _ => None }).collect();
        assert_eq!(users, ["first turn", "second turn"], "canonical history survives restart after handoff");
    });

    // When the adapter advertises only session/resume (not loadSession), reopening uses session/resume.
    step(&mut cx, &|ws, window, cx| {
        ws.acp_server = Some(resume_only_server.clone());
        ws.restart_session(&id, window, cx);
    });
    wait_ready(&mut cx, true);
    ws.read_with(&cx, |ws, app| {
        let prompts = std::fs::read_to_string(resume_only_dir.path().join("fake_agent.py.prompts")).unwrap();
        assert!(prompts.contains("resumed fake-session"), "resume-only adapter must be reopened with session/resume");
        let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
        let users: Vec<_> = view.thread.entries.iter().filter_map(|e| match e { Entry::User(t) => Some(t.as_str()), _ => None }).collect();
        assert_eq!(users, ["first turn", "second turn"]);
    });

    // When the adapter advertises neither loadSession nor resume, reopening fails explicitly.
    step(&mut cx, &|ws, window, cx| {
        ws.acp_server = Some(no_reopen_server.clone());
        ws.restart_session(&id, window, cx);
    });
    let until = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if ws.read_with(&cx, |ws, app| ws.live_view(&id).is_some_and(|view| !view.is_running(app) && view.start_error(app).is_some())) {
            break;
        }
        assert!(std::time::Instant::now() < until, "unsupported reopen should retain an ended view with recovery controls");
        std::thread::yield_now();
    }
    ws.read_with(&cx, |ws, _| {
        assert!(ws.toast.as_ref().is_some_and(|t| t.text.contains("neither session/load nor session/resume")));
    });
}

#[test]
fn interrupted_handoff_preserves_source_session_and_history() {
    use crate::harness::thread::Entry;
    let _dirs = crate::datadir::use_throwaway_folders();
    let work = tempfile::tempdir().unwrap();
    let server = fake_agent(work.path());

    // A replacement server that blocks on session/new so the handoff stays pending until interrupted.
    let hanging_dir = tempfile::tempdir().unwrap();
    let hanging_server = fake_agent(hanging_dir.path());
    let hanging_script = PathBuf::from(&hanging_server.1[0]);
    let hanging_contents = std::fs::read_to_string(&hanging_script)
        .unwrap()
        .replace(
            r#"send({"jsonrpc": "2.0", "id": i, "result": {"sessionId": "fake-session"}})"#,
            "import time; time.sleep(30)",
        );
    std::fs::write(&hanging_script, hanging_contents).unwrap();

    // A replacement server that crashes (EOF) during session/new without sending a JSON-RPC error.
    let crashing_dir = tempfile::tempdir().unwrap();
    let crashing_server = fake_agent(crashing_dir.path());
    let crashing_script = PathBuf::from(&crashing_server.1[0]);
    let crashing_contents = std::fs::read_to_string(&crashing_script)
        .unwrap()
        .replace(
            r#"send({"jsonrpc": "2.0", "id": i, "result": {"sessionId": "fake-session"}})"#,
            "sys.stderr.write('replacement crashed\\n'); sys.stderr.flush(); sys.exit(1)",
        );
    std::fs::write(&crashing_script, crashing_contents).unwrap();

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        cx.run_until_parked();
    };
    let state = tempfile::tempdir().unwrap();
    step(&mut cx, &|ws, window, cx| {
        ws.store = crate::store::Store::load(state.path().join("state.json"));
        ws.acp_server = Some(server.clone());
        ws.claude_exe = Some("unused-by-the-fake-agent".into());
        ws.new_session_from_home(Harness::Claude, None, work.path().to_path_buf(), Some("before handoff".into()), None, None, window, cx);
    });
    let id = ws.read_with(&cx, |ws, _| ws.active_id().unwrap().clone());
    let wait_settled = |cx: &mut TestAppContext, sid: &str| {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            if ws.read_with(cx, |ws, app| {
                ws.pending_handoffs.is_empty()
                    && ws.live_view(sid).and_then(|v| v.thread()).is_some_and(|view| {
                        let view = view.read(app);
                        !view.is_loading() && !view.is_working() && view.thread.entries.iter().any(|e| matches!(e, Entry::Agent(t) if t == "ok"))
                    })
            }) {
                break;
            }
            assert!(std::time::Instant::now() < until, "session never settled");
            std::thread::yield_now();
        }
    };
    wait_settled(&mut cx, &id);

    // 1. Replacement crashes mid-startup: rollback restores source view, original harness, and draft.
    step(&mut cx, &|ws, window, cx| {
        ws.acp_server = Some(crashing_server.clone());
        ws.dynamic_handoff(&id, Harness::Codex, None, "draft after crash".into(), false, None, window, cx);
    });
    wait_settled(&mut cx, &id);
    ws.read_with(&cx, |ws, app| {
        let meta = ws.store.session(&id).unwrap().agent.as_ref().unwrap();
        assert_eq!(meta.harness, Harness::Claude);
        assert_eq!(meta.session_id, "fake-session");
        let view = ws.live_view(&id).unwrap().thread().unwrap().read(app);
        assert_eq!(view.draft(app), "draft after crash");
        assert!(view.thread.entries.iter().any(|entry| matches!(entry,
            Entry::Notice { text, error: true, .. } if text.contains("Handoff failed"))));
        assert!(view.thread.active_notice(crate::harness::thread::NoticeKind::Handoff).is_none());
    });

    // 2. Replacement hangs: a concurrent handoff is refused, and ending the session cancels the pending handoff cleanly.
    step(&mut cx, &|ws, window, cx| {
        ws.acp_server = Some(hanging_server.clone());
        ws.dynamic_handoff(&id, Harness::Codex, None, "pending msg".into(), false, None, window, cx);
    });
    ws.read_with(&cx, |ws, _| assert!(ws.pending_handoffs.contains_key(&id)));
    step(&mut cx, &|ws, window, cx| {
        ws.dynamic_handoff(&id, Harness::OpenCode, None, "second attempt".into(), false, None, window, cx);
    });
    ws.read_with(&cx, |ws, _| {
        assert!(ws.toast.as_ref().is_some_and(|t| t.text.contains("already starting")));
    });
    step(&mut cx, &|ws, window, cx| ws.end_live(&id, window, cx));
    ws.read_with(&cx, |ws, _| {
        assert!(ws.pending_handoffs.is_empty(), "closing during handoff must clean up pending state");
        let meta = ws.store.session(&id).unwrap().agent.as_ref().unwrap();
        assert_eq!(meta.harness, Harness::Claude, "aborted handoff must not overwrite the stored harness");
        assert_eq!(meta.session_id, "fake-session", "aborted handoff must preserve the original provider session");
    });
    let persisted = crate::harness::thread::load_entries(&id);
    assert!(persisted.iter().any(|e| matches!(e, Entry::User(t) if t == "before handoff")));
    assert!(!persisted.iter().any(|e| matches!(e, Entry::Notice { text, .. } if text.contains("Switched to"))), "uncommitted handoff notice must not overwrite persisted history");
}

#[test]
fn compaction_respects_toggle_does_not_loop_on_failure_and_rearms() {
    use serde_json::Value;
    let _dirs = crate::datadir::use_throwaway_folders();
    let work = tempfile::tempdir().unwrap();
    let server = fake_agent(work.path());
    let script = PathBuf::from(&server.1[0]);
    let contents = std::fs::read_to_string(&script).unwrap().replace(
        &r#"        if text in ["fill context", "/compact"]:
            send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": s, "update": {"sessionUpdate": "usage_update", "used": 950 if text == "fill context" else 200, "size": 1000}}})
        send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "end_turn"}})"#.replace("\r\n", "\n"),
        r#"        compact_count = 0
        if os.path.exists(__file__ + ".compacts"):
            compact_count = int(open(__file__ + ".compacts").read().strip() or "0")
        if text == "fill context":
            send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": s, "update": {"sessionUpdate": "usage_update", "used": 950, "size": 1000}}})
            send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "end_turn"}})
        elif text == "drop context":
            send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": s, "update": {"sessionUpdate": "usage_update", "used": 400, "size": 1000}}})
            send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "end_turn"}})
        elif text == "/compact":
            compact_count += 1
            open(__file__ + ".compacts", "w").write(str(compact_count))
            if compact_count == 1:
                send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": s, "update": {"sessionUpdate": "usage_update", "used": 950, "size": 1000}}})
                send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "max_tokens"}})
            else:
                send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": s, "update": {"sessionUpdate": "usage_update", "used": 200, "size": 1000}}})
                send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "end_turn"}})
        else:
            send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "end_turn"}})"#,
    );
    std::fs::write(&script, contents).unwrap();

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        cx.run_until_parked();
    };
    let state = tempfile::tempdir().unwrap();
    step(&mut cx, &|ws, window, cx| {
        ws.store = crate::store::Store::load(state.path().join("state.json"));
        ws.acp_server = Some(server.clone());
        ws.new_session_from_home(Harness::Codex, None, work.path().to_path_buf(), None, None, None, window, cx);
    });
    let id = ws.read_with(&cx, |ws, _| ws.active_id().unwrap().clone());
    let wait_idle = |cx: &mut TestAppContext, check: &dyn Fn(&crate::thread::ThreadView) -> bool| {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            if ws.read_with(cx, |ws, app| {
                ws.live_view(&id).and_then(|v| v.thread()).is_some_and(|view| {
                    let view = view.read(app);
                    !view.is_loading() && !view.is_working() && check(view)
                })
            }) {
                break;
            }
            assert!(std::time::Instant::now() < until, "compaction scenario timed out");
            std::thread::yield_now();
        }
    };
    wait_idle(&mut cx, &|_| true);

    // Turning auto-compact off stops /compact even when context reaches 85%.
    step(&mut cx, &|ws, window, cx| {
        ws.act(Act::SetAutoCompact(id.clone(), Some(false)), window, cx);
        let view = ws.live_view(&id).unwrap().thread().unwrap().clone();
        view.update(cx, |v, cx| v.send("fill context".into(), window, cx));
    });
    wait_idle(&mut cx, &|v| v.thread.context == Some((950, 1000)));
    let turns = || -> Vec<String> {
        std::fs::read_to_string(work.path().join("fake_agent.py.turns"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["prompt"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(turns(), ["fill context"], "disabled auto-compact must not trigger /compact");

    // Turning auto-compact back on triggers /compact; the first attempt fails and must not loop.
    step(&mut cx, &|ws, window, cx| ws.act(Act::SetAutoCompact(id.clone(), Some(true)), window, cx));
    wait_idle(&mut cx, &|v| v.thread.entries.iter().any(|e| matches!(e, crate::harness::thread::Entry::Notice { text, error: true, .. } if text == "Context compaction did not finish.")));
    assert_eq!(turns(), ["fill context", "/compact"], "failed compaction must not loop");

    // Dropping below 90% re-arms compaction; hitting 95% again runs /compact and brings usage down to 20%.
    step(&mut cx, &|ws, window, cx| {
        let view = ws.live_view(&id).unwrap().thread().unwrap().clone();
        view.update(cx, |v, cx| v.send("drop context".into(), window, cx));
    });
    wait_idle(&mut cx, &|v| v.thread.context == Some((400, 1000)));
    step(&mut cx, &|ws, window, cx| {
        let view = ws.live_view(&id).unwrap().thread().unwrap().clone();
        view.update(cx, |v, cx| v.send("fill context".into(), window, cx));
    });
    wait_idle(&mut cx, &|v| v.thread.context == Some((200, 1000)));
    assert_eq!(turns(), ["fill context", "/compact", "drop context", "fill context", "/compact"]);
    step(&mut cx, &|ws, window, cx| ws.end_live(&id, window, cx));
}

#[test]
fn shared_subagents_keep_their_parent_focus_permissions_and_conversation() {
    use crate::mcp::bridge::Request;
    use serde_json::{Value, json};
    let _dirs = crate::datadir::use_throwaway_folders();
    let work = tempfile::tempdir().unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        cx.run_until_parked();
    };
    step(&mut cx, &|ws, _, _| {
        ws.acp_server = Some(fake_agent(work.path()));
        ws.claude_exe = Some(PathBuf::from("unused-by-the-fake-agent"));
        ws.store.data.settings.enabled_harnesses = None;
        ws.store.data.settings.custom_subagents = Default::default();
        ws.store.data.settings.claude_picked = vec!["haiku".into()];
        ws.store.data.settings.new_session.model = Some("opus".into());
    });
    for harness in [Harness::Claude, Harness::Codex, Harness::OpenCode] {
        step(&mut cx, &|ws, window, cx| {
            ws.new_session_from_home(harness, None, work.path().to_path_buf(), Some("parent".into()), None, None, window, cx);
        });
        let parent = ws.read_with(&cx, |ws, _| ws.active_id().unwrap().clone());
        let request = |tool: &str, args| Request { id: new_id(), from: format!("code:{parent}"), tool: tool.into(), args, deadline: now() + 60 };
        let spawn = request("spawn_subagent", json!({"harness": harness, "prompt": "child task"}));
        if harness == Harness::Claude {
            step(&mut cx, &|ws, window, cx| {
                ws.store.data.settings.claude_subagents.on = false;
                assert!(ws.run_request(&spawn, window, cx).is_err(), "Claude must respect its saved subagent switch");
                ws.store.data.settings.claude_subagents.on = true;
            });
        }
        let child = std::cell::RefCell::new(String::new());
        step(&mut cx, &|ws, window, cx| {
            let reply: Value = serde_json::from_str(&ws.run_request(&spawn, window, cx).unwrap()).unwrap();
            *child.borrow_mut() = reply["id"].as_str().unwrap().to_string();
            assert_eq!(ws.active_id(), Some(&parent), "spawning must not take focus from the parent");
            let meta = ws.store.session(&child.borrow()).unwrap().agent.as_ref().unwrap();
            assert_eq!(meta.parent_session.as_ref(), Some(&parent));
            assert!(meta.subagent_edits);
            assert_eq!(ws.subagents_of(&parent).iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(), vec![child.borrow().clone()], "the parent's right sidebar lists the subagent");
            assert!(!ws.running_ids(cx).contains(&child.borrow()), "a subagent is not a numbered, cycled session in the left list");
            if harness == Harness::Claude {
                assert_eq!(meta.chosen_model.as_deref(), Some("haiku"), "a disabled default model must not override the selected child models");
            }
        });
        let child = child.into_inner();
        let wait = request("wait_subagent", json!({"id": child}));
        // Generous, since a slow CI machine runs the stand-in agents' processes alongside the whole suite.
        let until = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let done = std::cell::Cell::new(false);
            step(&mut cx, &|ws, window, cx| {
                let status: Value = serde_json::from_str(&ws.run_request(&wait, window, cx).unwrap()).unwrap();
                done.set(status["state"] == "completed" && status["report"] == "ok");
            });
            if done.get() { break; }
            assert!(std::time::Instant::now() < until, "the child never finished");
            std::thread::yield_now();
        }
        let until = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            cx.run_until_parked();
            let reported = ws.read_with(&cx, |ws, app| {
                ws.live_view(&parent).unwrap().thread().unwrap().read(app).thread.entries.iter().any(|entry| matches!(entry,
                    crate::harness::thread::Entry::User(text) if crate::harness::thread::parse_subagent_report(text)
                        .is_some_and(|report| report.id == child && report.state == "completed" && report.body == "ok")))
            });
            if reported { break; }
            if std::time::Instant::now() >= until {
                let seen = ws.read_with(&cx, |ws, app| {
                    let view = |id: &str| ws.live_view(id).and_then(|v| v.thread()).map(|v| {
                        let v = v.read(app);
                        let entries: Vec<String> = v.thread.entries.iter().map(|e| format!("{e:?}").chars().take(90).collect()).collect();
                        format!("loading {} working {} entries {entries:?}", v.is_loading(), v.is_working())
                    });
                    format!("{harness:?}\nparent: {:?}\nchild: {:?}", view(&parent), view(&child))
                });
                panic!("the parent's chat never got the child's report\n{seen}");
            }
            std::thread::yield_now();
        }
        if harness == Harness::Claude {
            // A turn that fills the context starts an automatic compaction; its report still reaches the parent.
            let full = std::cell::RefCell::new(String::new());
            step(&mut cx, &|ws, window, cx| {
                let reply: Value = serde_json::from_str(&ws.run_request(&request("spawn_subagent", json!({"harness": harness, "prompt": "fill context"})), window, cx).unwrap()).unwrap();
                *full.borrow_mut() = reply["id"].as_str().unwrap().to_string();
            });
            let full = full.into_inner();
            let until = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                cx.run_until_parked();
                let reported = ws.read_with(&cx, |ws, app| {
                    ws.live_view(&parent).unwrap().thread().unwrap().read(app).thread.entries.iter().any(|entry| matches!(entry,
                        crate::harness::thread::Entry::User(text) if crate::harness::thread::parse_subagent_report(text)
                            .is_some_and(|report| report.id == full && report.body == "ok")))
                });
                if reported { break; }
                assert!(std::time::Instant::now() < until, "a compaction after the child's turn swallowed its report");
                std::thread::yield_now();
            }
            let turns = std::fs::read_to_string(work.path().join("fake_agent.py.turns")).unwrap();
            assert!(turns.contains(r#""prompt": "/compact""#), "the child's full context started a compaction: {turns}");
            step(&mut cx, &|ws, window, cx| ws.end_live(&full, window, cx));
        }
        #[cfg(target_os = "linux")]
        let child_pid = std::fs::read_to_string(work.path().join("fake_agent.py.turns")).unwrap().lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .rfind(|turn| turn["prompt"] == "child task").unwrap()["pid"].as_i64().unwrap() as i32;
        step(&mut cx, &|ws, window, cx| {
            let forged = Request { from: format!("code:{child}"), ..wait.clone() };
            assert!(ws.run_request(&forged, window, cx).is_err(), "a child cannot control its parent or a sibling");
            ws.run_request(&request("stop_subagent", json!({"id": child})), window, cx).unwrap();
            assert!(!ws.is_running(&child, cx));
            assert!(ws.store.session(&child).is_some());
            ws.run_request(&request("send_subagent_message", json!({"id": child, "prompt": "follow up"})), window, cx).unwrap();
            assert!(ws.is_running(&child, cx), "follow-ups resume the same child record");
            assert_eq!(ws.active_id(), Some(&parent));
            match harness.agent() {
                None => ws.store.data.settings.claude_subagents.edits = false,
                Some(agent) => ws.store.data.settings.custom_subagents.get_mut(agent).edits = false,
            }
            let limited: Value = serde_json::from_str(&ws.run_request(&spawn, window, cx).unwrap()).unwrap();
            let limited = ws.store.session(limited["id"].as_str().unwrap()).unwrap();
            assert!(!limited.agent.as_ref().unwrap().subagent_edits, "the target's Allow edits setting limits a writable parent");
            match harness.agent() {
                None => ws.store.data.settings.claude_subagents.edits = true,
                Some(agent) => ws.store.data.settings.custom_subagents.get_mut(agent).edits = true,
            }
            ws.live_view(&parent).unwrap().thread().unwrap().update(cx, |view, _| view.thread.mode = Permission::ReadOnly.native(harness).map(String::from));
            let restricted: Value = serde_json::from_str(&ws.run_request(&request("spawn_subagent", json!({"harness":harness,"prompt":"read only"})), window, cx).unwrap()).unwrap();
            let restricted = ws.store.session(restricted["id"].as_str().unwrap()).unwrap().clone();
            assert!(!restricted.agent.as_ref().unwrap().subagent_edits, "a read-only parent cannot grant edits");
            if harness == Harness::Claude {
                let start = ws.thread_start(&restricted.id, &restricted.cwd, restricted.agent.as_ref().unwrap(), None, None, Starter::User, cx).unwrap();
                assert_eq!(start.meta["claudeCode"]["options"]["permissionMode"], "plan", "read-only must apply before the first prompt");
            }
            ws.end_live(&parent, window, cx);
            assert!(!ws.is_running(&child, cx), "ending a parent stops its children");
        });
        #[cfg(target_os = "linux")]
        {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            // Signal zero only checks whether the process is still alive.
            while unsafe { libc::kill(child_pid, 0) } == 0 {
                cx.run_until_parked();
                assert!(std::time::Instant::now() < until, "the stopped child's ACP process is still alive");
                std::thread::yield_now();
            }
        }
    }
}

#[test]
fn home_does_not_launch_without_a_message() {
    let _dirs = crate::datadir::use_throwaway_folders();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    for creating in [false, true] {
        for text in ["", " \n\t "] {
            cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| {
                ws.claude_exe = None;
                ws.home.creating = creating.then(|| PathBuf::from("/pending-worktree"));
                ws.home.prompt.update(cx, |p, cx| p.set_value(text, window, cx));
                ws.act(Act::HomeLaunch, window, cx);
                assert!(ws.toast.is_none(), "an empty message must not attempt to launch a harness");
                assert!(!ws.home.launch_when_created, "an empty message must not queue a launch");
                assert!(ws.live.is_empty());
            })).unwrap();
        }
    }
}

/// With no `claude` found, starting a session says so and offers the setup window; asking for it twice shows one.
#[test]
#[ignore]
fn a_missing_claude_offers_the_setup_window() {
    let _data = crate::datadir::own_folder();
    let work = tempfile::tempdir().unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();

    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            ws.claude_exe = None;
            ws.new_session(work.path().to_path_buf(), window, cx);
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
    // Not `#[ignore]`d, so it runs beside the unit tests: it takes the run's one data folder, as `datadir::own_folder`
    // would move theirs while they read it (#149). Its repository is new, so
    // its worktree folder and project are its own there.
    crate::datadir::use_throwaway_folders();
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
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![repo_dir.clone()], cx))))
        .unwrap();
    let project = ws.read_with(&cx, |ws, _| ws.store.data.projects.iter().find(|p| p.path == repo_dir).expect("the project opened").id.clone());
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
            kind: SessionKind::Agent,
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
            agent: Some(AgentMeta { session_id: id.clone(), external: ExternalState::Present, ..Default::default() }),
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

    // The GitHub shortcut works on Home and uses the selected project.
    step(&mut cx, &|ws, _, cx| {
        ws.main = Main::Home;
        ws.active = None;
        ws.home.project = Some(project.clone());
        while ws.toast.is_some() { ws.dismiss_toast(cx); }
    });
    cx.dispatch_action(window, OpenGithub);
    cx.run_until_parked();
    ws.read_with(&cx, |ws, _| {
        let pane = ws.right_gh.as_deref().and_then(|id| ws.gh_pane(id)).expect("GitHub opens on Home");
        assert_eq!(pane.cwd, repo_dir);
        assert!(ws.active_id().is_none());
    });
    step(&mut cx, &|ws, _, _| ws.right_gh = None);

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

    // Each exit keeps its feedback until it is dismissed or expires; consume this one before checking the next.
    step(&mut cx, &|ws, _, cx| ws.dismiss_toast(cx));

    let missing = new_id();
    let record = SessionRecord {
        id: missing.clone(),
        kind: SessionKind::Agent,
        title: "Missing".into(),
        custom_title: None,
        auto_title: None,
        project_id: Some(project),
        cwd: folder.clone(),
        created_at: now(),
        last_active: now(),
        exit_code: None,
        hidden: false,
        agent: Some(AgentMeta { session_id: missing.clone(), external: ExternalState::Present, ..Default::default() }),
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
    let _data = crate::datadir::own_folder();
    let work = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(work.path()).unwrap().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let git = |args: &[&str]| assert!(std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).current_dir(&cwd).status().unwrap().success());
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "core.autocrlf", "false"]);
    std::fs::write(cwd.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "one"]);
    let agent = fake_agent(work.path());
    let prompts = PathBuf::from(format!("{}.prompts", agent.1[0]));

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
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
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.new_session(cwd.clone(), window, cx))).unwrap();
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
    let turn_id = ws.read_with(&cx, |ws, app| {
        let ids = thread(ws).read(app).turn_review_ids();
        assert_eq!(ids.len(), 1, "only the file-changing turn has a captured review");
        ids[0]
    });
    on_thread(&mut cx, &|v, _, cx| v.open_turn_review(turn_id, cx));
    wait(&mut cx, "the historical review", &|ws, app| thread(ws).read(app).review_files().is_some());
    assert_eq!(ws.read_with(&cx, |ws, app| thread(ws).read(app).review_files().unwrap().to_vec()), files);
    on_thread(&mut cx, &|v, _, cx| v.open_review(cx));
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
    let _data = crate::datadir::own_folder();
    let work = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(work.path()).unwrap();
    let agent = fake_agent(work.path());
    let prompts = PathBuf::from(format!("{}.prompts", agent.1[0]));
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
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
        assert_eq!((rec.provider_session_id(), rec.title.as_str()), (Some("from-terminal"), "Started in a terminal"));
    });
    act(&mut cx, pick);
    assert_eq!(ws.read_with(&cx, |ws, _| ws.store.data.sessions.iter().filter(|s| s.provider_session_id() == Some("from-terminal")).count()), 1, "no second record");
}

/// Handing a session to another agent starts a session of that agent in the same folder, whose first message carries
/// the conversation; the first session keeps running.
#[test]
#[ignore = "drives the workspace with a stand-in agent"]
fn handing_a_session_to_another_agent() {
    let _data = crate::datadir::own_folder();
    let work = tempfile::tempdir().unwrap();
    let cwd = dunce::canonicalize(work.path()).unwrap();
    let agent = fake_agent(work.path());
    let prompts = PathBuf::from(format!("{}.prompts", agent.1[0]));
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
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
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.new_session(cwd.clone(), window, cx))).unwrap();
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
        assert_eq!((rec.agent.as_ref().unwrap().harness, &rec.cwd), (crate::harness::Harness::Claude, &cwd));
    });
    wait(&mut cx, "the hand-off message", &|_, _| {
        std::fs::read_to_string(&prompts).is_ok_and(|p| {
            p.contains("taking over work from a Claude Code session")
                && p.contains("User: plan the change\n\nAgent: ok")
                && p.contains("User's next message:\nimplement step 2")
        })
    });
    ws.read_with(&cx, |ws, _| {
        let rec = ws.store.session(&first).unwrap();
        assert_eq!((rec.agent.as_ref().unwrap().harness, &rec.cwd), (crate::harness::Harness::Codex, &cwd));
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
    if crate::platform::find_executable("opencode").is_none() {
        eprintln!("SKIP: OpenCode integration requires the opencode CLI on PATH");
        return;
    }
    a_session_answers_in_its_thread(crate::harness::Harness::OpenCode);
}

fn a_session_answers_in_its_thread(harness: crate::harness::Harness) {
    use crate::harness::thread::Entry;
    let _data = crate::datadir::own_folder();
    let work = tempfile::tempdir().unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
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
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.new_session_with(harness, cwd.clone(), None, vec![], Starter::User, None, window, cx))).unwrap();
    draw(&mut cx);
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().expect("a session is shown"));
    let thread = |ws: &Workspace| ws.live_view(&id).and_then(|v| v.thread()).cloned().expect("a thread view");
    assert_eq!(ws.read_with(&cx, |ws, app| thread(ws).read(app).harness), harness, "the integration must launch the requested adapter");
    wait(&mut cx, "the session to open", 120, &|ws, app| !thread(ws).read(app).is_loading() || !ws.is_running(&id, app));
    assert!(ws.read_with(&cx, |ws, app| ws.is_running(&id, app)), "the harness stopped: {:?}", ws.read_with(&cx, |ws, app| thread(ws).read(app).start_error()));
    assert!(ws.read_with(&cx, |ws, _| ws.store.session(&id).and_then(|r| r.provider_session_id()).is_some()), "the session's id was kept");
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


#[test]
fn a_session_opens_another_with_its_model_and_effort_without_asking() {
    use crate::mcp::bridge::Request;
    use serde_json::json;
    let _dirs = crate::datadir::use_throwaway_folders();
    let work = tempfile::tempdir().unwrap();
    let server = fake_agent(work.path());
    // The stand-in offers a model and an effort to choose, and notes each choice it is sent.
    let script = PathBuf::from(&server.1[0]);
    let options = r#"{"sessionId": "fake-session", "configOptions": [{"id": "model", "category": "model", "type": "select", "currentValue": "default", "options": [{"value": "default", "name": "Default"}, {"value": "gpt-6", "name": "GPT-6"}]}, {"id": "effort", "category": "thought_level", "type": "select", "currentValue": "medium", "options": [{"value": "medium", "name": "medium"}, {"value": "high", "name": "high"}]}]}"#;
    let contents = std::fs::read_to_string(&script).unwrap()
        .replace(r#""result": {"sessionId": "fake-session"}"#, &format!(r#""result": {options}"#))
        .replace("    if \"id\" not in m or \"method\" not in m:\n", "    if \"id\" not in m or \"method\" not in m:\n        open(__file__ + \".answers\", \"a\").write(line)\n")
        .replace("        if \"edit\" in text:", "        if text == \"child task\":\n            send({\"jsonrpc\": \"2.0\", \"id\": \"perm-1\", \"method\": \"session/request_permission\", \"params\": {\"sessionId\": s, \"toolCall\": {\"toolCallId\": \"c1\", \"title\": \"m4code_list_models\"}, \"options\": [{\"optionId\": \"yes\", \"kind\": \"allow_once\"}, {\"optionId\": \"no\", \"kind\": \"reject_once\"}]}})\n        if \"edit\" in text:")
        .replace("    elif method == \"session/list\":", "    elif method == \"session/set_config_option\":\n        with open(__file__ + \".options\", \"a\") as log:\n            log.write(m[\"params\"][\"configId\"] + \"=\" + m[\"params\"][\"value\"] + \"\\n\")\n        send({\"jsonrpc\": \"2.0\", \"id\": i, \"result\": {}})\n    elif method == \"session/list\":");
    std::fs::write(&script, contents).unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    let step = |cx: &mut TestAppContext, f: &dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| f(ws, window, cx))).unwrap();
        cx.run_until_parked();
    };
    let state = tempfile::tempdir().unwrap();
    step(&mut cx, &|ws, window, cx| {
        // Its stand-in offers a model list of its own. Kept in this test's own state file, it never reaches the other
        // tests that share the throwaway data folder as Claude Code's catalog.
        ws.store = crate::store::Store::load(state.path().join("state.json"));
        ws.acp_server = Some(server.clone());
        ws.claude_exe = Some(PathBuf::from("unused-by-the-fake-agent"));
        ws.store.data.settings.enabled_harnesses = None;
        ws.store.data.settings.set_harness_models(Harness::Codex, vec![crate::harness::subagents::AgentModel { id: "gpt-6".into(), name: "GPT-6".into() }]);
        ws.new_session_from_home(Harness::Claude, None, work.path().to_path_buf(), Some("parent".into()), None, None, window, cx);
    });
    let parent = ws.read_with(&cx, |ws, _| ws.active_id().unwrap().clone());
    let request = |args| Request { id: new_id(), from: format!("code:{parent}"), tool: "start_code_session".into(), args, deadline: now() + 60 };
    let folder = work.path().to_string_lossy().into_owned();
    step(&mut cx, &|ws, window, cx| {
        let unknown = request(json!({"folder": folder, "model": "codex:gpt-0"}));
        ws.serve_requests(vec![unknown], window, cx);
        assert_eq!(ws.live.len(), 1, "an unknown model opens nothing");
        let effort = request(json!({"folder": folder, "effort": "extreme"}));
        ws.serve_requests(vec![effort], window, cx);
        assert_eq!(ws.live.len(), 1, "an unknown effort opens nothing");
        let wanted = request(json!({"folder": folder, "model": "codex:gpt-6", "effort": "high", "prompt": "child task", "permission": "bypassPermissions"}));
        ws.serve_requests(vec![wanted], window, cx);
        assert!(ws.modal.is_none(), "the user is not asked to confirm what they asked for");
        assert_eq!(ws.live.len(), 2, "the session opened at once");
    });
    let child = ws.read_with(&cx, |ws, _| ws.live.iter().map(|l| l.id.clone()).find(|id| *id != parent).unwrap());
    let until = std::time::Instant::now() + Duration::from_secs(10);
    let options = work.path().join("fake_agent.py.options");
    while !std::fs::read_to_string(&options).is_ok_and(|o| o.contains("model=gpt-6") && o.contains("effort=high")) {
        assert!(std::time::Instant::now() < until, "the model and effort were never chosen: {:?}", std::fs::read_to_string(&options));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(50));
    }
    let answers = work.path().join("fake_agent.py.answers");
    while !std::fs::read_to_string(&answers).is_ok_and(|a| a.contains(r#""optionId": "yes""#) || a.contains(r#""optionId":"yes""#)) {
        assert!(std::time::Instant::now() < until, "the prompt for m4code's own tool was not answered yes: {:?}", std::fs::read_to_string(&answers));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(50));
    }
    ws.read_with(&cx, |ws, app| {
        assert!(!ws.live_view(&child).unwrap().thread().unwrap().read(app).is_waiting(), "nobody was asked");
        let record = ws.store.session(&child).unwrap();
        assert_eq!(record.agent.as_ref().unwrap().harness, Harness::Codex, "the model's harness runs it");
        assert!(ws.links_of(&parent).iter().any(|(id, _)| *id == child), "it is linked with the session that opened it");
        let mode = ws.live_view(&child).unwrap().thread().unwrap().read(app).thread.mode.clone();
        assert_ne!(mode.as_deref(), Some("bypassPermissions"), "the model cannot choose the permission mode");
    });
    for id in [child, parent] {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.end_live(&id, window, cx))).unwrap();
    }
}

#[test]
fn foreground_adapters_apply_startup_options_and_isolate_session_updates() {
    use crate::harness::defaults::Starter;
    use crate::harness::thread::Entry;

    for harness in Harness::ALL {
        for mode_reply in ["omitted", "rejected", "applied"] {
            let work = tempfile::tempdir().unwrap();
            let mut server = fake_agent(work.path());
            server.1.push(Permission::ReadOnly.native(harness).unwrap().into());
            server.1.push(mode_reply.into());
            let script = PathBuf::from(&server.1[0]);
            let contents = std::fs::read_to_string(&script).unwrap()
                .replace("for line in sys.stdin:", r#"
read_only, mode_reply = sys.argv[1:3]
model, effort = "small", "low"
current_mode = "default"
def modes():
    return {"currentModeId": current_mode, "availableModes": [{"id": read_only, "name": "Read-only"}]}
def config():
    available_effort = ["low"] if model == "small" else ["medium", "high"]
    return [
        {"category": "model", "id": "model", "currentValue": model, "options": [{"value": v, "name": v} for v in ["small", "large"]]},
        {"category": "thought_level", "id": model + "-effort", "currentValue": effort, "options": [{"value": v, "name": v} for v in available_effort]},
    ]
for line in sys.stdin:"#)
                .replace(r#"        send({"jsonrpc": "2.0", "id": i, "result": {"sessionId": "fake-session"}})"#, r#"
        result = {"sessionId": "fake-session", "configOptions": config()}
        if mode_reply != "omitted":
            result["modes"] = modes()
        send({"jsonrpc": "2.0", "id": i, "result": result})
    elif method == "session/set_config_option":
        key, value = m["params"]["configId"], m["params"]["value"]
        if key == "model":
            model, effort = value, "medium"
        else:
            assert key == "large-effort" and value == "high"
            effort = value
        send({"jsonrpc": "2.0", "id": i, "result": {"configOptions": config()}})
    elif method == "session/set_mode":
        assert m["params"]["modeId"] == read_only
        if mode_reply == "applied":
            current_mode = read_only
        send({"jsonrpc": "2.0", "id": i, "result": {"modes": modes()}})"#)
                .replace(r#"        text = m["params"]["prompt"][0]["text"]"#, r#"        text = m["params"]["prompt"][0]["text"]
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "child-session", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "child reply"}}}})
        send({"jsonrpc": "2.0", "method": "session/request_permission", "id": "child-ask", "params": {"sessionId": "child-session", "toolCall": {"title": "Child tool"}, "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}]}})
        response = json.loads(next(sys.stdin))
        assert response["id"] == "child-ask" and response["result"]["outcome"]["outcome"] == "cancelled"
        assert model == "large" and effort == "high" and current_mode == read_only"#);
            std::fs::write(&script, contents).unwrap();
            let mut app = TestAppContext::single();
            app.update(gpui_kit::init);
            let (view, cx) = app.add_window_view(|window, cx| {
                crate::thread::ThreadView::new(crate::thread::Start {
                    record_id: String::new(),
                    harness,
                    all_models: Vec::new(),
                    picked: Vec::new(),
                    server: Some(server),
                    cwd: work.path().into(),
                    env: Vec::new(),
                    session_id: None,
                    meta: serde_json::Value::Null,
                    mcp_servers: Vec::new(), mcp_tools: None, launch_args: vec![],
                    permission: Some(Permission::ReadOnly),
                    starter: Starter::User,
                    options: vec![("thought_level", "high".into()), ("model", "large".into())],
                    prompt: Some("Review the project".into()),
                    prior_entries: Vec::new(),
                    inline_visuals: false,
                }, window, cx)
            });
            let until = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                cx.run_until_parked();
                let settled = cx.update(|_, app| {
                    let view = view.read(app);
                    !view.is_loading() && !view.is_working()
                });
                if settled { break; }
                assert!(std::time::Instant::now() < until, "{} did not settle with mode reply {mode_reply}", harness.name());
                std::thread::sleep(Duration::from_millis(5));
            }
            cx.update(|_, app| {
                let view = view.read(app);
                if mode_reply == "applied" {
                    assert_eq!(view.start_error(), None);
                    assert_eq!(view.permission(), Some(Permission::ReadOnly));
                    assert_eq!(view.thread.option("thought_level").unwrap().current, "high");
                    assert!(!view.is_waiting());
                    assert_eq!(view.thread.entries, [Entry::User("Review the project".into()), Entry::Agent("ok".into())]);
                    assert!(work.path().join("fake_agent.py.prompts").exists());
                } else {
                    assert!(view.start_error().is_some_and(|error| error.contains("Read-only")));
                    assert!(!work.path().join("fake_agent.py.prompts").exists());
                }
            });
        }
    }
}

#[test]
fn expired_login_retains_the_session_and_retries_without_repeating_the_prompt() {
    let _dirs = crate::datadir::use_throwaway_folders();
    let work = tempfile::tempdir().unwrap();
    let server = fake_agent(work.path());
    let script = PathBuf::from(&server.1[0]);
    let healthy = std::fs::read_to_string(&script).unwrap();
    let expired = healthy.replace(r#""result": {"sessionId": "fake-session"}"#, r#""error": {"code": -32000, "message": "Credentials unavailable", "data": {"provider": "codex"}}"#);
    assert_ne!(expired, healthy);
    std::fs::write(&script, expired).unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| { gpui_kit::init(app); crate::panels::theme::install(app); });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| {
        ws.acp_server = Some(server.clone());
        ws.new_session_from_home(Harness::Codex, None, work.path().into(), Some("Keep this task".into()), None, None, window, cx);
    })).unwrap();
    let id = ws.read_with(&cx, |ws, _| ws.active_id().unwrap().clone());
    let until = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if ws.read_with(&cx, |ws, app| ws.live_view(&id).is_some_and(|view| !view.is_running(app) && view.start_error(app).is_some_and(|error| error.contains("codex login")))) { break; }
        assert!(std::time::Instant::now() < until, "login failure never surfaced");
        std::thread::yield_now();
    }
    assert!(crate::thread::has_saved_queue(&id), "the initial task must remain queued");
    std::fs::write(&script, healthy).unwrap();
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.resume(&id, window, cx))).unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if ws.read_with(&cx, |ws, app| ws.live_view(&id).is_some_and(|view| view.is_running(app) && !view.thread().unwrap().read(app).is_loading())) { break; }
        assert!(std::time::Instant::now() < until, "retry never connected");
        std::thread::yield_now();
    }
    let prompts = work.path().join("fake_agent.py.prompts");
    assert!(!prompts.exists(), "reconnecting must leave the queue held instead of repeating work");
    assert!(crate::thread::has_saved_queue(&id));
}

#[test]
fn option_changes_coalesce_and_rejections_restore_the_confirmed_model() {
    let _dirs = crate::datadir::use_throwaway_folders();
    let work = tempfile::tempdir().unwrap();
    let server = fake_agent(work.path());
    let script = PathBuf::from(&server.1[0]);
    let source = std::fs::read_to_string(&script).unwrap()
        .replace("for line in sys.stdin:", r#"import time
def options(model):
    return [{"id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": model, "options": [{"value": v, "name": v} for v in ["a", "b", "c", "bad"]]}]
for line in sys.stdin:"#)
        .replace(r#""result": {"sessionId": "fake-session"}"#, r#""result": {"sessionId": "fake-session", "configOptions": options("a")}"#)
        .replace(r#"    elif method == "session/list":"#, r#"    elif method == "session/set_config_option":
        value = m["params"]["value"]
        with open(__file__ + ".options", "a", newline="\n") as log:
            log.write(value + "\n")
        time.sleep(0.02)
        if value == "bad":
            send({"jsonrpc": "2.0", "id": i, "error": {"code": -32602, "message": "model unavailable"}})
        else:
            send({"jsonrpc": "2.0", "id": i, "result": {"configOptions": options(value)}})
    elif method == "session/list":"#);
    std::fs::write(&script, source).unwrap();
    let mut cx = TestAppContext::single();
    cx.update(|app| { gpui_kit::init(app); crate::panels::theme::install(app); });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| {
        ws.acp_server = Some(server.clone());
        ws.new_session_from_home(Harness::Codex, None, work.path().into(), None, None, None, window, cx);
    })).unwrap();
    let id = ws.read_with(&cx, |ws, _| ws.active_id().unwrap().clone());
    let until = std::time::Instant::now() + Duration::from_secs(10);
    let thread = loop {
        cx.run_until_parked();
        let thread = ws.read_with(&cx, |ws, _| ws.live_view(&id).and_then(SessionView::thread).cloned()).unwrap();
        if thread.read_with(&cx, |v, _| !v.is_loading()) { break thread; }
        assert!(std::time::Instant::now() < until, "adapter did not start");
        std::thread::yield_now();
    };
    thread.update(&mut cx, |v, cx| {
        for value in ["b", "skipped", "c"] { v.set_option("model".into(), value.into(), cx); }
    });
    let until = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if thread.read_with(&cx, |v, _| v.thread.option("model").is_some_and(|o| o.current == "c")) { break; }
        assert!(std::time::Instant::now() < until, "latest selection was not confirmed");
        std::thread::yield_now();
    }
    assert_eq!(std::fs::read_to_string(work.path().join("fake_agent.py.options")).unwrap(), "b\nc\n");
    thread.update(&mut cx, |v, cx| v.set_option("model".into(), "bad".into(), cx));
    let until = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if thread.read_with(&cx, |v, _| v.thread.entries.iter().any(|e| matches!(e, crate::harness::thread::Entry::Notice { text, error: true, .. } if text.contains("model unavailable")))) { break; }
        assert!(std::time::Instant::now() < until, "rejection was not surfaced");
        std::thread::yield_now();
    }
    assert_eq!(thread.read_with(&cx, |v, _| v.thread.option("model").unwrap().current.clone()), "c");
}
