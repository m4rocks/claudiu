use super::*;

#[test]
#[ignore = "drives the workspace"]
fn skills_page_installs_toggles_syncs_and_removes() {
    use crate::skills::registry::CatalogEntry;
    use crate::skills::{InstalledSkill, global_skills_dir, project_skills_dir};

    let data = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let proj_dir = tempfile::tempdir().unwrap();
    // Create Antigravity's config dir so both Claude (default) and Antigravity count as active harnesses.
    std::fs::create_dir_all(home.path().join(".gemini").join("config")).unwrap();
    // SAFETY: test-isolated data dir.
    unsafe { std::env::set_var("M4CODE_DATA_DIR", data.path()) };

    let own = proj_dir.path().join(".claude").join("skills").join("own-skill");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("SKILL.md"), "---\nname: own-skill\ndescription: Ships with the repo\n---\n").unwrap();

    let mut cx = TestAppContext::single();
    cx.update(|app| {
        gpui_kit::init(app);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
    });
    let (window, ws) = cx
        .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| Workspace::new(window, vec![proj_dir.path().to_path_buf()], cx))))
        .unwrap();

    fn fake_popular() -> Result<Vec<CatalogEntry>, String> {
        Ok(vec![CatalogEntry {
            source: "acme/demo-skills".into(),
            skill_id: "demo-skill".into(),
            name: "demo-skill".into(),
            installs: 12_345,
            is_official: true,
        }])
    }
    fn fake_install(root: &std::path::Path, entry: &CatalogEntry) -> Result<InstalledSkill, String> {
        let dir = root.join(&entry.name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("SKILL.md"), "---\nname: demo-skill\ndescription: Demo skill\n---\n# Demo\n").map_err(|e| e.to_string())?;
        Ok(InstalledSkill {
            name: entry.name.clone(),
            source: entry.source.clone(),
            skill_id: entry.skill_id.clone(),
            description: "Demo skill".into(),
            installed_at: 1,
        })
    }

    let home_path = home.path().to_path_buf();
    fn fake_search(query: &str) -> Result<Vec<CatalogEntry>, String> {
        assert_eq!(query, "demo");
        fake_popular()
    }

    ws.update(&mut cx, |ws, _| {
        ws.skills_page.fetch_search = fake_search;
        ws.store.data.settings.enabled_harnesses = Some(vec![Harness::Claude]);
        for agent in crate::harness::subagents::Agent::ALL {
            ws.store.data.settings.custom_subagents.get_mut(agent).on = false;
        }
        ws.skills_page.home_override = Some(home_path.clone());
        ws.skills_page.fetch_popular = fake_popular;
        ws.skills_page.install_pkg = fake_install;
    });

    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
        cx.run_until_parked();
    };
    let wait = |cx: &mut TestAppContext, what: &str, done: &dyn Fn(&Workspace) -> bool| {
        let t = std::time::Instant::now();
        while !ws.read_with(cx, |ws, _| done(ws)) {
            assert!(t.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
            draw(cx);
        }
    };

    // Open Settings → Skills → Browse and wait for the catalog.
    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            ws.act(Act::OpenSettings, window, cx);
            ws.set_settings_page(SettingsPage::Skills, cx);
            ws.act(Act::SetSkillsTab(SkillsTab::Browse), window, cx);
        });
    })
    .unwrap();
    draw(&mut cx);
    wait(&mut cx, "popular catalog", &|ws| ws.skills_page.popular.as_ref().is_some_and(|p| p.len() == 1));

    // Install the skill while scope is Global; symlinks should appear in active harnesses' global dirs.
    let entry = ws.read_with(&cx, |ws, _| ws.skills_page.popular.as_ref().unwrap()[0].clone());
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.act(Act::InstallSkill(entry), window, cx))).unwrap();
    wait(&mut cx, "global skill sync", &|ws| {
        ws.store.data.settings.skills.is_global("demo-skill")
            && global_skills_dir(Harness::Claude, &home_path).join("demo-skill").join("SKILL.md").is_file()
            && global_skills_dir(Harness::Antigravity, &home_path).join("demo-skill").join("SKILL.md").is_file()
    });
    assert!(!global_skills_dir(Harness::Codex, &home_path).exists(), "inactive harness is untouched");

    // Toggle off globally and enable per-project instead.
    let project_id = ws.read_with(&cx, |ws, _| ws.store.data.projects[0].id.clone());
    cx.update_window(window, |_, window, app| {
        ws.update(app, |ws, cx| {
            ws.act(Act::SetSkillsTab(SkillsTab::Installed), window, cx);
            ws.act(Act::ToggleSkill("demo-skill".into()), window, cx);
            ws.act(Act::SetSkillScope(SkillScope::Project(project_id.clone())), window, cx);
            ws.act(Act::ToggleSkill("demo-skill".into()), window, cx);
        });
    })
    .unwrap();
    draw(&mut cx);
    let proj_path = ws.read_with(&cx, |ws, _| ws.store.data.projects[0].path.clone());
    wait(&mut cx, "project skill sync", &|_| {
        std::fs::symlink_metadata(global_skills_dir(Harness::Claude, &home_path).join("demo-skill")).is_err()
            && project_skills_dir(Harness::Claude, &proj_path).join("demo-skill").join("SKILL.md").is_file()
            && project_skills_dir(Harness::Antigravity, &proj_path).join("demo-skill").join("SKILL.md").is_file()
    });

    // The project's own skill shows under the repository, and m4code's link to demo-skill does not.
    wait(&mut cx, "repo skills", &|ws| {
        ws.skills_page.repo_skills.as_ref().is_some_and(|(id, found)| *id == project_id && found.iter().map(|s| s.name.as_str()).eq(["own-skill"]))
    });

    // Updating downloads again through the install path and keeps the project toggle and its links.
    ws.update(&mut cx, |ws, _| ws.store.data.settings.skills.installed[0].installed_at = 0);
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.act(Act::UpdateSkills(vec!["demo-skill".into()]), window, cx))).unwrap();
    wait(&mut cx, "skill update", &|ws| {
        ws.skills_page.installing.is_empty()
            && ws.store.data.settings.skills.installed[0].installed_at == 1
            && ws.store.data.settings.skills.is_project(&project_id, "demo-skill")
            && project_skills_dir(Harness::Claude, &proj_path).join("demo-skill").join("SKILL.md").is_file()
    });

    // Uninstall cleans up both project symlinks and the canonical stored folder.
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| ws.act(Act::UninstallSkill("demo-skill".into()), window, cx))).unwrap();
    wait(&mut cx, "skill uninstall", &|ws| {
        !ws.store.data.settings.skills.is_installed("demo-skill")
            && std::fs::symlink_metadata(project_skills_dir(Harness::Claude, &proj_path).join("demo-skill")).is_err()
            && !data.path().join("skills").join("demo-skill").exists()
    });

    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| {
        ws.act(Act::SetSkillsTab(SkillsTab::Browse), window, cx);
        let Some(Modal::Settings { skill_search, .. }) = &ws.modal else { panic!("settings should be open") };
        skill_search.update(cx, |input, cx| input.set_value("demo", window, cx));
        ws.act(Act::SearchSkills, window, cx);
    })).unwrap();
    wait(&mut cx, "catalog search", &|ws| ws.skills_page.results.as_ref().is_some_and(|rows| rows.len() == 1));
    cx.update_window(window, |_, window, app| ws.update(app, |ws, cx| {
        ws.act(Act::Dismiss, window, cx);
        ws.act(Act::OpenSettings, window, cx);
        let Some(Modal::Settings { skill_search, .. }) = &ws.modal else { panic!("settings should reopen") };
        assert_eq!(skill_search.read(cx).value().as_ref(), "demo");
    })).unwrap();
}

