use super::*;

#[test]
#[ignore = "drives the workspace"]
fn dropdowns_switch_on_one_click_and_load_all_harness_models() {
    use gpui::px;
    use std::time::Instant;

    let data = tempfile::tempdir().unwrap();
    let proj_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(proj_dir.path()).unwrap();
    // SAFETY: test-isolated data dir.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
        app.bind_keys(crate::app::line_action_bindings());
    });
    let (window, ws) = cx
        .update(|app| {
            gpui_kit::open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(gpui::point(px(0.0), px(0.0)), gpui::size(px(1280.0), px(800.0))))),
                    ..Default::default()
                },
                app,
                |window, app| app.new(|cx| Workspace::new(window, vec![proj_dir.path().to_path_buf()], cx)),
            )
        })
        .unwrap();

    // Inject a uniform lister for all harnesses (including Claude Code) and trigger background model loading.
    ws.update(&mut cx, |ws, cx| {
        ws.claude_exe = Some(PathBuf::from("/fake/claude"));
        ws.store.data.settings.enabled_harnesses = Some(vec![Harness::Claude, Harness::Codex]);
        ws.model_lookups.list = |harness, _cwd, _env| match harness {
            Harness::Claude => Ok(crate::harness::HarnessCatalog {
                models: vec![crate::claude::subagents::AgentModel { id: "claude-opus-4-6".into(), name: "Opus 4.6".into() }],
                modes: vec![("default".into(), "Ask Before Editing".into()), ("bypassPermissions".into(), "Full Access".into())],
            }),
            Harness::Codex => Ok(crate::harness::HarnessCatalog {
                models: vec![crate::claude::subagents::AgentModel { id: "gpt-5.4".into(), name: "GPT-5.4".into() }],
                modes: vec![("ask".into(), "Ask".into()), ("fullAccess".into(), "Full Access".into())],
            }),
            _ => Ok(Default::default()),
        };
        ws.load_enabled_models(cx);
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        let ready = ws.read_with(&cx, |ws, _| {
            let names: Vec<String> = ws.store.data.settings.aggregated_models().into_iter().map(|m| m.name).collect();
            names.contains(&"Opus 4.6".to_string()) && names.contains(&"GPT-5.4".to_string())
        });
        if ready {
            break;
        }
        assert!(Instant::now() < deadline, "all enabled harnesses should load models uniformly");
        std::thread::sleep(Duration::from_millis(20));
    }

    // Draw once so Anchor probes capture real bounds on the Welcome chips.
    cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();

    let click = |cx: &mut TestAppContext, x: f32, y: f32| {
        cx.update_window(window, |_, window, app| {
            let _ = window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    position: gpui::point(px(x), px(y)),
                    modifiers: gpui::Modifiers::none(),
                    button: gpui::MouseButton::Left,
                    click_count: 1,
                    first_mouse: false,
                }),
                app,
            );
            window.draw(app).clear(app);
        })
        .unwrap();
    };

    // Click the Welcome Model chip to open HomeModel, then read its exact painted bounds to locate the neighboring
    // Permission chip right beside it.
    let (model_pos, neighbor_pos) = {
        // Welcome composer is shifted slightly above center in the main pane. Scan horizontally for HomeModel first.
        let mut found_model = None;
        for y in (340..520).step_by(12) {
            for x in (340..480).step_by(20) {
                click(&mut cx, x as f32, y as f32);
                if ws.read_with(&cx, |ws, _| matches!(ws.picker_kind(), Some(crate::app::WorkspacePicker::HomeModel { .. }))) {
                    let b = ws.read_with(&cx, |ws, _| ws.picker.as_ref().unwrap().at.trigger);
                    found_model = Some((f32::from(b.center().x), f32::from(b.center().y), f32::from(b.right())));
                    break;
                }
            }
            if found_model.is_some() {
                break;
            }
        }
        let (mx, my, mright) = found_model.expect("should hit Welcome model chip");
        ((mx, my), (mright + 36.0, my))
    };

    // While HomeModel is open, a SINGLE click on the neighboring Permission chip must dismiss HomeModel and open HomePermission.
    click(&mut cx, neighbor_pos.0, neighbor_pos.1);
    assert!(
        ws.read_with(&cx, |ws, _| matches!(ws.picker_kind(), Some(crate::app::WorkspacePicker::HomePermission))),
        "clicking another trigger while a dropdown is open must switch immediately in one click"
    );

    // Clicking the Permission chip again while HomePermission is open toggles it closed.
    click(&mut cx, neighbor_pos.0, neighbor_pos.1);
    assert!(ws.read_with(&cx, |ws, _| ws.picker.is_none()), "clicking the active trigger again toggles its dropdown closed");

    // Clicking Model again opens HomeModel cleanly.
    click(&mut cx, model_pos.0, model_pos.1);
    assert!(ws.read_with(&cx, |ws, _| matches!(ws.picker_kind(), Some(crate::app::WorkspacePicker::HomeModel { .. }))));
}#[test]
#[ignore = "drives the workspace"]
/// Up, Down and Enter move through a dropdown's rows and choose one while the keyboard is in its search box.
fn dropdowns_answer_the_arrow_keys() {
    let data = tempfile::tempdir().unwrap();
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // SAFETY: test-isolated data dir.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let dirs = vec![one.path().to_path_buf(), two.path().to_path_buf()];
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, dirs, cx)))).unwrap();
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    let origin = crate::panels::widgets::Dropdown::at(gpui::point(gpui::px(0.0), gpui::px(0.0)));
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.open_project_picker(crate::app::ProjectPickerMode::Home, origin, window, cx))).unwrap();
    cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
    let mut visual = gpui::VisualTestContext::from_window(window, &cx);
    let second = ws.read_with(&visual, |ws, _| ws.store.data.projects[1].id.clone());
    // Two rows: down to the second, up to the first, down again (Down wraps past the last row).
    visual.simulate_keystrokes("down up down enter");
    ws.read_with(&visual, |ws, _| {
        assert!(ws.picker.is_none(), "Enter chooses and closes the dropdown");
        assert_eq!(ws.home_selected_project().map(|p| p.id.clone()), Some(second), "the keys moved to the second project");
    });
}



