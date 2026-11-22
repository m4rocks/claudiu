use super::*;

/// The worktree dialog and the switch of a session between worktrees, with a stand-in for `claude`, so what Claude Code
/// does in the new folder is not checked.
fn switching_between_worktrees_in_the_workspace() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
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
    ws.update(&mut cx, |ws, _| {
        ws.claude_exe = Some(fake.clone());
        ws.acp_server = Some(fake_agent(work.path()));
    });

    // A new session starts in the main checkout. The menu is there but cannot switch until a session runs.
    step(&mut cx, &|ws, w, cx| ws.act(Act::NewClaude { cwd: repo_dir.clone() }, w, cx));
    assert!(ws.read_with(&cx, |ws, _| ws.modal.is_none() && ws.live.len() == 1));
    let session = ws.read_with(&cx, |ws, _| active(ws));
    assert_eq!(session.cwd, repo_dir);
    // The conversation exists once Claude Code has had a prompt; the test stands one in, in Claude Code's own place.
    wait(&mut cx, "the session to open", &|ws, _| ws.store.session(&session.id).and_then(|r| r.claude_id()).is_some());
    let claude_id = ws.read_with(&cx, |ws, _| ws.store.session(&session.id).unwrap().claude_id().unwrap().to_string());
    let transcript = home.path().join("projects/p").join(format!("{claude_id}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "{\"type\":\"user\"}
").unwrap();
    wait(&mut cx, "the first screen", &|ws, app| ws.live.iter().all(|l| !l.view.is_loading(app) && !l.view.is_working(app)));

    // Files open in the editor when the session moves: two are committed, so the new worktree has them, and one is
    // ignored, so it only exists in the main checkout.
    let cli = |args: &[&str]| assert!(std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).current_dir(&repo_dir).status().unwrap().success());
    std::fs::write(repo_dir.join(".gitignore"), "only_main.txt\n").unwrap();
    std::fs::write(repo_dir.join("same.txt"), "committed\n").unwrap();
    std::fs::write(repo_dir.join("dirty.txt"), "committed\n").unwrap();
    std::fs::write(repo_dir.join("only_main.txt"), "ignored\n").unwrap();
    cli(&["add", "."]);
    cli(&["commit", "-q", "-m", "files"]);
    let id = session.id.clone();
    step(&mut cx, &|ws, w, cx| ws.toggle_editor(w, cx));
    wait(&mut cx, "listing the folder", &|ws, _| ws.panes.get(&id).is_some_and(|p| p.nodes.iter().any(|n| n.name == "same.txt")));
    for name in ["same.txt", "dirty.txt", "only_main.txt"] {
        step(&mut cx, &|ws, w, cx| ws.open_file(&id, repo_dir.join(name), None, w, cx));
    }
    wait(&mut cx, "the three tabs", &|ws, _| ws.panes[&id].tabs.len() == 3);
    step(&mut cx, &|ws, w, cx| {
        let tab = ws.panes[&id].tabs.iter().find(|t| t.path.ends_with("dirty.txt")).unwrap();
        tab.buffer.clone().update(cx, |b, cx| b.replace_all("edited\n", w, cx));
    });
    assert!(ws.read_with(&cx, |ws, _| ws.panes[&id].tabs.iter().any(|t| t.path.ends_with("dirty.txt") && t.dirty)), "the edit marks the tab");
    let tab_paths = |ws: &Workspace| ws.panes[&id].tabs.iter().map(|t| t.path.clone()).collect::<Vec<_>>();
    let canonical = |p: &std::path::Path| dunce::canonicalize(p).unwrap();
    assert_eq!(ws.read_with(&cx, |ws, _| ws.panes[&id].root.clone()), canonical(&repo_dir), "the file tree starts at the main checkout");

    // A shell is open under the session at a height of the user's choosing.
    step(&mut cx, &|ws, w, cx| {
        ws.toggle_shell(&id, w, cx);
        ws.shells.get_mut(&id).unwrap().height = 333.0;
    });
    let shell = |ws: &Workspace| ws.shells.get(&id).map(|p| (p.view.entity_id(), p.open, p.height));
    let (shell_before, ..) = ws.read_with(&cx, |ws, _| shell(ws)).unwrap();
    // The panels ease open over `Reveal::MS` of real time, so the pane's grid keeps changing for that long; how far
    // it has got depends on how fast the machine ran the steps above. The grid read below must be the settled one.

    // A bad name changes nothing; a typed one moves the session into the worktree Claude Code will make.
    let quick = |name: &str| Act::QuickWorktree { main_checkout: repo_dir.clone(), name: name.into(), base: crate::git::worktree::Base::Current, for_home: false };
    step(&mut cx, &|ws, w, cx| ws.act(quick("bad name"), w, cx));
    assert!(ws.read_with(&cx, |ws, app| ws.live_view(&session.id).and_then(|v| v.thread()).is_some_and(|v| v.read(app).handover.is_none())), "a bad name is refused");
    step(&mut cx, &|ws, w, cx| ws.act(quick("fix-login"), w, cx));
    let worktree = crate::git::worktree::folder(&repo_dir, "fix-login");
    let skeleton = |ws: &Workspace, app: &App| ws.live_view(&session.id).and_then(|v| v.thread()).and_then(|v| v.read(app).handover.as_ref().map(|h| h.message.clone()));
    assert_eq!(ws.read_with(&cx, |ws, app| skeleton(ws, app)).as_deref(), Some("Switching to fix-login…"), "the skeleton names the worktree");
    // Until the new process is up, the session stays shown and running behind its skeleton, never an empty pane.
    let moved_to = |folder: &std::path::Path, ws: &Workspace, app: &App| {
        let loading = ws.live_view(&session.id).is_some_and(|v| v.is_loading(app));
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

    // The shell is a new one, open and as tall as before.
    let (shell_now, open, height) = ws.read_with(&cx, |ws, _| shell(ws)).expect("the session still has a shell");
    assert!(shell_now != shell_before && open && height == 333.0, "a fresh shell, open at {height}");

    // The editor follows the session: a clean tab whose file the worktree has moves with it, one with unsaved edits or
    // whose file the worktree lacks stays where it was and the user is told, and the file tree shows the worktree.
    let (in_worktree, in_main) = (canonical(&worktree), canonical(&repo_dir));
    assert_eq!(ws.read_with(&cx, |ws, _| tab_paths(ws)), [in_worktree.join("same.txt"), in_main.join("dirty.txt"), in_main.join("only_main.txt")], "tabs after the switch");
    assert!(ws.read_with(&cx, |ws, _| ws.toast.as_ref().is_some_and(|t| t.text.contains("2 open files stay"))), "the toast counts the tabs that stayed");
    wait(&mut cx, "the file tree at the worktree", &|ws, _| ws.panes[&id].root == in_worktree && ws.panes[&id].nodes.iter().any(|n| n.name == "same.txt"));
    assert_eq!(ws.read_with(&cx, |ws, _| ws.panes[&id].tabs.iter().find(|t| t.path.ends_with("dirty.txt")).map(|t| t.dirty)), Some(true), "the unsaved edit is kept");

    // The worktree is outside the repository, so git has nothing to list there and no .gitignore line is offered.
    let status = std::process::Command::new("git").args(["status", "--porcelain"]).current_dir(&repo_dir).output().unwrap();
    assert!(status.stdout.is_empty(), "the main checkout is unchanged: {}", String::from_utf8_lossy(&status.stdout));
    assert!(ws.read_with(&cx, |ws, _| ws.toast.as_ref().is_none_or(|t| t.action.is_none())), "no toast offers anything");

    // Back to the main checkout, as a session that `claude --worktree` put in the worktree: Claude Code's record of the
    // worktree would win over the folder, so one line is appended.
    std::fs::write(worktree.join("wip.txt"), "x").unwrap();
    std::fs::write(&transcript, "{\"type\":\"user\"}
{\"type\":\"worktree-state\",\"worktreeSession\":{\"worktreeName\":\"fix-login\"},\"sessionId\":\"x\"}
").unwrap();
    wait(&mut cx, "the first screen again", &|ws, app| ws.live.iter().all(|l| !l.view.is_loading(app) && !l.view.is_working(app)));
    step(&mut cx, &|ws, w, cx| ws.act(Act::SwitchWorktree { folder: repo_dir.clone(), base: Default::default() }, w, cx));
    // The work file stays in the worktree the session leaves, so it asks first.
    wait(&mut cx, "the question", &|ws, _| matches!(&ws.modal, Some(Modal::Confirm { body, .. }) if body.contains("1 uncommitted file stays")));
    step(&mut cx, &|ws, w, cx| ws.act(Act::Dismiss, w, cx));
    assert!(ws.read_with(&cx, |ws, app| ws.modal.is_none() && skeleton(ws, app).is_none() && active(ws).cwd == worktree), "cancelling leaves the session where it was, with no skeleton");
    step(&mut cx, &|ws, w, cx| ws.act(Act::SwitchWorktree { folder: repo_dir.clone(), base: Default::default() }, w, cx));
    wait(&mut cx, "the question again", &|ws, _| matches!(&ws.modal, Some(Modal::Confirm { .. })));
    step(&mut cx, &|ws, w, cx| ws.submit_modal(w, cx));
    assert_eq!(ws.read_with(&cx, |ws, app| skeleton(ws, app)).as_deref(), Some("Switching to the main checkout…"));
    wait(&mut cx, "the switch back", &|ws, app| moved_to(&repo_dir, ws, app));
    // The tab that moved into the worktree moves back; the ones that stayed in the main checkout are not under the
    // tree's root any more, so they stay.
    assert_eq!(ws.read_with(&cx, |ws, _| tab_paths(ws)), [in_main.join("same.txt"), in_main.join("dirty.txt"), in_main.join("only_main.txt")], "tabs after the switch back");
    wait(&mut cx, "the file tree at the main checkout", &|ws, _| ws.panes[&id].root == in_main);
    assert!(!crate::claude::worktree_recorded(&transcript).unwrap(), "the transcript no longer names the worktree");
    assert!(std::fs::read_to_string(&transcript).unwrap().lines().count() == 3);

    step(&mut cx, &|ws, w, cx| ws.restart_session(&session.id, w, cx));
    wait(&mut cx, "the restart", &|ws, app| ws.live_view(&session.id).is_some_and(|v| !v.is_loading(app)));

    // Mid-turn nothing changes: a session that has not shown its first screen is not idle.
    step(&mut cx, &|ws, w, cx| ws.act(Act::SwitchWorktree { folder: worktree.clone(), base: Default::default() }, w, cx));
    // Either the check ran before the screen was ready (told to wait) or after (switched); it never half-switches.
    wait(&mut cx, "a settled state", &|ws, app| ws.is_running(&session.id, app) && ws.live_view(&session.id).is_some_and(|v| !v.is_loading(app)));

    // A project without commits disables "+ New worktree" in the unified Branch & Worktree dialog.
    let bare = dunce::canonicalize(work.path()).unwrap().join("bare");
    git2::Repository::init(&bare).unwrap();
    let bare_project = ws.update(&mut cx, |ws, _| ws.store.add_project(&bare));
    let bare_cwd = bare.clone();
    let origin = crate::panels::widgets::Dropdown::at(gpui::point(gpui::px(0.0), gpui::px(0.0)));
    step(&mut cx, &|ws, w, cx| ws.open_git_picker(bare_project.clone(), bare_cwd.clone(), crate::app::GitPickerTarget::Session, origin, w, cx));
    let reason = ws.read_with(&cx, |ws, _| ws.git_picker().and_then(|p| p.new_worktree_reason.clone()));
    assert!(reason.as_deref().is_some_and(|r| r.contains("no commits")), "{reason:?}");
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
/// (with an agent running in it until the session ends, which on Windows holds the folder until the process is gone).
fn removing_a_worktree_when_its_session_ends() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread reads the environment; this test owns the process.
    unsafe { std::env::set_var("CLAUDIU_DATA_DIR", data.path()) };
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
    ws.update(&mut cx, |ws, _| ws.acp_server = Some(fake_agent(work.path())));
    // The worktree is known to the project once git has been read again.
    step(&mut cx, &|ws, _w, _cx| ws.store.refresh_git());
    // A new session never starts inside a worktree; Fix-with-Claude is the one starter that keeps the folder it is given.
    step(&mut cx, &|ws, w, cx| ws.new_claude_with(folder.clone(), None, vec![], crate::claude::defaults::Starter::FixGit, w, cx));
    let id = ws.read_with(&cx, |ws, _| ws.active.clone().unwrap());

    // Any other way of starting a session, asked to start inside the worktree, starts in the main checkout.
    step(&mut cx, &|ws, w, cx| ws.new_claude(folder.clone(), w, cx));
    let (other, started_in) = ws.read_with(&cx, |ws, _| (ws.active.clone().unwrap(), ws.store.session(ws.active.as_ref().unwrap()).unwrap().cwd.clone()));
    assert_eq!(started_in, repo_dir);
    step(&mut cx, &|ws, w, cx| ws.close_live(&other, w, cx));
    step(&mut cx, &|ws, w, cx| ws.act(Act::Select(id.clone()), w, cx));

    // The unified Branch & Worktree dialog offers removal for the linked worktree and not the main checkout.
    let project = ws.read_with(&cx, |ws, _| ws.store.data.projects[0].id.clone());
    let origin = crate::panels::widgets::Dropdown::at(gpui::point(gpui::px(0.0), gpui::px(0.0)));
    let worktrees_of = |cx: &mut TestAppContext, cwd: PathBuf| {
        step(cx, &|ws, w, cx| ws.open_git_picker(project.clone(), cwd.clone(), crate::app::GitPickerTarget::Session, origin, w, cx));
        ws.read_with(cx, |ws, _| ws.git_picker().map(|p| p.worktrees.iter().map(|w| (w.label.clone(), w.remove.is_some())).collect::<Vec<_>>()).unwrap_or_default())
    };
    for cwd in [folder.clone(), repo_dir.clone()] {
        let rows = worktrees_of(&mut cx, cwd);
        assert!(rows[0].0.contains("main checkout") && !rows[0].1, "the main checkout has no x: {rows:?}");
        assert!(rows[1].0.contains("fix") && rows[1].1, "the worktree has an x: {rows:?}");
        assert!(ws.read_with(&cx, |ws, _| ws.git_picker().is_some_and(|p| p.new_worktree_reason.is_none())), "creating a new worktree is enabled");
    }

    // The x on the worktree a session is running in is refused, naming the session.
    let own_x = ws.read_with(&cx, |ws, _| ws.git_picker().and_then(|p| p.worktrees[1].remove.clone()));
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

    // The same dialog lists branches and puts an x on every local branch except the current one.
    assert!(git(&["branch", "spare"]).status.success());
    step(&mut cx, &|ws, w, cx| ws.open_git_picker(project.clone(), repo_dir.clone(), crate::app::GitPickerTarget::Session, origin, w, cx));
    wait(&mut cx, "the branch list", &|ws| ws.git_picker().is_some_and(|p| p.branches.is_some()));
    let rows = ws.read_with(&cx, |ws, _| {
        ws.git_picker()
            .and_then(|p| p.branches.as_ref())
            .and_then(|r| r.as_ref().ok())
            .map(|list| list.iter().map(|b| (b.name.clone(), b.current, b.remove.clone())).collect::<Vec<_>>())
            .unwrap_or_default()
    });
    let spare = rows.iter().find(|r| r.0 == "spare").and_then(|r| r.2.clone());
    assert!(spare.is_some(), "a local branch has an x: {rows:?}");
    assert!(rows.iter().find(|r| r.1).is_some_and(|r| r.2.is_none()), "the current branch has none: {rows:?}");

    let x = spare.unwrap();
    step(&mut cx, &|ws, w, cx| ws.act(x.clone(), w, cx));
    wait(&mut cx, "the question", &|ws| matches!(&ws.modal, Some(Modal::Confirm { confirm, .. }) if confirm == "Delete"));
    step(&mut cx, &|ws, w, cx| ws.submit_modal(w, cx));
    wait(&mut cx, "the branch to go", &|_| !git(&["rev-parse", "--verify", "-q", "spare"]).status.success());

    // Settings lists the worktrees in Claudiu's data folder. Removing one asks inside the page, so the dialog stays open.
    let listed = crate::git::worktree::folder(&repo_dir, "listed");
    assert!(git(&["worktree", "add", "-q", "-b", "listed", &listed.to_string_lossy()]).status.success());
    step(&mut cx, &|ws, w, cx| {
        ws.act(Act::OpenSettings, w, cx);
        ws.set_settings_page(SettingsPage::Projects, cx);
    });
    let shown = |ws: &Workspace| ws.worktrees_page.stored.as_ref().is_some_and(|s| s.iter().any(|w| w.folder == listed));
    wait(&mut cx, "the page to list it", &shown);
    let main_checkout = repo_dir.clone();
    step(&mut cx, &|ws, w, cx| ws.act(Act::RemoveWorktree { main_checkout: main_checkout.clone(), folder: listed.clone() }, w, cx));
    wait(&mut cx, "the question in the page", &|ws| ws.worktrees_page.asking.as_ref().is_some_and(|a| a.folder == listed));
    assert!(ws.read_with(&cx, |ws, _| matches!(ws.modal, Some(Modal::Settings { .. }))), "the dialog stays open");
    let confirm = ws.read_with(&cx, |ws, _| ws.worktrees_page.asking.as_ref().unwrap().act.clone());
    step(&mut cx, &|ws, w, cx| ws.act(confirm.clone(), w, cx));
    wait(&mut cx, "the folder to go", &|_| !listed.exists());
    wait(&mut cx, "the page to drop it", &|ws| !shown(ws));
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

