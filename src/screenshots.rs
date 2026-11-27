use anyhow::{Result, ensure};
#[cfg(not(target_os = "macos"))]
use anyhow::Context as _;
use gpui::{HeadlessAppContext, Keystroke, PlatformHeadlessRenderer};
use gpui_wgpu::CosmicTextSystem;
use std::{path::Path, sync::Arc};

#[cfg(target_os = "macos")]
fn renderer() -> Result<Option<Box<dyn PlatformHeadlessRenderer>>> {
    Ok(Some(Box::new(
        gpui_macos::metal_renderer::MetalHeadlessRenderer::new(),
    )))
}

#[cfg(not(target_os = "macos"))]
fn renderer() -> Result<Option<Box<dyn PlatformHeadlessRenderer>>> {
    let renderer = gpui_wgpu::WgpuHeadlessRenderer::new()
        .context("initialize GPUI's offscreen renderer with a Vulkan or OpenGL adapter")?;
    Ok(Some(Box::new(renderer)))
}

#[test]
#[ignore = "requires an offscreen GPU renderer; run with just screenshot"]
fn capture() -> Result<()> {
    let scenario = std::env::var("M4CODE_SCREENSHOT_SCENARIO").unwrap_or_else(|_| "home".into());
    let session_scenario = matches!(scenario.as_str(), "session" | "session-auth" | "session-no-compact" | "session-scrolled" | "session-quotes" | "session-quotes-menu" | "session-events" | "toast" | "review" | "review-hover");
    let shortcut = match scenario.as_str() {
        "home" | "dashboards-disabled" | "picker" | "context-menu" | "github" | "github-wide" | "github-issues" | "editor" | "session" | "session-auth" | "session-no-compact" | "session-scrolled" | "session-quotes" | "session-quotes-menu" | "session-events" | "toast" | "harnesses" | "review" | "review-hover" => None,
        "settings" => Some(Keystroke::parse("secondary-,").expect("Settings shortcut")),
        _ => anyhow::bail!("unknown screenshot scenario {scenario:?}; use home, dashboards-disabled, settings, picker, context-menu, github, github-wide, editor, session, session-no-compact, session-scrolled, toast, harnesses, review or review-hover"),
    };

    let fixture = tempfile::tempdir()?;
    // The recipe selects only this test. Set paths before initializing GPUI or starting app tasks.
    unsafe {
        std::env::set_var("M4CODE_DATA_DIR", fixture.path().join("m4code"));
        std::env::set_var("CLAUDE_CONFIG_DIR", fixture.path().join("claude"));
        std::env::set_var("CODEX_HOME", fixture.path().join("codex"));
    }
    let path = crate::store::Store::default_path();
    let mut store = crate::store::Store::load(path.clone());
    let settings = &mut store.data.settings;
    // Keep the fixture offline and independent of installed CLIs, usage counters, and live model catalogs.
    settings.update_url = Some(String::new());
    settings.enabled_harnesses = Some(Vec::new());
    if matches!(scenario.as_str(), "dashboards-disabled" | "session-auth") {
        settings.enabled_harnesses = Some(vec![crate::harness::Harness::Claude]);
    }
    settings.auto_compact_threshold = 75;
    settings.auto_compact = scenario != "session-no-compact";
    settings.custom_subagents = crate::harness::subagents::CustomSubagents(Default::default());
    if matches!(scenario.as_str(), "github" | "github-wide" | "github-issues") {
        // Request a wide pane; github-wide also widens the window to leave it enough room.
        store.data.layout.right_gh_w = Some(900.0);
    }
    if scenario == "github-wide" {
        store.data.layout.window = Some(crate::store::WindowGeom { x: 20.0, y: 20.0, w: 1920.0, h: 1080.0, maximized: false });
    }
    crate::datadir::write_atomic(&path, &serde_json::to_string(&store.data)?)?;

    let font = if cfg!(windows) {
        "Segoe UI"
    } else if cfg!(target_os = "macos") {
        "Helvetica"
    } else {
        "sans-serif"
    };
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(CosmicTextSystem::new(font)),
        Arc::new(crate::Assets),
        renderer,
    );
    let project = fixture.path().join("demo");
    if scenario == "editor" || session_scenario {
        sample_project(&project)?;
    }
    let projects = if scenario == "editor" || session_scenario { vec![dunce::canonicalize(&project)?] } else { Vec::new() };
    let (window, workspace) = cx.update(|app| {
        crate::init_ui(app);
        crate::open_main_window(projects.clone(), app)
    });
    workspace.update(&mut cx, |ws, _| {
        ws.model_lookups.list = |harness, _, _| Ok(crate::harness::HarnessCatalog {
            models: (harness.adapter().default_models)().into_iter().map(|m| crate::harness::subagents::AgentModel { id: m.id, name: m.name }).collect(),
            modes: crate::harness::permission::Permission::ALL.into_iter().filter_map(|p| p.native(harness).map(|id| (id.into(), p.label().into()))).collect(),
        });
    });
    cx.run_until_parked();
    if let Some(shortcut) = shortcut {
        let handled = cx.update_window(window, |_, window, app| {
            window.dispatch_keystroke(shortcut, app)
        })?;
        ensure!(handled, "the Settings shortcut was not handled");
        cx.run_until_parked();
    }
    if matches!(scenario.as_str(), "picker" | "context-menu") {
        cx.update_window(window, |_, window, app| {
            workspace.update(app, |ws, cx| {
                let position = gpui::point(gpui::px(380.0), gpui::px(260.0));
                if scenario == "picker" {
                    ws.open_home_permission_picker(crate::panels::widgets::Dropdown::at(position), window, cx);
                    ws.picker_nav.reset_to(1);
                } else {
                    ws.open_menu(position, vec![
                        crate::app::MenuItem::new("Settings", crate::app::Act::OpenSettings),
                        crate::app::MenuItem::new("Dismiss", crate::app::Act::Dismiss),
                    ], window, cx);
                }
            });
        })?;
        cx.run_until_parked();
    }
    if matches!(scenario.as_str(), "github" | "github-wide" | "github-issues") {
        let root = fixture.path().to_path_buf();
        cx.update_window(window, |_, window, app| workspace.update(app, |ws, cx| ws.open_github(root, window, cx)))?;
        cx.run_until_parked();
        cx.update_window(window, |_, _, app| {
            let view = workspace.read(app).right_gh.as_ref().and_then(|id| workspace.read(app).gh_pane(id)).map(|p| p.view.clone());
            let view = view.expect("GitHub screenshot pane must be open");
            {
                view.update(app, |v, cx| {
                    github_sample(v);
                    if scenario == "github-issues" {
                        v.kind = crate::github::model::Kind::Issue;
                        v.lists[0].items = v.lists[1].items.clone();
                        for item in &mut v.lists[0].items { item.kind = crate::github::model::Kind::Issue; }
                        if let Some(detail) = &mut v.detail {
                            detail.kind = crate::github::model::Kind::Issue;
                            if let Some(data) = &mut detail.data { data.item.kind = crate::github::model::Kind::Issue; data.pr = None; }
                        }
                    }
                    cx.notify();
                });
            }
        })?;
        cx.run_until_parked();
    }
    if session_scenario {
        cx.update_window(window, |_, window, app| workspace.update(app, |ws, cx| {
            ws.start_screenshot_session(projects[0].clone(), window, cx);
        }))?;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update_window(window, |_, _, app| {
                let ws = workspace.read(app);
                ws.active_id().and_then(|id| ws.live_view(id)).and_then(|v| v.thread()).is_some_and(|v| !v.read(app).is_loading())
            })?;
            if ready { break; }
            ensure!(std::time::Instant::now() < until, "offline screenshot session did not open");
            std::thread::yield_now();
        }
        cx.update_window(window, |_, window, app| workspace.update(app, |ws, cx| {
            if let Some(view) = ws.active_id().and_then(|id| ws.live_view(id)).and_then(|v| v.thread()).cloned() {
                view.update(cx, |v, cx| v.screenshot_activity(scenario != "session-no-compact", scenario == "session-scrolled", cx));
                if matches!(scenario.as_str(), "session-quotes" | "session-quotes-menu") {
                    view.update(cx, |v, cx| v.screenshot_quoted_context(scenario == "session-quotes-menu", cx));
                }
                if scenario == "session-events" {
                    let now = crate::store::now();
                    ws.store.data.claude_usage.seven_day = Some(crate::store::RateLimit { utilization: Some(1.0), resets_at: Some(now + 300), seen_at: now });
                    ws.store.data.settings.enabled_harnesses = Some(vec![crate::harness::Harness::Codex]);
                    ws.store.data.settings.set_harness_models(crate::harness::Harness::Codex, vec![crate::harness::subagents::AgentModel { id: "gpt".into(), name: "Codex".into() }]);
                    ws.store.data.settings.auto_handoff_on_limit = false;
                    ws.store.data.settings.notify_model_on_limit = false;
                    view.update(cx, |v, cx| v.screenshot_events(cx));
                }
                if scenario == "session-auth" {
                    view.update(cx, |v, cx| v.screenshot_login_failure(cx));
                    let state = ws.limits_state.get_mut(crate::harness::Harness::Claude);
                    state.authentication_required = true;
                    state.error = Some("Usage is stale. Sign in to refresh it.".into());
                    cx.notify();
                }
                if matches!(scenario.as_str(), "review" | "review-hover") {
                    view.update(cx, |v, cx| v.screenshot_review(cx));
                }
            }
            if scenario == "toast" {
                if let Some(project) = ws.store.data.projects.first().map(|project| project.id.clone()) {
                    ws.act(crate::app::Act::RemoveProject(project), window, cx);
                }
                ws.toast("Workspace settings saved", cx);
                ws.error_toast("Could not load GitHub: example error".into(), cx);
            }
        }))?;
        cx.run_until_parked();
    }
    if scenario == "harnesses" {
        cx.update_window(window, |_, window, app| workspace.update(app, |ws, cx| {
            ws.act(crate::app::Act::OpenSettings, window, cx);
            ws.settings_page = crate::app::SettingsPage::Harnesses;
        }))?;
        cx.run_until_parked();
    }
    if scenario == "editor" {
        let file = dunce::canonicalize(&project)?.join("src").join("main.rs");
        cx.update_window(window, |_, window, app| {
            workspace.update(app, |ws, cx| {
                if let Some(id) = ws.store.data.projects.first().map(|p| p.id.clone()) {
                    ws.act(crate::app::Act::HomeSelectProject(id), window, cx);
                }
                ws.toggle_editor(window, cx);
                if let Some(id) = ws.shown_pane(cx).map(|(id, _)| id) {
                    ws.open_file(&id, file.clone(), None, window, cx);
                }
            })
        })?;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let open = cx.update_window(window, |_, _, app| {
                let ws = workspace.read(app);
                ws.editor_owner().and_then(|id| ws.panes.get(&id)).is_some_and(|pane| pane.shown().is_some_and(|tab| !tab.loading))
            })?;
            if open {
                break;
            }
            ensure!(std::time::Instant::now() < until, "the sample file never opened");
            std::thread::yield_now();
        }
    }
    if scenario == "session-events" {
        cx.update_window(window, |_, window, app| {
            window.resize(gpui::size(gpui::px(1440.0), gpui::px(2600.0)));
            window.bounds_changed(app);
        })?;
        cx.run_until_parked();
    }
    cx.update_window(window, |_, window, app| window.draw(app).clear(app))?;
    if scenario == "review-hover" {
        cx.update_window(window, |_, window, app| {
            let view = workspace.read(app).active_id().and_then(|id| workspace.read(app).live_view(id)).and_then(|v| v.thread()).expect("session fixture").clone();
            let before = view.read(app).screenshot_review_line();
            // List item bounds exclude the review list's 12px top padding.
            window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                position: before.center() + gpui::point(gpui::px(0.0), gpui::px(12.0)), pressed_button: None, modifiers: Default::default(),
            }), app);
            window.draw(app).clear(app);
            assert_eq!(before, view.read(app).screenshot_review_line(), "hover must not shift the diff line");
        })?;
    }
    // Opening overlays starts their fade on the first draw. Capture their settled state.
    std::thread::sleep(std::time::Duration::from_millis(250));
    cx.run_until_parked();
    cx.update_window(window, |_, window, app| window.draw(app).clear(app))?;
    if scenario == "github-wide" {
        let width = cx.update_window(window, |_, _, app| {
            let ws = workspace.read(app);
            ws.right_gh.as_ref().and_then(|id| ws.gh_pane(id)).map(|p| p.view.read(app).width)
        })?;
        ensure!(width.is_some_and(|w| w >= 860.0), "GitHub fixture did not reach the side-by-side breakpoint: {width:?}");
    }
    let frame = cx.capture_screenshot(window)?;
    ensure!(
        frame.pixels().any(|pixel| pixel != frame.get_pixel(0, 0)),
        "GPUI rendered a blank frame"
    );

    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/screenshots");
    std::fs::create_dir_all(&directory)?;
    let output = directory.join(format!("{scenario}.png"));
    let mut png = tempfile::NamedTempFile::new_in(directory)?;
    frame.write_to(&mut png, image::ImageFormat::Png)?;
    png.persist(&output)?;
    println!(
        "{} ({}×{})",
        output.display(),
        frame.width(),
        frame.height()
    );
    Ok(())
}