#[test]
#[ignore = "drives the workspace"]
fn text_generation_picker_saves_a_model_independently_of_new_sessions() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };
    let mut cx = TestAppContext::single();
    cx.update(gpui_kit::init);
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| {
        app.new(|cx| Workspace::new(window, vec![], cx))
    })).unwrap();

    let chosen = crate::store::TextGenerationModel { harness: Harness::Codex, id: "light".into() };
    let previous = cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            let settings = &mut ws.store.data.settings;
            settings.enabled_harnesses = Some(vec![Harness::Claude, Harness::Codex]);
            settings.set_harness_models(Harness::Claude, vec![crate::claude::subagents::AgentModel { id: "full".into(), name: "Full".into() }]);
            settings.set_harness_models(Harness::Codex, vec![crate::claude::subagents::AgentModel { id: "light".into(), name: "Light".into() }]);
            ws.model_lookups.list = |_, _, _| Err("Catalog preset by test.".into());
            let previous = settings.default_model();
            ws.act(Act::OpenSettings, window, cx);
            previous
        })
    }).unwrap();

    let open_picker = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| {
            ws.update(app, |ws, cx| {
                let at = crate::panels::widgets::Dropdown::at(gpui::point(gpui::px(20.0), gpui::px(20.0)));
                ws.open_settings_model_picker(at, true, window, cx);
            });
            window.draw(app).clear(app);
        }).unwrap();
    };
    let close_settings = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.act(Act::Dismiss, window, cx))).unwrap();
    };

    open_picker(&mut cx);
    let mut visual = gpui::VisualTestContext::from_window(window, &cx);
    visual.simulate_keystrokes("down down enter");
    assert!(ws.read_with(&visual, |ws, _| matches!(&ws.modal, Some(Modal::Settings { text_generation_model: Some(model), .. }) if model == &chosen)));
    close_settings(&mut cx);
    ws.read_with(&cx, |ws, _| {
        assert_eq!(ws.store.data.settings.text_generation_model.as_ref(), Some(&chosen));
        assert_eq!(ws.store.data.settings.default_model(), previous);
    });
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.act(Act::OpenSettings, window, cx))).unwrap();
    open_picker(&mut cx);
    cx.update_window(window, |_, window, app| {
        let Some(WorkspacePicker::SettingsModel { search, .. }) = ws.read(app).picker_kind() else {
            panic!("the text generation picker is open");
        };
        search.clone().update(app, |search, cx| search.set_value("local", window, cx));
        window.draw(app).clear(app);
    }).unwrap();
    visual.simulate_keystrokes("enter");
    close_settings(&mut cx);
    assert!(ws.read_with(&cx, |ws, _| ws.store.data.settings.text_generation_model.is_none()));
}
