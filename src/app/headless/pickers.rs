use super::*;

#[test]
#[ignore = "drives the workspace"]
fn dropdowns_switch_on_one_click_and_load_all_harness_models() {
    use gpui::px;
    use std::time::Instant;

    let _data = crate::datadir::own_folder();
    let proj_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(proj_dir.path()).unwrap();

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
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
                models: vec![crate::harness::subagents::AgentModel { id: "claude-opus-4-6".into(), name: "Opus 4.6".into() }],
                modes: vec![("default".into(), "Ask Before Editing".into()), ("bypassPermissions".into(), "Full Access".into())],
            }),
            Harness::Codex => Ok(crate::harness::HarnessCatalog {
                models: vec![crate::harness::subagents::AgentModel { id: "gpt-5.4".into(), name: "GPT-5.4".into() }],
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
    let _data = crate::datadir::own_folder();
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
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

/// A workspace on the test platform, its window active so keys reach it.
fn keyboard_workspace(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<Workspace>) {
    cx.update(|app| {
        gpui_kit::init(app);
        crate::panels::theme::install(app);
    });
    let (window, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![], cx)))).unwrap();
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    (window, ws)
}

#[test]
#[ignore = "drives the workspace"]
/// A dropdown with no search box takes the keyboard itself, and hands it back to Settings when it closes.
fn dropdowns_without_a_search_box_answer_the_arrow_keys() {
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            ws.act(Act::OpenSettings, window, cx);
            ws.open_settings_effort_picker(crate::panels::widgets::Dropdown::at(gpui::point(gpui::px(20.0), gpui::px(20.0))), window, cx);
        });
        window.draw(app).clear(app);
    })
    .unwrap();
    let mut visual = gpui::VisualTestContext::from_window(window, &cx);
    visual.simulate_keystrokes("down enter");
    cx.update_window(window, |_, window, app| {
        let ws = ws.read(app);
        assert!(ws.picker.is_none(), "Enter chooses and closes the dropdown");
        let Some(Modal::Settings { effort, list_focus, .. }) = &ws.modal else { panic!("Settings stays open") };
        assert_eq!(*effort, Effort::ALL[1], "Down moved to the second row");
        assert!(list_focus.is_focused(window), "the keyboard is back in Settings");
    })
    .unwrap();
}

#[test]
#[ignore = "drives the workspace"]
/// Up and Down skip a menu's greyed-out rows, Enter runs the highlighted one, and Esc gives the keyboard back.
fn context_menus_answer_the_keyboard() {
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    let open = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| {
            ws.update(app, |ws, cx| {
                window.focus(&ws.root_focus, cx);
                let items = vec![
                    MenuItem::new("First", Act::Dismiss),
                    MenuItem { disabled: true, ..MenuItem::new("Greyed out", Act::Dismiss) },
                    MenuItem::new("Settings", Act::OpenSettings),
                ];
                ws.open_menu(gpui::point(gpui::px(20.0), gpui::px(20.0)), items, window, cx);
            });
            window.draw(app).clear(app);
        })
        .unwrap();
    };

    open(&mut cx);
    let mut visual = gpui::VisualTestContext::from_window(window, &cx);
    visual.simulate_keystrokes("escape");
    cx.update_window(window, |_, window, app| {
        let ws = ws.read(app);
        assert!(ws.menu.is_none(), "Esc closes the menu");
        assert!(ws.root_focus.is_focused(window), "the keyboard goes back to what had it");
    })
    .unwrap();

    open(&mut cx);
    visual.simulate_keystrokes("down enter");
    ws.read_with(&visual, |ws, _| {
        assert!(ws.menu.is_none());
        assert!(matches!(ws.modal, Some(Modal::Settings { .. })), "Down skipped the greyed-out row to Settings");
    });
}