/// A small Rust project for the editor scenario.
fn sample_project(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n")?;
    std::fs::write(root.join("README.md"), "# demo\n\nA tiny greeter.\n")?;
    std::fs::write(root.join("src").join("lib.rs"), "pub fn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n")?;
    std::fs::write(
        root.join("src").join("main.rs"),
        "//! Prints a greeting for each name on the command line.\n\nuse demo::greet;\n\nfn main() {\n    let names: Vec<String> = std::env::args().skip(1).collect();\n    if names.is_empty() {\n        println!(\"{}\", greet(\"world\"));\n        return;\n    }\n    for name in &names {\n        println!(\"{}\", greet(name));\n    }\n}\n",
    )?;
    Ok(())
}

/// The GitHub pane with a repository, a list of pull requests and one of them open, all made up.
fn github_sample(v: &mut crate::github::view::GithubView) {
    use crate::github::model::*;
    use crate::github::view::{DetailState, DetailTab, Phase};
    let now = crate::store::now();
    // Label colors are GitHub data, given as GitHub sends them.
    let label = |name: &str, color: &str| Label { id: name.into(), name: name.into(), color: u32::from_str_radix(color, 16).unwrap_or_default() };
    let user = |login: &str| User { id: login.into(), login: login.into() };
    #[allow(clippy::too_many_arguments)]
    let item = |number: u32, title: &str, state: State, draft: bool, author: &str, ago_h: i64, labels: Vec<Label>, checks: Option<&str>, review: Option<&str>, comments: u32| Item {
        kind: Kind::Pull,
        id: format!("PR_{number}"),
        number,
        title: title.into(),
        state,
        not_planned: false,
        draft,
        url: format!("https://github.com/m4rocks/demo/pull/{number}"),
        author: author.into(),
        created: now - ago_h * 3600,
        updated: now - ago_h * 1800,
        comments,
        labels,
        assignees: vec![user(author)],
        milestone: None,
        review: review.map(Into::into),
        checks: checks.map(Into::into),
        head: format!("feature-{number}"),
        base: "main".into(),
    };
    let items = vec![
        item(482, "Keep long issue and pull request titles within the panel while the editor, conversation and repository controls remain accessible", State::Open, false, "ana", 3, vec![label("ui", "1d76db"), label("editor", "5319e7")], Some("SUCCESS"), Some("APPROVED"), 4),
        item(479, "Fix the flip-flop between working and done", State::Open, false, "codex-bot", 9, vec![label("bug", "d73a4a")], Some("FAILURE"), Some("CHANGES_REQUESTED"), 7),
        item(476, "Theme tokens for every color", State::Open, true, "mario", 26, vec![label("theme", "0e8a16")], Some("PENDING"), None, 1),
        item(471, "Kit buttons in Settings", State::Merged, false, "ana", 50, vec![], Some("SUCCESS"), Some("APPROVED"), 2),
        item(468, "Drop the legacy color aliases", State::Closed, false, "mario", 80, vec![label("cleanup", "fbca04")], None, None, 0),
    ];
    v.phase = Phase::Ready;
    v.repo = Some(crate::github::cli::RepoRef { owner: "m4rocks".into(), name: "demo".into(), url: "https://github.com/m4rocks/demo".into(), host: None });
    v.info = Some(RepoInfo { viewer: "mario".into(), permission: "ADMIN".into(), issues_enabled: true, allow_merge: true, allow_squash: true, default_branch: "main".into(), ..Default::default() });
    v.kind = Kind::Pull;
    v.queries[1] = "is:open".into();
    let list = &mut v.lists[1];
    (list.total, list.open, list.closed, list.loaded, list.loading) = (items.len(), 3, 2, true, false);
    list.items = items.clone();
    let issues = &mut v.lists[0];
    (issues.total, issues.open, issues.closed, issues.loaded, issues.loading) = (0, 12, 40, true, false);

    let pr = Pr {
        mergeable: "MERGEABLE".into(),
        merge_state: "CLEAN".into(),
        head_repo: "m4rocks/demo".into(),
        additions: 86,
        deletions: 12,
        files: 3,
        commits: 2,
        reviews: vec![("mario".into(), "APPROVED".into())],
        requested: vec!["sol".into()],
        checks: vec![
            Check { name: "lint".into(), result: "SUCCESS".into(), url: String::new() },
            Check { name: "test (windows)".into(), result: "SUCCESS".into(), url: String::new() },
        ],
        ..Default::default()
    };
    let detail = Detail {
        item: items[0].clone(),
        body: "The editor now shows where the open file sits in the project.\n\n- One segment per folder\n- The file name in the foreground\n\nCloses #470.".into(),
        assoc: "MEMBER".into(),
        edited: false,
        locked: false,
        mine: false,
        can_update: true,
        reactions: vec![Reaction { content: "THUMBS_UP".into(), count: 3, mine: true }, Reaction { content: "ROCKET".into(), count: 1, mine: false }],
        timeline: vec![
            Entry::Commit(CommitEntry { short: "a1c4e84".into(), headline: "Add the breadcrumb component".into(), author: "ana".into(), created: now - 3 * 3600 }),
            Entry::Event(Event { actor: "ana".into(), what: "added the label".into(), label: Some(label("ui", "1d76db")), created: now - 3 * 3600 }),
            Entry::Comment(Comment {
                id: "c1".into(),
                author: "mario".into(),
                assoc: "OWNER".into(),
                body: "Looks good. Could the last segment stay bold?".into(),
                created: now - 2 * 3600,
                edited: false,
                can_edit: false,
                can_delete: false,
                reactions: vec![],
            }),
            Entry::Review(Review { author: "mario".into(), state: "APPROVED".into(), body: "Ship it.".into(), created: now - 3600, comments: vec![] }),
        ],
        next: None,
        pr: Some(pr),
    };
    let mut state = DetailState::new(Kind::Pull, 482);
    (state.data, state.loading, state.tab) = (Some(detail), false, DetailTab::Conversation);
    v.detail = Some(state);
}