#[test]
#[ignore = "drives the workspace"]
fn text_generation_picker_saves_a_model_independently_of_new_sessions() {
    let _data = crate::datadir::own_folder();
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
            settings.set_harness_models(Harness::Claude, vec![crate::harness::subagents::AgentModel { id: "full".into(), name: "Full".into() }]);
            settings.set_harness_models(Harness::Codex, vec![crate::harness::subagents::AgentModel { id: "light".into(), name: "Light".into() }]);
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
                ws.open_settings_model_picker(at, crate::app::SettingsModelTarget::TextGeneration, window, cx);
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

#[test]
#[ignore = "drives the workspace"]
/// Command-K only lists active (running) sessions, not closed/recent ones.
fn command_palette_lists_only_active_sessions() {
    let _data = crate::datadir::own_folder();
    let dir = tempfile::tempdir().unwrap();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    let closed_id = "closed-session-id".to_string();
    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            ws.store.data.sessions.push(crate::store::SessionRecord {
                id: closed_id.clone(),
                title: "Closed Session".into(),
                custom_title: None,
                auto_title: None,
                cwd: dir.path().to_path_buf(),
                project_id: None,
                kind: crate::store::SessionKind::Agent,
                agent: Some(crate::store::AgentMeta { harness: Harness::Claude, ..Default::default() }),
                created_at: 1,
                last_active: 1,
                exit_code: None,
                hidden: false,
            });
            ws.open_palette(window, cx);
        });
    })
    .unwrap();
    ws.read_with(&cx, |ws, _| {
        let p = ws.palette.as_ref().expect("palette is open");
        let session_titles: Vec<&str> = p.items.iter().filter(|i| i.badge == "Session").map(|i| i.title.as_str()).collect();
        assert!(!session_titles.contains(&"Closed Session"), "closed sessions must not appear in Command-K: {session_titles:?}");
    });
}

#[test]
#[ignore = "drives the workspace"]
fn settings_tab_reaches_model_button_and_keyboard_opens_it() {
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| ws.act(Act::OpenSettings, window, cx));
        window.draw(app).clear(app);
    }).unwrap();
    let mut visual = VisualTestContext::from_window(window, &cx);
    visual.simulate_keystrokes("tab");
    let trigger_focus = visual.update(|window, app| window.focused(app).unwrap());
    let keystroke = gpui::Keystroke::parse("enter").unwrap();
    visual.simulate_event(gpui::KeyDownEvent { keystroke: keystroke.clone(), is_held: false, prefer_character_input: false });
    visual.simulate_event(gpui::KeyUpEvent { keystroke });
    assert!(ws.read_with(&visual, |ws, _| matches!(ws.picker_kind(), Some(WorkspacePicker::SettingsModel { target: crate::app::SettingsModelTarget::Default, .. }))),
        "Tab must reach the model button instead of skipping it for the Shell field");
    visual.simulate_keystrokes("escape");
    visual.update(|window, _| assert!(trigger_focus.is_focused(window), "Closing the picker must restore its trigger's focus"));
}

#[test]
#[ignore = "drives the workspace"]
fn settings_tab_wraps_without_focusing_the_workspace_behind_it() {
    struct CrowdedWorkspace(Entity<Workspace>);
    impl gpui::Render for CrowdedWorkspace {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            use gpui::{ParentElement, Styled};
            gpui::div().size_full()
                .children((0..150).map(|i| crate::panels::ui::button::text(format!("outside-stop-{i}"), "Outside", false).absolute().size(gpui::px(1.0)).opacity(0.0)))
                .child(self.0.clone())
        }
    }
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    cx.update(|app| { gpui_kit::init(app); crate::panels::theme::install(app); });
    let (window, host) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| {
        let ws = app.new(|cx| Workspace::new(window, vec![], cx));
        app.new(|_| CrowdedWorkspace(ws))
    })).unwrap();
    let ws = host.read_with(&cx, |host, _| host.0.clone());
    cx.update_window(window, |_, window, app| {
        window.activate_window();
        ws.update(app, |ws, cx| ws.act(Act::OpenSettings, window, cx));
        window.draw(app).clear(app);
    }).unwrap();
    let mut visual = VisualTestContext::from_window(window, &cx);
    visual.simulate_keystrokes("shift-tab");
    visual.update(|window, app| {
        assert!(ws.read(app).modal_focus.contains_focused(window, app), "Shift+Tab must stay in Settings");
    });
}

#[test]
#[ignore = "drives the workspace"]
fn a_focused_confirmation_accepts_enter_without_first_tabbing_to_a_button() {
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            ws.modal = Some(Modal::Confirm { title: "Confirm".into(), body: "Continue?".into(), confirm: "Continue".into(), danger: false, act: Act::Dismiss });
            window.focus(&ws.modal_focus, cx);
        });
        window.draw(app).clear(app);
    }).unwrap();
    let mut visual = VisualTestContext::from_window(window, &cx);
    visual.simulate_keystrokes("enter");
    assert!(ws.read_with(&visual, |ws, _| ws.modal.is_none()), "Enter must submit the focused confirmation");
}

#[test]
#[ignore = "drives the workspace"]
fn toasts_preserve_actions_and_error_severity_in_delivery_order() {
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    cx.update_window(window, |_, _, app| ws.update(app, |ws, cx| {
        ws.show_toast("Removed project".into(), Some(("Undo".into(), Act::Dismiss)), cx);
        ws.toast("Copied", cx);
        ws.error_toast("Could not load GitHub".into(), cx);
        assert_eq!(ws.toast.as_ref().unwrap().text, "Removed project");
        assert!(ws.toast.as_ref().unwrap().action.is_some());
        ws.dismiss_toast(cx);
        assert_eq!(ws.toast.as_ref().unwrap().text, "Copied");
        ws.dismiss_toast(cx);
        assert_eq!(ws.toast.as_ref().unwrap().text, "Could not load GitHub");
        assert_eq!(ws.toast.as_ref().unwrap().tone, crate::panels::ui::alert::Tone::Destructive);
        ws.dismiss_toast(cx);
        assert!(ws.toast.is_none());
    })).unwrap();
}

#[test]
#[ignore = "drives the workspace"]
fn settings_reports_both_invalid_executable_paths() {
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| {
        ws.act(Act::OpenSettings, window, cx);
        if let Some(Modal::Settings { shell, claude, .. }) = &ws.modal {
            shell.update(cx, |input, cx| input.set_value("m4code-nonexistent-shell-768491", window, cx));
            claude.update(cx, |input, cx| input.set_value("m4code-nonexistent-claude-768491", window, cx));
        }
        ws.close_settings(window, cx);
        let message = &ws.toast.as_ref().expect("validation feedback").text;
        assert!(message.contains("Shell:"));
        assert!(message.contains("Claude path:"));
    })).unwrap();
}

#[test]
#[ignore = "drives the workspace"]
fn enter_on_cancel_does_not_run_the_confirmation_action() {
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            ws.modal = Some(Modal::Confirm { title: "Confirm".into(), body: "Continue?".into(), confirm: "Continue".into(), danger: true, act: Act::OpenSettings });
            window.focus(&ws.modal_focus, cx);
        });
        window.draw(app).clear(app);
    }).unwrap();
    let mut visual = VisualTestContext::from_window(window, &cx);
    visual.simulate_keystrokes("tab");
    let keystroke = gpui::Keystroke::parse("enter").unwrap();
    visual.simulate_event(gpui::KeyDownEvent { keystroke: keystroke.clone(), is_held: false, prefer_character_input: false });
    visual.simulate_event(gpui::KeyUpEvent { keystroke });
    assert!(ws.read_with(&visual, |ws, _| ws.modal.is_none()), "Cancel must dismiss rather than execute the action that opens Settings");
}

#[test]
#[ignore = "drives the workspace"]
fn category_enter_does_not_accept_an_inline_worktree_removal() {
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            ws.act(Act::OpenSettings, window, cx);
            ws.worktrees_page.asking = Some(crate::app::worktrees::Asked { folder: PathBuf::from("unused"), body: "Delete?".into(), confirm: "Delete".into(), danger: true, act: Act::CopyText("unexpected removal".into()), cancel_focus: cx.focus_handle(), focus_pending: std::cell::Cell::new(false) });
        });
        window.draw(app).clear(app);
    }).unwrap();
    let mut visual = VisualTestContext::from_window(window, &cx);
    visual.simulate_keystrokes("enter");
    assert!(ws.read_with(&visual, |ws, _| ws.visible_toasts().all(|toast| toast.text != "Copied")), "Category Enter must not execute the pending removal action");
}

#[test]
#[ignore = "drives the workspace"]
fn repeated_toasts_merge_and_each_visible_toast_dismisses_independently() {
    let _data = crate::datadir::own_folder();
    let mut cx = TestAppContext::single();
    let (window, ws) = keyboard_workspace(&mut cx);
    cx.update_window(window, |_, _, app| ws.update(app, |ws, cx| {
        ws.show_toast("Removed".into(), Some(("Undo".into(), Act::Dismiss)), cx);
        ws.toast("Copied", cx);
        ws.toast("Copied", cx);
        assert_eq!(ws.visible_toasts().count(), 2);
        let serial = ws.visible_toasts().find(|toast| toast.text == "Copied").unwrap().serial;
        ws.dismiss_toast_serial(serial, cx);
        assert_eq!(ws.visible_toasts().count(), 1);
        assert_eq!(ws.toast.as_ref().unwrap().text, "Removed");
        ws.dismiss_toast_serial(serial, cx);
        assert_eq!(ws.visible_toasts().count(), 1, "an expired timer must not dismiss another toast");
    })).unwrap();
}
