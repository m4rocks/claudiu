//! Drives the real workspace through Rune's headless runtime (no window opens) with throwaway data and Claude Code
//! folders, to catch hangs and panics when sessions start, switch and end. Sessions run a stand-in for `claude`
//! that prints a line and waits, so no test talks to a model or reads the user's own Claude Code data. Each test has
//! a state file of its own; the folders around it are the test run's (`datadir::use_throwaway_folders`).
use super::*;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

/// The tests share the run's data folder (the cross-session requests, the list of running sessions), so they take
/// turns.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

struct Harness {
    rt: rune::Runtime<Workspace>,
    /// Where the test's projects and the stand-in `claude` live (canonical, so paths compare equal).
    work: PathBuf,
    /// Claude Code's data folder, where a conversation is looked for.
    home: PathBuf,
    _dirs: [tempfile::TempDir; 2],
    // Last: the runtime and its terminals go before the next test may start.
    _turn: MutexGuard<'static, ()>,
}

/// A stand-in for `claude` that shows a line and stays up whatever it is asked.
fn fake_claude(dir: &Path) -> PathBuf {
    let fake = dir.join(if cfg!(windows) { "claude.cmd" } else { "claude" });
    let script = if cfg!(windows) { "@echo off\r\necho ready\r\nping -n 60 127.0.0.1 >nul\r\n" } else { "#!/bin/sh\necho ready\nsleep 60\n" };
    std::fs::write(&fake, script).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    fake
}

/// A git repository with one commit at `dir`.
fn git_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"], &["commit", "-q", "--allow-empty", "-m", "one"]] {
        assert!(git(dir, args).status.success(), "git {args:?}");
    }
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git").args(args).current_dir(dir).output().unwrap()
}

impl Harness {
    /// The workspace in a 1600×900 window, with the projects `initial` makes in the work folder registered.
    fn new(initial: impl FnOnce(&Path) -> Vec<PathBuf>) -> Self {
        let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let (_, home) = datadir::use_throwaway_folders();
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let work = dunce::canonicalize(dirs[1].path()).unwrap();
        let store = Store::load(dirs[0].path().join("state.json"));
        let mut ws = Workspace::with_store(initial(&work), store);
        ws.claude_exe = Some(fake_claude(&work));
        ws.start();
        let mut rt = rune::Runtime::new(ws, crate::panels::workspace_view::view, (1600.0, 900.0), rune::Env::new(true, 2.0));
        // What `main` hands the app: keys no element took.
        rt.on_key = Some(|ws: &mut Workspace, ev: &rune::KeyEvent, tx: &mut Tx| ws.on_key(ev, tx));
        let mut h = Self { rt, work, home, _dirs: dirs, _turn: turn };
        h.pump();
        h
    }

    fn ws(&self) -> &Workspace {
        &self.rt.state
    }

    /// Run `f` as a click or key would, then draw. Nothing a step starts may block the window.
    fn step(&mut self, f: impl FnOnce(&mut Workspace)) {
        let t = Instant::now();
        self.rt.update(|ws, tx| {
            f(ws);
            ws.after_input(tx);
        });
        let acted = t.elapsed();
        self.pump();
        self.pump();
        assert!(acted < Duration::from_millis(400), "the window was blocked for {acted:?}");
    }

    /// One frame: background answers and terminal output are taken in, then the view is drawn.
    fn pump(&mut self) {
        self.rt.update(|ws, tx| {
            ws.poll(tx);
        });
        self.rt.frame(1.0 / 60.0);
    }

    fn wait(&mut self, what: &str, done: impl Fn(&Workspace) -> bool) {
        let t = Instant::now();
        let mut said = None;
        while !done(self.ws()) {
            said = self.ws().toast.as_ref().map(|t| t.text.clone()).or(said);
            assert!(t.elapsed() < Duration::from_secs(30), "{what} never happened; the last toast said {said:?}");
            std::thread::sleep(Duration::from_millis(50));
            self.pump();
        }
    }

    /// Let springs and fades finish.
    fn settle(&mut self) {
        for _ in 0..90 {
            self.pump();
        }
    }

    fn drawn(&self, key: &str) -> bool {
        self.rt.find(key).is_some()
    }

    fn keys(&self) -> Vec<String> {
        self.rt.elements().iter().filter_map(|e| e.key.clone()).collect()
    }

    fn click(&mut self, key: &str) {
        let (x, y) = self.rt.find(key).unwrap_or_else(|| panic!("{key} is not drawn"));
        self.rt.pointer_move(x, y);
        self.rt.pointer_down(x, y);
        self.rt.pointer_up(x, y);
        self.pump();
    }

    fn press(&mut self, key: rune::Key, mods: impl FnOnce(&mut rune::Mods)) {
        let mut m = rune::Mods::default();
        mods(&mut m);
        self.rt.key(rune::KeyEvent { key, mods: m });
        self.pump();
    }

    /// A session record in the first project, running `spec` instead of `claude`.
    fn start_one(&mut self, title: &str, cwd: &Path, spec: SpawnSpec) -> Id {
        let id = new_id();
        let project_id = self.ws().store.data.projects.first().map(|p| p.id.clone());
        let record = SessionRecord {
            id: id.clone(),
            kind: SessionKind::Claude,
            title: title.into(),
            custom_title: None,
            auto_title: None,
            project_id,
            cwd: cwd.to_path_buf(),
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            // Present, so closing it keeps the history record instead of treating it as an empty new session.
            claude: Some(ClaudeMeta { session_id: id.clone(), external: ExternalState::Present, ..Default::default() }),
        };
        self.step(|ws| ws.start_session(record, spec, true));
        id
    }

    /// The menu's rows: label and whether it has an x.
    fn menu_rows(&self) -> Vec<(String, bool)> {
        self.ws().menu.as_ref().map(|m| m.items.iter().map(|i| (i.label.trim().to_string(), i.remove.is_some())).collect()).unwrap_or_default()
    }
}

fn exit_spec(code: i32, cwd: &Path) -> SpawnSpec {
    let args = if cfg!(windows) { vec!["/c".into(), format!("exit {code}")] } else { vec!["-c".into(), format!("exit {code}")] };
    SpawnSpec { program: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.into(), args, cwd: Some(cwd.to_path_buf()), ..Default::default() }
}

fn idle(ws: &Workspace) -> bool {
    ws.live.iter().all(|l| !l.view.is_loading() && !l.view.is_working())
}

/// The window draws its three columns, and the sidebar, the settings dialog and the shortcuts answer.
#[test]
fn the_workspace_draws_and_its_controls_answer() {
    let mut h = Harness::new(|_| vec![]);
    h.settle();
    for key in ["topbar", "sidebar", "new-claude", "search-box", "settings"] {
        assert!(h.drawn(key), "{key} is not drawn: {:?}", h.keys());
    }

    // The gear opens Settings; Escape closes it.
    h.click("settings");
    assert!(matches!(h.ws().modal, Some(Modal::Settings { .. })), "Settings did not open");
    h.press(rune::Key::Escape, |_| {});
    assert!(h.ws().modal.is_none(), "Escape did not close Settings");

    // The sidebar shortcut hides the sidebar and shows it again.
    let visible = h.ws().store.data.layout.sidebar_visible;
    let secondary = |m: &mut rune::Mods| {
        m.cmd = true;
        m.shift = true;
        if cfg!(target_os = "macos") { m.logo = true } else { m.ctrl = true }
    };
    h.press(rune::Key::Command('b'), secondary);
    assert_eq!(h.ws().store.data.layout.sidebar_visible, !visible, "the shortcut toggles the sidebar");
    h.press(rune::Key::Command('b'), secondary);
    assert_eq!(h.ws().store.data.layout.sidebar_visible, visible);

    // A new session from the button: shown, running, and its row is in the sidebar.
    let cwd = h.work.clone();
    h.step(|ws| ws.new_claude(cwd));
    let id = h.ws().active.clone().expect("a session is shown");
    h.wait("the first screen", idle);
    h.settle();
    assert!(h.keys().iter().any(|k| k.starts_with("row-") && k.ends_with(&id)), "the session's row: {:?}", h.keys());
    assert!(h.ws().is_running(&id));
}

/// Sessions and the shell under them, the sidebars' widths, cards and the GitHub view, with a stand-in `claude`.
#[test]
fn sessions_and_the_shell_panel() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let first = h.ws().active.clone().expect("a session is shown");
    // A filter on a project that no longer exists filters nothing; the list still draws.
    h.step(|ws| ws.act(Act::FilterProject(Some("gone".into()))));
    assert!(h.ws().project_filter().is_none());
    h.step(|ws| ws.act(Act::FilterProject(None)));

    // The shell under a Claude session: starts on first use, survives switching away, goes with its session.
    h.step(|ws| ws.toggle_shell(&first));
    assert!(h.ws().shells.get(&first).is_some_and(|p| p.open), "shell opened");
    h.step(|ws| ws.new_claude(cwd.clone()));
    let second = h.ws().active.clone().unwrap();
    assert_ne!(first, second);
    assert!(h.ws().shells.contains_key(&first) && !h.ws().shells.contains_key(&second), "the shell stays with its own session");
    h.step(|ws| ws.toggle_shell(&first));
    assert!(h.ws().active.as_deref() == Some(first.as_str()) && h.ws().shells[&first].open, "from another session, it shows the session and its shell");
    h.step(|ws| ws.toggle_shell(&first));
    assert!(!h.ws().shells[&first].open, "toggled shut");
    h.step(|ws| ws.toggle_shell(&first));
    h.settle();
    assert!(h.drawn("shell-panel"), "the shell panel draws: {:?}", h.keys());

    // Dragging the divider resizes the panel within bounds.
    h.step(|ws| {
        ws.shell_drag = Some(SideDrag { size: SHELL_HEIGHT });
        ws.drag_shell(-100.0);
        assert_eq!(ws.shells[&first].height, SHELL_HEIGHT + 100.0);
        ws.drag_shell(-5000.0);
        assert_eq!(ws.shells[&first].height, 700.0, "keeps room for the terminal");
        ws.drag_shell(5000.0);
        assert_eq!(ws.shells[&first].height, SHELL_MIN);
        ws.shell_drag = None;
    });
    h.step(|ws| ws.close_live(&first));
    assert!(!h.ws().shells.contains_key(&first), "closing the session drops its shell");

    // Both sidebars can be dragged; the menu and the GitHub view each remember their own width.
    assert_eq!(h.ws().right_width(1600.0), RIGHT_MENU_W, "the menu starts at its default width");
    h.step(|ws| {
        ws.right_drag = Some(SideDrag { size: RIGHT_MENU_W });
        ws.drag_right(-40.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_MENU_W + 40.0);
        ws.drag_right(5000.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_MENU_MIN);
        ws.drag_right(-5000.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_MENU_MAX);
        ws.drag_right(-40.0);
        ws.right_drag = None;
        assert_eq!(ws.left_width(1600.0), SIDEBAR_W);
        ws.left_drag = Some(SideDrag { size: SIDEBAR_W });
        ws.drag_left(50.0);
        assert_eq!(ws.left_width(1600.0), SIDEBAR_W + 50.0);
        ws.drag_left(-5000.0);
        assert_eq!(ws.left_width(1600.0), SIDEBAR_MIN);
        ws.drag_left(5000.0);
        assert_eq!(ws.left_width(1600.0), SIDEBAR_MAX);
        assert_eq!(ws.left_width(700.0), SIDEBAR_MIN, "keeps room for the session in a small window");
        ws.left_drag = None;
    });

    // A card reaches the terminal of its session, and a card for a session that is not running is dropped.
    let shown = h.ws().active.clone().unwrap();
    let stem = datadir::safe_file_stem(&shown);
    let card = |call: &str| crate::visual::Card { call: call.into(), content: "# Hi".into() };
    h.step(|ws| ws.apply_visuals(vec![(stem.clone(), card("c1")), ("no-such-session".into(), card("c2"))]));
    let view = h.ws().live_view(&shown).unwrap();
    assert!(view.has_card("c1") && !view.has_card("c2"));

    // The GitHub view lives in the right sidebar: one per folder, with its own width.
    h.step(|ws| ws.open_github(cwd.clone()));
    assert!(h.ws().gh.len() == 1 && h.ws().right_gh.is_some(), "github shown in the right sidebar");
    assert_eq!(h.ws().right_width(1600.0), RIGHT_GH_W, "GitHub keeps its own width");
    h.settle();
    h.step(|ws| {
        ws.right_drag = Some(SideDrag { size: RIGHT_GH_W });
        ws.drag_right(-100.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_GH_W + 100.0);
        ws.drag_right(5000.0);
        assert_eq!(ws.right_width(1600.0), RIGHT_GH_MIN);
        ws.drag_right(-5000.0);
        assert_eq!(ws.right_width(1600.0), 1040.0, "keeps room for the session");
        ws.right_drag = None;
    });
    h.step(|ws| ws.toggle_right());
    assert!(h.ws().store.data.layout.right_hidden, "hidden");
    h.step(|ws| ws.toggle_right());
    assert!(!h.ws().store.data.layout.right_hidden, "shown again");
    h.step(|ws| ws.open_github(cwd.clone()));
    assert!(h.ws().gh.len() == 1, "one github pane per folder");

    // A running Claude session in the folder takes the request itself, so close the leftovers to see a new one start.
    let leftovers: Vec<Id> = h.ws().live.iter().map(|l| l.id.clone()).collect();
    for id in leftovers {
        h.step(|ws| ws.close_live(&id));
    }
    let before = h.ws().live.len();
    let pane = h.ws().gh[0].id.clone();
    h.step(|ws| ws.on_gh_event(&pane, &GhEvent::AskClaude("explain issue 1".into())));
    assert!(h.ws().live.len() == before + 1 && h.ws().active.is_some(), "Ask Claude opens a session");
    assert!(h.ws().right_gh.is_some() && h.ws().gh.len() == 1, "the right sidebar keeps following the same project");
    // A new session in another folder: the open GitHub view moves to that folder.
    let other = h.work.join("other");
    std::fs::create_dir_all(&other).unwrap();
    h.step(|ws| ws.new_claude(other.clone()));
    let follows = h.ws().right_gh.as_ref().and_then(|id| h.ws().gh_pane(id)).is_some_and(|p| platform::path_key(&p.cwd) == platform::path_key(&other));
    assert!(h.ws().gh.len() == 2 && follows, "GitHub follows a new session");
    h.settle();
}

/// Cross-session tools are answered on the spot, and one that can't work says why.
#[test]
fn other_sessions_are_listed_started_and_read() {
    use serde_json::{Value, json};
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().expect("a session is shown");
    let ask = |from: String, tool: &'static str, args: Value| std::thread::spawn(move || crate::claude::bridge::ask(&from, tool, &args));
    let take = || loop {
        if let Some(r) = crate::claude::bridge::take_requests().pop() {
            break r;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let from = format!("code:{id}");

    let asked = ask(from.clone(), "list_code_sessions", json!({}));
    let req = take();
    h.step(|ws| ws.serve_requests(vec![req]));
    let text = asked.join().unwrap().unwrap();
    assert!(text.contains(&id) && text.contains("this session"), "{text}");

    // A request whose asker gave up is dropped.
    let stale = crate::claude::bridge::Request { id: "late".into(), from: from.clone(), tool: "start_code_session".into(), args: json!({"folder": cwd.to_string_lossy()}), deadline: now() - 1 };
    let before = h.ws().live.len();
    h.step(|ws| ws.serve_requests(vec![stale]));
    assert_eq!(h.ws().live.len(), before, "an expired request was carried out");

    let asked = ask(from.clone(), "start_code_session", json!({"folder": "no-such-project"}));
    let req = take();
    h.step(|ws| ws.serve_requests(vec![req]));
    assert!(asked.join().unwrap().unwrap_err().contains("No project or folder"));

    // A first message of its own needs the user's yes: a closed dialog is a refusal, and nothing was started.
    let asked = ask(from.clone(), "start_code_session", json!({"folder": cwd.to_string_lossy(), "prompt": "do the thing"}));
    let req = take();
    h.step(|ws| ws.serve_requests(vec![req]));
    assert!(matches!(h.ws().modal, Some(Modal::Confirm { act: Act::ConfirmStartSession(_), .. })), "the user was not asked");
    h.settle();
    assert!(h.drawn("modal-card"), "the question is drawn: {:?}", h.keys());
    h.step(|ws| ws.act(Act::Dismiss));
    h.step(|ws| ws.serve_requests(vec![]));
    assert_eq!(asked.join().unwrap().unwrap_err(), "The user declined.");
    assert_eq!(h.ws().live.len(), before, "a declined request started a session");

    let asked = ask(from.clone(), "start_code_session", json!({"folder": cwd.to_string_lossy()}));
    let req = take();
    h.step(|ws| ws.serve_requests(vec![req]));
    let text = asked.join().unwrap().unwrap();
    assert_eq!(h.ws().live.len(), before + 1);
    let code_id = text.split("(id ").nth(1).and_then(|t| t.split(')').next()).unwrap().to_string();
    // The stand-in prints one line at the top of a screen taller than 40 rows.
    h.wait("output to read", |ws| ws.live_view(&code_id).is_some_and(|v| !v.tail_text(500).is_empty()));
    let asked = ask(from.clone(), "read_code_session", json!({"id": code_id}));
    let req = take();
    h.step(|ws| ws.serve_requests(vec![req]));
    assert!(asked.join().unwrap().unwrap().contains("latest terminal output"));
    h.step(|ws| ws.close_live(&id));
    // Closing goes to the home screen; the ended session is picked from the sidebar.
    h.step(|ws| ws.act(Act::Select(id.clone())));
}

/// With no `claude` found, starting a session says so and offers the setup window; asking for it twice opens one.
#[test]
fn a_missing_claude_offers_the_setup_window() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    h.step(|ws| {
        ws.claude_exe = None;
        ws.new_claude(cwd);
    });
    let action = h.ws().toast.as_ref().and_then(|t| t.action.as_ref().map(|(label, act)| (label.clone(), matches!(act, Act::OpenSetup))));
    assert_eq!(action, Some(("Check setup".to_string(), true)), "the toast offers the setup window");
    h.settle();
    assert!(h.keys().iter().any(|k| k.starts_with("toast-")), "the toast is drawn: {:?}", h.keys());
    for _ in 0..2 {
        h.step(|ws| ws.act(Act::OpenSetup));
    }
    assert!(h.ws().setup_window.is_some() && h.ws().setup_serial == 1, "one setup window");
}

/// The setup window's Done reaches the workspace: the `claude` it found is used and saved, and the window is gone.
#[test]
fn the_setup_windows_answer_reaches_the_workspace() {
    let mut h = Harness::new(|_| vec![]);
    h.step(|ws| ws.claude_exe = None);
    let fake = fake_claude(&h.work);
    let hand_back = super::setup::to_workspace(h.ws().rt.mailbox(), super::setup::Mode::Reopened);
    hand_back(super::setup::SetupEvent::Done { claude_path: Some(fake.clone()), recommended_trims: false });
    h.wait("the answer to arrive", |ws| ws.claude_exe.is_some());
    assert_eq!(h.ws().claude_exe.as_deref(), Some(fake.as_path()));
    assert_eq!(h.ws().store.data.settings.claude_path.as_deref(), Some(fake.as_path()), "the path is saved");
    assert!(h.ws().setup_window.is_none());
}

/// A process that ends by itself closes the session the way the X does. A clean exit says nothing, a non-zero
/// exit is a toast, and the worktree stays: Claude Code already asked on `/exit`, and the X is the path that offers.
#[test]
fn a_process_that_ends_closes_the_session_and_leaves_its_worktree() {
    let mut h = Harness::new(|work| {
        git_repo(&work.join("proj"));
        vec![work.join("proj")]
    });
    let repo_dir = h.work.join("proj");
    let folder = crate::git::worktree::folder(&repo_dir, "fix");
    let added = git(&repo_dir, &["worktree", "add", "-q", "-b", "worktree-fix", &folder.to_string_lossy()]);
    assert!(added.status.success(), "{}", String::from_utf8_lossy(&added.stderr));
    // Pump until the view is gone. A late removal dialog is spawned off the UI thread, so keep going after the close.
    let until_closed = |h: &mut Harness, id: &str| {
        h.wait("the session to close after its process ended", |ws| ws.live_view(id).is_none());
        let rest = Instant::now();
        while rest.elapsed() < Duration::from_secs(1) {
            h.pump();
            std::thread::sleep(Duration::from_millis(50));
            assert!(h.ws().modal.is_none(), "a process that ended by itself offered to remove the worktree");
        }
    };

    // Session-only shortcuts are inert on Home, even with a project available as a fallback folder.
    h.step(|ws| {
        ws.main = Main::Home;
        ws.active = None;
        ws.toast = None;
    });
    h.step(|ws| ws.command(Command::OpenGithub));
    assert!(h.ws().gh.is_empty() && h.ws().right_gh.is_none() && h.ws().toast.is_none(), "disabled shortcuts must not open a view or show a toast");

    let clean = h.start_one("Fix login", &folder, exit_spec(0, &folder));
    until_closed(&mut h, &clean);
    let (code, toast) = (h.ws().store.session(&clean).map(|s| s.exit_code), h.ws().toast.as_ref().map(|t| t.text.clone()));
    assert_eq!(code, Some(Some(0)), "a clean exit is kept on the history record");
    assert_eq!(toast, None, "a clean exit says nothing");
    assert!(folder.exists(), "the worktree stays");

    let bad = h.start_one("Broken", &folder, exit_spec(1, &folder));
    until_closed(&mut h, &bad);
    let (code, toast) = (h.ws().store.session(&bad).map(|s| s.exit_code), h.ws().toast.as_ref().map(|t| t.text.clone()));
    assert_eq!(code, Some(Some(1)));
    assert_eq!(toast.as_deref(), Some("Broken ended (exit code 1)"));

    let spec = SpawnSpec { program: "definitely-not-a-real-program-claudiu".into(), args: vec![], cwd: Some(folder.clone()), ..Default::default() };
    let missing = h.start_one("Missing", &folder, spec);
    until_closed(&mut h, &missing);
    let toast = h.ws().toast.as_ref().map(|t| t.text.clone());
    assert!(toast.as_deref().is_some_and(|t| t.starts_with("Could not start Missing:")), "{toast:?}");
}

/// The worktree dialog and the switch of a session between worktrees. What Claude Code does in the new folder is not
/// checked: the stand-in only prints a line.
#[test]
fn switching_between_worktrees() {
    let mut h = Harness::new(|work| {
        git_repo(&work.join("proj"));
        vec![work.join("proj")]
    });
    let repo_dir = h.work.join("proj");
    let active = |ws: &Workspace| ws.store.session(ws.active.as_ref().unwrap()).unwrap().clone();

    // A new session starts in the main checkout.
    h.step(|ws| ws.act(Act::NewClaude { cwd: repo_dir.clone() }));
    assert!(h.ws().modal.is_none() && h.ws().live.len() == 1);
    let session = active(h.ws());
    assert_eq!(session.cwd, repo_dir);
    // The conversation exists once Claude Code has had a prompt; the test stands one in, in Claude Code's own place.
    let claude_id = session.claude_id().unwrap().to_string();
    let transcript = h.home.join("projects/p").join(format!("{claude_id}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();
    h.wait("the first screen", idle);

    // A shell is open under the session at a height of the user's choosing.
    let id = session.id.clone();
    h.step(|ws| {
        ws.toggle_shell(&id);
        ws.shells.get_mut(&id).unwrap().height = 333.0;
    });
    h.settle();
    let grid = h.ws().live_view(&id).unwrap().terminal.size();
    assert_ne!(grid, (120, 32), "the window must not happen to match the default size");

    // A bad name keeps the dialog open; a typed one moves the session into the worktree Claude Code will make.
    h.step(|ws| ws.act(Act::NewWorktree(repo_dir.clone())));
    h.settle();
    assert!(h.drawn("modal-card"), "the dialog draws: {:?}", h.keys());
    h.step(|ws| {
        modal_field(&mut ws.modal, 0).unwrap().replace_text("bad name");
        ws.submit_modal();
    });
    assert!(matches!(h.ws().modal, Some(Modal::NewWorktree { .. })), "a bad name is refused");
    h.step(|ws| {
        modal_field(&mut ws.modal, 0).unwrap().replace_text("fix-login");
        ws.act(Act::PickBase(crate::git::worktree::Base::Current));
        ws.submit_modal();
    });
    let worktree = crate::git::worktree::folder(&repo_dir, "fix-login");
    let skeleton = |ws: &Workspace| ws.live_view(&id).and_then(|v| v.handover.as_ref().map(|h| h.message.clone()));
    assert_eq!(skeleton(h.ws()).as_deref(), Some("Switching to fix-login…"), "the skeleton names the worktree");
    // Until the new process is up, the session stays shown and running behind its skeleton, never an empty pane, and
    // the skeleton sits on the pane's grid from the first frame on.
    let moved_to = |folder: &Path, ws: &Workspace| {
        let loading = ws.live_view(&id).is_some_and(|v| v.is_loading());
        assert_eq!(ws.live_view(&id).map(|v| v.terminal.size()), Some(grid), "mid-switch the skeleton is on another grid");
        assert!(ws.active.as_ref() == Some(&id) && ws.is_running(&id), "mid-switch the session is not shown running");
        let moved = ws.store.session(&id).is_some_and(|r| r.cwd == folder);
        assert!(loading || moved, "mid-switch the pane has no skeleton");
        moved && !loading
    };
    h.wait("the switch", |ws| moved_to(&worktree, ws));
    let branch = git(&worktree, &["branch", "--show-current"]);
    assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "fix-login", "the branch is named as typed");
    assert_eq!(h.ws().live.len(), 1, "the same session, not a new one");
    assert_eq!(active(h.ws()).id, id);
    assert_eq!(active(h.ws()).claude_id().map(str::to_string), Some(claude_id.clone()), "the same conversation");
    assert_eq!(std::fs::read_to_string(&transcript).unwrap().lines().count(), 1, "making a worktree writes nothing to the conversation");
    let shell = h.ws().shells.get(&id).map(|p| (p.open, p.height));
    assert_eq!(shell, Some((true, 333.0)), "the shell is open and as tall as before");

    // The worktree is outside the repository, so git has nothing to list there and no .gitignore line is offered.
    let status = git(&repo_dir, &["status", "--porcelain"]);
    assert!(status.stdout.is_empty(), "the main checkout is unchanged: {}", String::from_utf8_lossy(&status.stdout));
    assert!(h.ws().toast.as_ref().is_none_or(|t| t.action.is_none()), "no toast offers anything");

    // Back to the main checkout, as a session that `claude --worktree` put in the worktree: Claude Code's record of the
    // worktree would win over the folder, so one line is appended.
    std::fs::write(worktree.join("wip.txt"), "x").unwrap();
    std::fs::write(&transcript, "{\"type\":\"user\"}\n{\"type\":\"worktree-state\",\"worktreeSession\":{\"worktreeName\":\"fix-login\"},\"sessionId\":\"x\"}\n").unwrap();
    h.wait("the first screen again", idle);
    h.step(|ws| ws.act(Act::SwitchWorktree { folder: repo_dir.clone(), base: Default::default() }));
    // The work file stays in the worktree the session leaves, so it asks first.
    h.wait("the question", |ws| matches!(&ws.modal, Some(Modal::Confirm { body, .. }) if body.contains("1 uncommitted file stays")));
    h.step(|ws| ws.act(Act::Dismiss));
    assert!(h.ws().modal.is_none() && skeleton(h.ws()).is_none() && active(h.ws()).cwd == worktree, "cancelling leaves the session where it was, with no skeleton");
    h.step(|ws| ws.act(Act::SwitchWorktree { folder: repo_dir.clone(), base: Default::default() }));
    h.wait("the question again", |ws| matches!(&ws.modal, Some(Modal::Confirm { .. })));
    h.step(|ws| ws.submit_modal());
    assert_eq!(skeleton(h.ws()).as_deref(), Some("Switching to the main checkout…"));
    h.wait("the switch back", |ws| moved_to(&repo_dir, ws));
    assert!(!crate::claude::worktree_recorded(&transcript).unwrap(), "the transcript no longer names the worktree");
    assert_eq!(std::fs::read_to_string(&transcript).unwrap().lines().count(), 3);

    // A restart (a switch, a hand-off) puts the new terminal on the pane's grid before its first frame.
    h.step(|ws| {
        ws.restart_session(&id);
        assert_eq!(ws.live_view(&id).map(|v| v.terminal.size()), Some(grid), "the new terminal starts on another grid");
    });
    h.wait("the restart", |ws| ws.live_view(&id).is_some_and(|v| !v.is_loading()));

    // Mid-turn nothing changes: either the check ran before the screen was ready (told to wait) or after (switched);
    // it never half-switches.
    h.step(|ws| ws.act(Act::SwitchWorktree { folder: worktree.clone(), base: Default::default() }));
    h.wait("a settled state", |ws| ws.is_running(&id) && ws.live_view(&id).is_some_and(|v| !v.is_loading()));

    // A project without commits has a disabled "New worktree…".
    let bare = h.work.join("bare");
    git2::Repository::init(&bare).unwrap();
    let mut bare_project = String::new();
    h.step(|ws| bare_project = ws.store.add_project(&bare));
    h.step(|ws| ws.open_worktree_menu(bare_project.clone(), bare.clone(), MenuAnchor::Point(100.0, 30.0)));
    let last = h.ws().menu.as_ref().and_then(|m| m.items.last().map(|i| (i.label.clone(), i.disabled, i.hint.clone())));
    assert!(matches!(&last, Some((label, true, Some(hint))) if label == "New worktree…" && hint.contains("no commits")), "{last:?}");
    h.settle();
    assert!(h.keys().iter().any(|k| k.starts_with("menu-")), "the menu draws: {:?}", h.keys());
}

/// A running session refuses the removal of its worktree. Ending it offers the removal, and confirming removes the
/// folder. Branches and the Settings page's worktrees are removed the same way.
#[test]
fn removing_a_worktree() {
    let mut h = Harness::new(|work| {
        git_repo(&work.join("proj"));
        vec![work.join("proj")]
    });
    let repo_dir = h.work.join("proj");
    let folder = crate::git::worktree::folder(&repo_dir, "fix");
    let added = git(&repo_dir, &["worktree", "add", "-q", "-b", "worktree-fix", &folder.to_string_lossy()]);
    assert!(added.status.success(), "{}", String::from_utf8_lossy(&added.stderr));
    // The worktree is known to the project once git has been read again.
    h.step(|ws| ws.store.refresh_git());
    // A new session never starts inside a worktree; Fix-with-Claude is the one starter that keeps the folder it is given.
    h.step(|ws| ws.new_claude_with(folder.clone(), vec![], Starter::FixGit));
    let id = h.ws().active.clone().unwrap();
    // Any other way of starting a session, asked to start inside the worktree, starts in the main checkout.
    h.step(|ws| ws.new_claude(folder.clone()));
    let other = h.ws().active.clone().unwrap();
    assert_eq!(h.ws().store.session(&other).unwrap().cwd, repo_dir);
    h.step(|ws| ws.close_live(&other));
    h.step(|ws| ws.act(Act::Select(id.clone())));

    // The top-bar menu offers the removal for the session's own worktree, and not in the main checkout.
    let project = h.ws().store.data.projects[0].id.clone();
    for cwd in [folder.clone(), repo_dir.clone()] {
        h.step(|ws| ws.open_worktree_menu(project.clone(), cwd, MenuAnchor::Point(100.0, 30.0)));
        let rows = h.menu_rows();
        assert_eq!(rows[0].0, "Worktrees");
        assert!(rows[1].0.contains("main checkout") && !rows[1].1, "the main checkout has no x: {rows:?}");
        assert!(rows[2].0.contains("fix") && rows[2].1, "the worktree has an x: {rows:?}");
        assert!(rows.last().is_some_and(|r| r.0 == "New worktree…" && !r.1), "{rows:?}");
    }

    // The x on the worktree a session is running in is refused, naming the session.
    let x = h.ws().menu.as_ref().and_then(|m| m.items[2].remove.clone()).expect("an x on the worktree's row");
    h.step(|ws| ws.act(x));
    let toast = h.ws().toast.as_ref().map(|t| t.text.clone());
    assert!(toast.as_deref().is_some_and(|t| t.contains("is in fix. Switch it to main first.")), "{toast:?}");
    assert!(folder.exists() && h.ws().modal.is_none() && h.ws().live.len() == 1, "nothing changes");

    // Ending the session by itself offers the removal.
    h.step(|ws| ws.close_live(&id));
    h.wait("the offer", |ws| matches!(&ws.modal, Some(Modal::Confirm { act: Act::ConfirmRemoveWorktree { .. }, .. })));
    h.step(|ws| ws.submit_modal());
    h.wait("the folder to go", |_| !folder.exists());
    let toast = h.ws().toast.as_ref().map(|t| t.text.clone());
    assert!(toast.as_deref().is_some_and(|t| t.contains("Removed worktree fix")), "{toast:?}");
    assert!(!git(&repo_dir, &["rev-parse", "--verify", "-q", "worktree-fix"]).status.success(), "the merged branch went with it");

    // The branch menu has a title and an x on every local branch but the current one.
    assert!(git(&repo_dir, &["branch", "spare"]).status.success());
    h.step(|ws| ws.open_branch_menu(repo_dir.clone(), MenuAnchor::Point(100.0, 30.0)));
    h.wait("the branch menu", |ws| ws.menu.is_some());
    let rows: Vec<(String, Option<Act>)> = h.ws().menu.as_ref().map(|m| m.items.iter().map(|i| (i.label.trim().to_string(), i.remove.clone())).collect()).unwrap_or_default();
    assert_eq!(rows[0].0, "Branches");
    let spare = rows.iter().find(|r| r.0 == "spare").and_then(|r| r.1.clone()).expect("a local branch has an x");
    assert!(rows.iter().find(|r| r.0.starts_with('✓')).is_some_and(|r| r.1.is_none()), "the current branch has none");
    h.step(|ws| ws.act(spare));
    h.wait("the question", |ws| matches!(&ws.modal, Some(Modal::Confirm { confirm, .. }) if confirm == "Delete"));
    h.step(|ws| ws.submit_modal());
    h.wait("the branch to go", |_| !git(&repo_dir, &["rev-parse", "--verify", "-q", "spare"]).status.success());

    // Settings lists the worktrees in Claudiu's data folder. Removing one asks inside the page, so the dialog stays open.
    let listed = crate::git::worktree::folder(&repo_dir, "listed");
    assert!(git(&repo_dir, &["worktree", "add", "-q", "-b", "listed", &listed.to_string_lossy()]).status.success());
    h.step(|ws| {
        ws.act(Act::OpenSettings);
        ws.set_settings_page(SettingsPage::Worktrees);
    });
    let shown = |ws: &Workspace| ws.worktrees_page.stored.as_ref().is_some_and(|s| s.iter().any(|w| w.folder == listed));
    h.wait("the page to list it", shown);
    h.settle();
    let main_checkout = repo_dir.clone();
    h.step(|ws| ws.act(Act::RemoveWorktree { main_checkout, folder: listed.clone() }));
    h.wait("the question in the page", |ws| ws.worktrees_page.asking.as_ref().is_some_and(|a| a.folder == listed));
    assert!(matches!(h.ws().modal, Some(Modal::Settings { .. })), "the dialog stays open");
    let confirm = h.ws().worktrees_page.asking.as_ref().unwrap().act.clone();
    h.step(|ws| ws.act(confirm));
    h.wait("the folder to go", |_| !listed.exists());
    h.wait("the page to drop it", |ws| !shown(ws));
}

/// What the user types and pastes reaches the shown session's program, and a click on the terminal takes the
/// keyboard back from a text field.
#[test]
fn typing_and_pasting_reach_the_session() {
    let mut h = Harness::new(|_| vec![]);
    // A plain terminal session (no start-up skeleton) running `cat`, which echoes what it is sent.
    let cwd = h.work.clone();
    let id = new_id();
    let record = SessionRecord {
        id: id.clone(),
        kind: SessionKind::Shell,
        title: "cat".into(),
        custom_title: None,
        auto_title: None,
        project_id: None,
        cwd: cwd.clone(),
        created_at: now(),
        last_active: now(),
        exit_code: None,
        hidden: false,
        claude: None,
    };
    let program = if cfg!(windows) { "more" } else { "/bin/cat" };
    h.step(|ws| ws.start_session(record, SpawnSpec { program: program.into(), cwd: Some(cwd), ..Default::default() }, true));
    h.settle();
    assert!(h.ws().terminal_focused(&id), "a new session has the keyboard");
    let shows = |h: &mut Harness, text: &str, what: &str| {
        h.wait(what, |ws| ws.live_view(&id).is_some_and(|v| v.tail_text(500).contains(text)));
    };

    for c in "hello".chars() {
        h.press(rune::Key::Text(c.to_string()), |_| {});
    }
    h.press(rune::Key::Enter, |_| {});
    shows(&mut h, "hello", "the typed line to reach the program");

    h.rt.paste("pasted-text");
    h.pump();
    h.press(rune::Key::Enter, |_| {});
    shows(&mut h, "pasted-text", "the pasted text to reach the program");

    // Settings takes the keyboard; once it is closed, a click on the terminal gives it back.
    h.click("settings");
    assert!(!h.ws().terminal_focused(&id));
    h.press(rune::Key::Escape, |_| {});
    h.click(&id);
    assert!(h.ws().terminal_focused(&id), "the click gave the terminal the keyboard");
    for c in "again".chars() {
        h.press(rune::Key::Text(c.to_string()), |_| {});
    }
    h.press(rune::Key::Enter, |_| {});
    shows(&mut h, "again", "typing after the click to reach the program");
}

/// A working session's icon turns only while someone can see it: the window is in front and the sidebar shown.
/// Otherwise the window stops asking for frames.
#[test]
fn busy_icons_stop_asking_for_frames_when_nobody_can_see_them() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    h.step(|ws| ws.new_claude(cwd));
    let id = h.ws().active.clone().unwrap();
    h.wait("the first screen", idle);
    let animating = |h: &mut Harness, active: bool, sidebar: bool| {
        h.step(|ws| {
            ws.activity.insert(id.clone(), Activity::Working);
            ws.window_active = active;
            ws.store.data.layout.sidebar_visible = sidebar;
        });
        // Five seconds of frames: anything else that moved (a meter filling as usage arrives) has settled by then.
        (0..300).all(|_| h.rt.frame(1.0 / 60.0))
    };
    assert!(animating(&mut h, true, true), "the icon turns in a window in front");
    assert!(!animating(&mut h, false, true), "a window in the background asks for no frames");
    assert!(!animating(&mut h, true, false), "nor does a hidden sidebar");
}

/// Quit (⌘Q, or closing the window) asks first while a Claude session is mid-turn, and quits at once otherwise.
#[test]
#[cfg(unix)] // The stand-in that keeps printing is a `sh` loop.
fn quitting_asks_first_only_while_a_session_is_working() {
    let mut h = Harness::new(|_| vec![]);
    let quit_now = |h: &mut Harness| {
        h.step(|ws| ws.modal = None);
        h.rt.update(|ws, tx| ws.quit_or_ask(tx));
        h.pump();
        h.ws().quitting
    };
    // A session that streams output for as long as it runs, as Claude Code does mid-turn.
    let cwd = h.work.clone();
    let script = "while :; do echo working; sleep 0.1; done";
    let spec = SpawnSpec { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()], cwd: Some(cwd.clone()), ..Default::default() };
    let id = h.start_one("Busy", &cwd, spec);
    h.wait("the session to be working", |ws| ws.live_view(&id).is_some_and(|v| v.is_working()));
    assert!(!quit_now(&mut h), "a working session holds the quit");
    assert!(matches!(h.ws().modal, Some(Modal::Confirm { act: Act::ConfirmQuit, .. })), "and the user is asked");
    h.settle();
    assert!(h.drawn("modal-card"), "the question is drawn");
    h.step(|ws| ws.act(Act::Dismiss));
    assert!(!h.ws().quitting && h.ws().is_running(&id), "Cancel keeps everything running");

    h.step(|ws| ws.close_live(&id));
    assert!(quit_now(&mut h), "with nothing working it quits at once");
    assert!(h.ws().modal.is_none());
}

/// Hundreds of sessions: the list scrolls under the wheel and keeps drawing rows where it brings them.
#[test]
fn the_session_list_draws_rows_where_scrolling_brings_them() {
    let mut h = Harness::new(|_| vec![]);
    h.step(|ws| {
        for i in 0..300 {
            ws.store.data.sessions.push(SessionRecord {
                id: format!("s{i:03}"),
                kind: SessionKind::Claude,
                title: format!("Session {i}"),
                custom_title: None,
                auto_title: None,
                cwd: PathBuf::from("/nowhere"),
                project_id: None,
                created_at: 0,
                // Newer than anything else, so these come first, s000 at the top.
                last_active: 4_000_000_000 - i,
                exit_code: None,
                hidden: false,
                claude: None,
            });
        }
        ws.store.data.layout.older_open = true;
    });
    h.settle();
    let row_y = |h: &Harness, n: usize| h.keys().into_iter().find(|k| k.starts_with("row-") && k.ends_with(&format!("s{n:03}"))).and_then(|k| h.rt.find(&k)).map(|p| p.1);
    let top = row_y(&h, 0).expect("the first row draws");
    let (x, y) = h.rt.find("sidebar-scroll").expect("the list draws");
    h.rt.pointer_move(x, y);
    h.rt.wheel(x, y, -1200.0);
    h.settle();
    let moved = row_y(&h, 0);
    assert!(moved.is_none_or(|m| m < top - 600.0), "the list did not scroll: the first row went from {top} to {moved:?}");
    let shown = (1..300).filter(|n| row_y(&h, *n).is_some_and(|y| (0.0..900.0).contains(&y))).count();
    assert!(shown > 10, "only {shown} rows are drawn where the list scrolled to");
}

/// Cmd on macOS, Ctrl elsewhere: the modifier of the app's shortcuts.
fn secondary(m: &mut rune::Mods) {
    m.cmd = true;
    if cfg!(target_os = "macos") { m.logo = true } else { m.ctrl = true }
}

impl Harness {
    fn editor_pane(&self, id: &str) -> &EditorPane {
        self.ws().panes.get(id).expect("the session has an editor pane")
    }

    fn shown_tab(&self, id: &str) -> Option<&editor_mode::Tab> {
        self.ws().panes.get(id).and_then(|p| p.shown())
    }

    fn shown_text(&self, id: &str) -> String {
        self.shown_tab(id).map(|t| t.editor.buffer.text()).unwrap_or_default()
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.press(rune::Key::Text(c.to_string()), |_| {});
        }
    }
}

/// Editor mode: the shown session keeps its terminal; its folder's files are listed beside it and open in tabs; edits
/// save; a change on disk reloads a clean tab and never overwrites unsaved edits; and nothing unsaved is dropped
/// without asking.
#[test]
fn editor_mode_lists_opens_edits_and_saves_files() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.join("project");
    std::fs::create_dir_all(cwd.join("src")).unwrap();
    std::fs::write(cwd.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(cwd.join("src/code.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().expect("a session is shown");
    h.wait("the first screen", idle);
    let sidebars = (h.ws().store.data.layout.sidebar_visible, h.ws().store.data.layout.right_hidden);

    h.press(rune::Key::Command('e'), |m| {
        secondary(m);
        m.shift = true;
    });
    assert!(h.ws().editing(), "Shift+E turns editor mode on");
    h.wait("listing the folder", |ws| ws.panes.get(&id).is_some_and(|p| p.rows.iter().any(|r| r.name == "notes.md")));
    h.settle();
    assert!(h.drawn("editor-pane") && h.drawn("file-tree"), "the pane is drawn: {:?}", h.keys());
    assert!(h.ws().is_running(&id) && h.drawn(&format!("pane-{id}")), "the terminal keeps running beside it");
    let names: Vec<String> = h.editor_pane(&id).rows.iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, ["src", "notes.md"], "folders first, closed until opened");

    // Folders open in place; a file opens in a tab with the keyboard.
    let root = h.editor_pane(&id).root.clone();
    h.click(&format!("tree-{}", platform::path_key(&root.join("src"))));
    let names: Vec<String> = h.editor_pane(&id).rows.iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, ["src", "code.rs", "notes.md"]);
    h.settle();
    h.click(&format!("tree-{}", platform::path_key(&root.join("notes.md"))));
    h.wait("opening a file", |ws| ws.panes[&id].shown().is_some());
    h.settle();
    assert_eq!(h.shown_text(&id), "# Notes\n");
    let key = h.shown_tab(&id).unwrap().editor.key.clone();
    assert_eq!(h.ws().keys_to.as_deref(), Some(key.as_str()), "the opened file has the keyboard");
    assert!(h.drawn(&format!("{key}/l0")), "its first line is drawn");

    // Typing edits it and marks the tab; the pane's Save writes it.
    h.press(rune::Key::End, |_| {});
    h.type_text(" and more");
    assert_eq!(h.shown_text(&id), "# Notes and more\n");
    assert!(h.shown_tab(&id).unwrap().dirty, "an edit marks the tab");
    h.press(rune::Key::Command('s'), secondary);
    h.wait("saving", |ws| ws.panes[&id].shown().is_some_and(|t| !t.dirty));
    assert_eq!(std::fs::read_to_string(cwd.join("notes.md")).unwrap(), "# Notes and more\n");
    // Undo back to what was loaded is a change from what is saved now.
    h.press(rune::Key::Command('z'), secondary);
    assert_eq!(h.shown_text(&id), "# Notes\n");
    assert!(h.shown_tab(&id).unwrap().dirty);
    h.press(rune::Key::Command('z'), |m| {
        secondary(m);
        m.shift = true;
    });
    assert!(!h.shown_tab(&id).unwrap().dirty, "redo back to the saved text is clean again");

    // A change on disk reloads a clean tab; with unsaved edits it becomes a conflict that overwrites neither side.
    let code = root.join("src/code.rs");
    h.step(|ws| ws.open_file(&id, code.clone(), Some(1)));
    h.wait("opening the second file", |ws| ws.panes[&id].shown().is_some_and(|t| t.path == code));
    assert_eq!(h.shown_tab(&id).unwrap().editor.caret().line, 1, "it opens at the line asked for");
    std::fs::write(&code, "fn by_claude() {}\n// longer than before\n").unwrap();
    h.wait("reloading a clean tab", |ws| ws.panes[&id].shown().is_some_and(|t| t.editor.buffer.text().starts_with("fn by_claude")));
    h.type_text("x");
    std::fs::write(&code, "fn by_claude_again() {}\n").unwrap();
    h.wait("the conflict", |ws| ws.panes[&id].shown().is_some_and(|t| t.conflict == Some(Conflict::Changed)));
    h.settle();
    assert!(h.drawn("conflict") && h.drawn("conflict-keep"), "the conflict is shown with its choices");
    assert!(h.shown_text(&id).contains('x'), "unsaved edits stay");
    h.press(rune::Key::Command('s'), secondary);
    h.wait("the refused save", |ws| ws.toast.as_ref().is_some_and(|t| t.text.starts_with("Not saved")));
    assert_eq!(std::fs::read_to_string(&code).unwrap(), "fn by_claude_again() {}\n", "Claude's change is not overwritten");
    h.click("conflict-keep");
    h.press(rune::Key::Command('s'), secondary);
    h.wait("saving over the change", |ws| ws.panes[&id].shown().is_some_and(|t| !t.dirty && t.stamp.is_some()));
    let mine = h.shown_text(&id);
    assert_eq!(std::fs::read_to_string(&code).unwrap(), mine, "keeping mine and saving writes mine");

    // Each file has one tab; unsaved edits are never dropped without asking.
    let tabs = |h: &Harness| h.editor_pane(&id).tabs.iter().map(|t| t.path.file_name().unwrap().to_string_lossy().to_string()).collect::<Vec<_>>();
    assert_eq!(tabs(&h), ["notes.md", "code.rs"]);
    h.step(|ws| ws.open_file(&id, root.join("notes.md"), None));
    assert_eq!((tabs(&h).len(), h.editor_pane(&id).active), (2, Some(0)), "opening an open file shows its tab");
    h.type_text("!");
    h.press(rune::Key::Command('w'), secondary);
    assert!(matches!(h.ws().modal, Some(Modal::Confirm { act: Act::DiscardTab(..), .. })) && tabs(&h).len() == 2, "closing a dirty tab asks");
    h.step(|ws| ws.act(Act::Dismiss));
    assert_eq!(h.ws().keys_to.as_deref(), Some(h.shown_tab(&id).unwrap().editor.key.as_str()), "cancelling gives the editor the keyboard back");
    h.step(|ws| ws.act(Act::CloseLive(id.clone())));
    assert!(matches!(h.ws().modal, Some(Modal::Confirm { act: Act::ConfirmCloseLive(_), .. })) && h.ws().is_running(&id), "ending the session asks");
    h.step(|ws| ws.act(Act::Dismiss));
    h.press(rune::Key::Command('w'), secondary);
    h.step(|ws| ws.submit_modal());
    assert_eq!(tabs(&h), ["code.rs"], "discarding closes the tab");
    assert_eq!(std::fs::read_to_string(cwd.join("notes.md")).unwrap(), "# Notes and more\n", "nothing was saved by asking");

    // Back returns to the place the editor left.
    h.step(|ws| ws.open_file(&id, root.join("notes.md"), None));
    h.wait("reopening", |ws| ws.panes[&id].tabs.len() == 2);
    let back = if cfg!(target_os = "macos") { (rune::Key::Text("-".into()), true) } else { (rune::Key::Left, false) };
    h.press(back.0, |m| if back.1 { m.ctrl = true } else { m.alt = true });
    assert!(h.shown_tab(&id).is_some_and(|t| t.path == code), "Back shows the file before");

    // Leaving editor mode brings the sidebars back as they were; asking for a sidebar in it leaves it.
    h.step(|ws| ws.toggle_editor());
    assert_eq!((h.ws().editing(), (h.ws().store.data.layout.sidebar_visible, h.ws().store.data.layout.right_hidden)), (false, sidebars));
    h.step(|ws| {
        ws.store.data.layout.sidebar_visible = false;
        ws.toggle_editor();
        ws.toggle_left();
    });
    assert!(!h.ws().editing() && h.ws().store.data.layout.sidebar_visible, "the sidebar shows");

    // Ctrl+click on a path in the terminal opens it in editor mode, at its line.
    h.step(|ws| {
        ws.open_in_editor_mode(&id, "src/code.rs", Some(1));
    });
    h.wait("the clicked file", |ws| ws.editing() && ws.panes[&id].shown().is_some_and(|t| t.path == code));

    // The pane goes with its session.
    h.step(|ws| ws.close_live(&id));
    assert!(!h.ws().panes.contains_key(&id), "closing the session drops its pane");
}

/// Quick open finds a file by its letters and opens it; find in files finds a line and opens the file there.
#[test]
fn quick_open_and_find_in_files_open_what_they_find() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.join("project");
    std::fs::create_dir_all(cwd.join("src")).unwrap();
    std::fs::write(cwd.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(cwd.join("src/code.rs"), "fn a() {}\nfn needle() {}\n").unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    h.wait("listing the folder", |ws| ws.panes[&id].files.len() == 2);
    let picker = |h: &Harness| h.editor_pane(&id).picker.as_ref().map(|p| (p.kind, p.hits.iter().map(|hit| (hit.text.clone(), hit.line)).collect::<Vec<_>>()));

    h.press(rune::Key::Command('p'), secondary);
    h.settle();
    assert!(h.drawn("picker") && h.drawn("picker-field"), "quick open is drawn: {:?}", h.keys());
    h.type_text("notmd");
    h.pump();
    assert_eq!(picker(&h), Some((picker::PickerKind::Files, vec![("notes.md".to_string(), 0)])));
    h.press(rune::Key::Enter, |_| {});
    h.wait("opening the picked file", |ws| ws.panes[&id].shown().is_some_and(|t| t.path.ends_with("notes.md")));
    assert!(picker(&h).is_none(), "choosing closes the picker");

    // From the tree: over the editor, the editor's own search keys come first, as in main.
    h.step(|ws| ws.focus_terminal(&editor_mode::tree_key(&id)));
    h.press(rune::Key::Command('f'), |m| {
        secondary(m);
        m.shift = true;
    });
    assert_eq!(picker(&h).map(|p| p.0), Some(picker::PickerKind::Text), "the search shortcut finds in files in editor mode");
    h.type_text("NEEDLE");
    h.wait("find in files", |ws| ws.panes[&id].picker.as_ref().is_some_and(|p| !p.hits.is_empty()));
    assert_eq!(picker(&h).unwrap().1, [("fn needle() {}".to_string(), 1)]);
    h.settle();
    assert!(h.drawn("picked-0"));
    h.press(rune::Key::Enter, |_| {});
    h.wait("opening the found file", |ws| ws.panes[&id].shown().is_some_and(|t| t.path.ends_with("code.rs")));
    assert_eq!(h.shown_tab(&id).unwrap().editor.caret().line, 1, "the caret is on the found line");
    h.wait("colouring the Rust file", |ws| ws.panes[&id].shown().is_some_and(|t| t.editor.spans.len() == 3 && !t.editor.spans[1].is_empty()));
    // Typing a new line moves the colours below it down with their lines until they are worked out again.
    h.press(rune::Key::Home, |_| {});
    h.press(rune::Key::Enter, |_| {});
    let e = &h.shown_tab(&id).unwrap().editor;
    assert!(e.spans.len() == 4 && !e.spans[2].is_empty(), "the colours follow their line: {:?}", e.spans);

    // Escape closes it and gives the editor back the keyboard.
    h.press(rune::Key::Command('p'), secondary);
    h.press(rune::Key::Escape, |_| {});
    assert!(picker(&h).is_none());
    assert_eq!(h.ws().keys_to.as_deref(), Some(h.shown_tab(&id).unwrap().editor.key.as_str()));
}

/// A file at the editor's line limit opens and types without stalling, draws only the lines on screen, and a wheel
/// notch eases the text down and stops at the end.
#[test]
fn a_long_file_opens_draws_only_what_shows_and_scrolls() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    let big = cwd.join("big.rs");
    std::fs::write(&big, "let x = 1; // a line of ordinary code\n".repeat(crate::files::MAX_LINES)).unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    let t = Instant::now();
    h.step(|ws| ws.open_file(&id, big.clone(), None));
    h.wait("opening the long file", |ws| ws.panes[&id].shown().is_some());
    h.settle();
    eprintln!("a {}-line file opened and drew in {:?}", crate::files::MAX_LINES, t.elapsed());
    let key = h.shown_tab(&id).unwrap().editor.key.clone();
    let drawn_lines = || h.keys().iter().filter(|k| k.starts_with(&format!("{key}/l"))).count();
    let lines = drawn_lines();
    assert!((10..120).contains(&lines), "{lines} lines are built for one screen");

    let t = Instant::now();
    for _ in 0..20 {
        h.press(rune::Key::Text("y".into()), |_| {});
    }
    let per_key = t.elapsed() / 20;
    assert!(per_key < Duration::from_millis(100), "a keystroke in a long file took {per_key:?}");
    let t = Instant::now();
    for _ in 0..20 {
        h.press(rune::Key::Down, |m| m.alt = true);
    }
    let per_move = t.elapsed() / 20;
    assert!(per_move < Duration::from_millis(100), "moving a line in a long file took {per_move:?}");
    assert_eq!(h.shown_tab(&id).unwrap().editor.caret().line, 20, "the line moved down twenty times");

    let (x, y) = h.rt.find(&key).expect("the editor is drawn");
    h.rt.pointer_move(x, y);
    let scroll = |h: &Harness| h.shown_tab(&id).unwrap().editor.scroll[1];
    // Mouse wheel notches (not a trackpad's precise steps), down.
    h.rt.wheel_event(rune::WheelEvent::new(0.0, -120.0).at(x, y));
    let mut seen = vec![];
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(8));
        h.pump();
        seen.push(scroll(&h));
    }
    assert!(seen.windows(2).all(|w| w[1] >= w[0]), "the scroll only moves down: {seen:?}");
    assert!(seen[0] < *seen.last().unwrap() && *seen.last().unwrap() > 100.0, "the text eased down: {seen:?}");
    h.press(rune::Key::End, |m| if cfg!(target_os = "macos") { m.logo = true } else { m.ctrl = true });
    h.settle();
    assert_eq!(h.shown_tab(&id).unwrap().editor.caret().line, crate::files::MAX_LINES, "the caret went to the end");
    assert!(h.drawn(&format!("{key}/l{}", crate::files::MAX_LINES - 1)), "the end is on screen");
}

/// A stand-in language server on the other end of two pipes, for the file `main.rs` in `root`: it reports a problem
/// when the file opens, describes `alpha`, says `alpha` is defined on the first line, offers `alpha` as a completion,
/// and keeps the notifications it hears (method and text) in `heard`.
fn fake_language_server(root: &Path, heard: std::sync::Arc<Mutex<Vec<(String, String)>>>) -> (crate::lsp::Server, futures::channel::mpsc::UnboundedReceiver<crate::lsp::Event>) {
    use crate::lsp::rpc;
    use serde_json::{Value, json};
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, mut server_out) = std::io::pipe().unwrap();
    let uri = crate::lsp::uri(&root.join("main.rs")).unwrap().as_str().to_string();
    let range = |line: u32, from: u32, to: u32| json!({"start": {"line": line, "character": from}, "end": {"line": line, "character": to}});
    std::thread::spawn(move || {
        let mut input = std::io::BufReader::new(server_in);
        while let Ok(Some(m)) = rpc::read_message(&mut input) {
            let method = m["method"].as_str().unwrap_or_default().to_string();
            let result = match method.as_str() {
                "initialize" => json!({"capabilities": {}}),
                "textDocument/hover" => json!({"contents": {"kind": "markdown", "value": "```rust\nfn alpha()\n```"}}),
                "textDocument/definition" => json!({"uri": uri, "range": range(0, 3, 8)}),
                "textDocument/completion" => json!([{"label": "alpha", "textEdit": {"range": range(1, 21, 23), "newText": "alpha"}}, {"label": "other"}]),
                "shutdown" | "exit" => {
                    heard.lock().unwrap().push((method.clone(), String::new()));
                    if method == "exit" {
                        return;
                    }
                    Value::Null
                }
                _ => {
                    let text = m["params"]["contentChanges"][0]["text"].as_str().or(m["params"]["textDocument"]["text"].as_str()).unwrap_or_default();
                    heard.lock().unwrap().push((method.clone(), text.to_string()));
                    if method == "textDocument/didOpen" {
                        let d = json!({"uri": uri, "diagnostics": [{"range": range(1, 21, 23), "message": "cannot find `al`", "severity": 1}]});
                        rpc::write_message(&mut server_out, &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": d})).unwrap();
                    }
                    continue;
                }
            };
            rpc::write_message(&mut server_out, &json!({"jsonrpc": "2.0", "id": m["id"], "result": result})).unwrap();
        }
    });
    let (events, incoming) = futures::channel::mpsc::unbounded();
    let server = crate::lsp::Server::connect(client_in, client_out, None::<std::io::Empty>, root, false, events).unwrap();
    (server, incoming)
}

/// A file's language server underlines its problems, describes what the pointer rests on, completes the word at the
/// caret, finds definitions, and hears every edit, save and close.
#[test]
fn a_language_server_checks_describes_completes_and_finds() {
    use crate::editor::buffer::{Pos, Selection};
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    let main = cwd.join("main.rs");
    std::fs::write(&main, "fn alpha() {}\nfn main() { alpha(); al }\n").unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    let heard = std::sync::Arc::new(Mutex::new(vec![]));
    let (server, incoming) = fake_language_server(&cwd, heard.clone());
    h.step(|ws| {
        language_servers::forward_events(ws.rt.mailbox(), id.clone(), "rust-analyzer", incoming);
        ws.on_server_started(&id, crate::lsp::kind_for("rust").unwrap(), Ok(server));
    });
    h.step(|ws| ws.open_file(&id, main.clone(), None));
    h.wait("the server's problem", |ws| ws.panes[&id].shown().is_some_and(|t| !t.editor.problems.is_empty()));
    let problem = &h.shown_tab(&id).unwrap().editor.problems[0];
    assert_eq!((problem.from, problem.to, problem.error), (Pos::new(1, 21), Pos::new(1, 23), true), "the problem underlines `al`");
    let opened: Vec<String> = heard.lock().unwrap().iter().filter(|(m, _)| m == "textDocument/didOpen").map(|(_, t)| t.clone()).collect();
    assert_eq!(opened, ["fn alpha() {}\nfn main() { alpha(); al }\n"], "the server got the file's text once");
    h.settle();
    assert_eq!(h.ws().server_status(&id).as_deref(), Some("rust-analyzer"));
    assert!(h.drawn("server-status"));

    // Resting the pointer on `alpha` shows what the server says about it.
    let key = h.shown_tab(&id).unwrap().editor.key.clone();
    let (editor, _) = h.rt.presented(&key).expect("the editor is drawn");
    let (line, _) = h.rt.presented(&format!("{key}/l1")).expect("the second line is drawn");
    let y = line[1] + line[3] / 2.0;
    let e = &h.shown_tab(&id).unwrap().editor;
    let x = (0..2000).map(|x| x as f32).find(|&x| e.pos_at(x, y - editor[1]) == Pos::new(1, 13)).expect("a point over `alpha`");
    h.rt.pointer_move(editor[0] + x, y);
    h.pump();
    h.wait("the hover card", |ws| ws.panes[&id].shown().and_then(|t| t.editor.hover.as_ref()).is_some_and(|c| c.docs.is_some()));
    let card = h.shown_tab(&id).unwrap().editor.hover.clone().unwrap();
    assert_eq!((card.at, card.docs.as_deref()), (Pos::new(1, 12), Some("```rust\nfn alpha()\n```")));

    // F12 on the call goes to where `alpha` is defined.
    h.step(|ws| ws.panes.get_mut(&id).unwrap().shown_mut().unwrap().editor.select(Selection::caret(Pos::new(1, 14))));
    h.press(rune::Key::Function(12), |_| {});
    h.wait("going to the definition", |ws| ws.panes[&id].shown().is_some_and(|t| t.editor.caret() == Pos::new(0, 3)));

    // Ctrl+Space after `al` lists what the server offers; Enter writes the choice over the word.
    h.step(|ws| ws.panes.get_mut(&id).unwrap().shown_mut().unwrap().editor.select(Selection::caret(Pos::new(1, 23))));
    h.press(rune::Key::Text(" ".into()), |m| m.ctrl = true);
    h.wait("the completion list", |ws| ws.panes[&id].shown().is_some_and(|t| t.editor.completion.is_some()));
    h.settle();
    h.press(rune::Key::Enter, |_| {});
    let edited = "fn alpha() {}\nfn main() { alpha(); alpha }\n";
    assert_eq!(h.shown_text(&id), edited, "the completion replaced `al`");

    // The server hears the edit once typing pauses, then the save and the close.
    let heard_of = |method: &'static str, text: &'static str| {
        let heard = heard.clone();
        move |_: &Workspace| heard.lock().unwrap().iter().any(|(m, t)| m == method && t == text)
    };
    h.wait("the server hearing the edit", heard_of("textDocument/didChange", edited));
    h.press(rune::Key::Command('s'), secondary);
    h.wait("the server hearing the save", heard_of("textDocument/didSave", ""));
    h.step(|ws| ws.close_tab(&id, 0));
    h.wait("the server hearing the close", heard_of("textDocument/didClose", ""));

    // Ending the session ends its server.
    h.step(|ws| ws.close_live(&id));
    h.wait("the server told to exit", heard_of("exit", ""));
}

/// Right-clicking the editor opens its menu; asking Claude about the selected lines sends `@path#L..` and the request
/// to the session's mod (never typed into its terminal) and hands the keyboard to Claude.
#[test]
fn the_editor_menu_asks_claude_about_the_selected_lines() {
    use crate::editor::buffer::{Pos, Selection};
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.join("project");
    std::fs::create_dir_all(cwd.join("src")).unwrap();
    std::fs::write(cwd.join("src/code.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.wait("the session starting", |ws| ws.is_running(&ws.active.clone().unwrap()));
    h.step(|ws| ws.toggle_editor());
    h.wait("listing", |ws| !ws.panes[&id].root.as_os_str().is_empty());
    let root = h.editor_pane(&id).root.clone();
    h.step(|ws| ws.open_file(&id, root.join("src/code.rs"), None));
    h.wait("opening", |ws| ws.panes[&id].shown().is_some());
    h.settle();
    h.step(|ws| ws.panes.get_mut(&id).unwrap().shown_mut().unwrap().editor.select(Selection { anchor: Pos::new(1, 0), head: Pos::new(3, 0) }));

    // A right click inside the selection keeps it.
    let key = h.shown_tab(&id).unwrap().editor.key.clone();
    let (x, y) = h.rt.find(&format!("{key}/l1")).expect("line 2 is drawn");
    h.rt.secondary_down(x, y);
    h.rt.secondary_up(x, y);
    h.settle();
    let rows: Vec<String> = h.menu_rows().into_iter().map(|(label, _)| label).collect();
    assert_eq!(rows[0], "Go to Definition");
    let ask = rows.iter().position(|r| r == "Ask Claude to explain this").expect("the menu asks Claude");
    assert_eq!(h.shown_tab(&id).unwrap().editor.sel, Selection { anchor: Pos::new(1, 0), head: Pos::new(3, 0) }, "the selection stays");

    let serial = h.ws().menu.as_ref().unwrap().serial;
    h.click(&format!("menu-{serial}-{ask}"));
    let sent = std::fs::read_to_string(crate::claude::usage::ctl_file(&id)).expect("a request went to the session's mod");
    assert!(sent.contains("@src/code.rs#L2-3: Explain this code."), "{sent}");
    assert_eq!(h.ws().keys_to.as_deref(), Some(id.as_str()), "Claude has the keyboard");

    // Outside the selection, the click moves the caret there first.
    h.step(|ws| ws.focus_pane());
    let (x, y) = h.rt.find(&format!("{key}/l0")).expect("line 1 is drawn");
    h.rt.secondary_down(x, y);
    h.rt.secondary_up(x, y);
    h.pump();
    let e = &h.shown_tab(&id).unwrap().editor;
    assert!(e.sel.is_empty() && e.caret().line == 0, "the caret went to the clicked line");
}

/// Moving the keyboard from the editor to Claude while lines are selected puts `@path#L..: ` into Claude's input box,
/// and moving it away again without typing erases it. A stand-in for `claude` draws an input box, asks for bracketed
/// paste, and records what reaches it.
#[cfg(unix)]
#[test]
fn selected_lines_are_named_in_claudes_input_until_the_keyboard_leaves() {
    use crate::editor::buffer::{Pos, Selection};
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join("code.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    let heard = h.work.join("heard");
    let prompt = h.work.join("prompt-claude");
    let rule = "─".repeat(30);
    // Bracketed paste on, a rule, the `❯` line, a rule, then the cursor back after `❯ `; raw, so bytes arrive as sent.
    let script = format!("#!/bin/sh\nstty raw -echo\nprintf '\\033[?2004h\\033[2J\\033[H{rule}\\r\\n❯ \\r\\n{rule}\\r\\n\\033[2;3H'\nexec cat > '{}'\n", heard.display());
    std::fs::write(&prompt, script).unwrap();
    std::fs::set_permissions(&prompt, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    h.step(|ws| ws.claude_exe = Some(prompt.clone()));
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.wait("the input box", |ws| ws.live_view(&id).is_some_and(|v| !v.is_loading() && v.input_holds("")));
    h.step(|ws| ws.toggle_editor());
    h.wait("listing", |ws| !ws.panes[&id].root.as_os_str().is_empty());
    let root = h.editor_pane(&id).root.clone();
    h.step(|ws| ws.open_file(&id, root.join("code.rs"), None));
    h.wait("opening", |ws| ws.panes[&id].shown().is_some());
    h.step(|ws| ws.panes.get_mut(&id).unwrap().shown_mut().unwrap().editor.select(Selection { anchor: Pos::new(1, 0), head: Pos::new(3, 0) }));

    let read = || std::fs::read(&heard).unwrap_or_default();
    h.step(|ws| ws.focus_terminal(&id));
    h.wait("the reference reaching Claude", |_| String::from_utf8_lossy(&read()).contains("@code.rs#L2-3: "));
    assert_eq!(read(), b"\x1b[200~@code.rs#L2-3: \x1b[201~", "pasted, so nothing it holds is taken as a key");

    h.step(|ws| ws.focus_pane());
    let reference = "@code.rs#L2-3: ".chars().count();
    h.wait("the reference erased", |_| read().iter().filter(|&&b| b == 0x7f).count() == reference);

    // Typed after, it stays: the user's comment is behind it.
    h.step(|ws| ws.focus_terminal(&id));
    h.press(rune::Key::Text("x".into()), |_| {});
    h.step(|ws| ws.focus_pane());
    h.settle();
    assert_eq!(read().iter().filter(|&&b| b == 0x7f).count(), reference, "nothing more is erased");
}

/// The tree's right-click menu creates, renames and deletes files: a new file opens, a rename carries its open tab
/// along, a delete asks first and closes the tab, and a name that would leave the folder or overwrite a file is refused.
#[test]
fn the_tree_creates_renames_and_deletes_files() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.join("project");
    std::fs::create_dir_all(cwd.join("src")).unwrap();
    std::fs::write(cwd.join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(cwd.join("src/taken.rs"), "").unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    h.wait("listing", |ws| ws.panes[&id].rows.iter().any(|r| r.name == "src"));
    h.settle();
    let root = h.editor_pane(&id).root.clone();
    let src = root.join("src");
    let pick = |h: &mut Harness, label: &str| {
        let rows: Vec<String> = h.menu_rows().into_iter().map(|(l, _)| l).collect();
        let ix = rows.iter().position(|r| r == label).unwrap_or_else(|| panic!("no {label:?} in {rows:?}"));
        let serial = h.ws().menu.as_ref().unwrap().serial;
        h.click(&format!("menu-{serial}-{ix}"));
    };
    let name_it = |h: &mut Harness, name: &str| {
        h.step(|ws| {
            if let Some(f) = modal_field(&mut ws.modal, 0) {
                *f = modal::field(name, true);
            }
        });
        h.step(|ws| ws.submit_modal());
    };

    // New file in a folder, from the folder's row: it is made, the folder opens, and the file opens in a tab.
    let (x, y) = h.rt.find(&format!("tree-{}", platform::path_key(&src))).unwrap();
    h.rt.secondary_down(x, y);
    h.rt.secondary_up(x, y);
    h.settle();
    pick(&mut h, "New file…");
    name_it(&mut h, "b.rs");
    assert!(src.join("b.rs").is_file());
    h.wait("the new file's tab", |ws| ws.panes[&id].shown().is_some_and(|t| t.path == src.join("b.rs")));
    assert!(h.editor_pane(&id).expanded.contains(&src), "its folder is open");

    // Refused names keep the dialog open and touch nothing.
    h.step(|ws| ws.act(Act::AskFileName(id.clone(), NameFor::Rename(src.join("b.rs")))));
    name_it(&mut h, "taken.rs");
    assert!(h.ws().modal.is_some(), "a name in use is refused");
    name_it(&mut h, "../out.rs");
    assert!(h.ws().modal.is_some() && !cwd.join("out.rs").exists(), "a name can't leave the folder");

    // A rename carries the open tab with it.
    name_it(&mut h, "c.rs");
    assert!(h.ws().modal.is_none());
    assert!(src.join("c.rs").is_file() && !src.join("b.rs").exists());
    assert!(h.editor_pane(&id).tabs.iter().any(|t| t.path == src.join("c.rs")), "the tab follows the file");
    assert!(h.editor_pane(&id).tabs.iter().all(|t| t.path != src.join("b.rs")));
    assert!(h.editor_pane(&id).recent.contains(&src.join("c.rs")) && !h.editor_pane(&id).recent.contains(&src.join("b.rs")), "recent files follow the rename");

    // Delete asks first, then removes the file and closes its tab.
    h.wait("the tree listing c.rs", |ws| ws.panes[&id].rows.iter().any(|r| r.name == "c.rs"));
    h.settle();
    let (x, y) = h.rt.find(&format!("tree-{}", platform::path_key(&src.join("c.rs")))).unwrap();
    h.rt.secondary_down(x, y);
    h.rt.secondary_up(x, y);
    h.settle();
    pick(&mut h, "Delete");
    assert!(src.join("c.rs").exists(), "nothing goes before the answer");
    h.step(|ws| ws.submit_modal());
    h.wait("deleting", |ws| !src.join("c.rs").exists() && ws.panes[&id].tabs.iter().all(|t| t.path != src.join("c.rs")));
    let pane = h.editor_pane(&id);
    assert!(pane.recent.iter().chain(pane.back.iter().map(|(p, _)| p)).all(|p| !p.starts_with(src.join("c.rs"))), "no place is left in the deleted file");
}

/// A file git knows gets a bar beside each changed line, drawn only for the lines on screen: scrolled to a later
/// change, its bar shows and the first one's goes.
#[test]
fn git_change_marks_are_drawn_beside_their_lines_and_follow_scrolling() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.join("repo");
    git_repo(&cwd);
    let file = cwd.join("a.txt");
    let base: String = (1..=150).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&file, &base).unwrap();
    assert!(git(&cwd, &["add", "."]).status.success() && git(&cwd, &["commit", "-qm", "base"]).status.success());
    std::fs::write(&file, base.replace("line 2\n", "TWO\n").replace("line 120\n", "CHANGED\n")).unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    h.wait("listing", |ws| !ws.panes[&id].root.as_os_str().is_empty());
    let file = h.editor_pane(&id).root.join("a.txt");
    h.step(|ws| ws.open_file(&id, file.clone(), None));
    h.wait("the marks", |ws| ws.panes[&id].shown().is_some_and(|t| !t.editor.marks.is_empty()));
    h.settle();
    let key = h.shown_tab(&id).unwrap().editor.key.clone();
    assert_eq!(h.shown_tab(&id).unwrap().editor.marks, [(1, crate::editor::Mark::Modified), (119, crate::editor::Mark::Modified)]);
    let bar = |h: &Harness, line: usize| h.drawn(&format!("{key}/mark{line}"));
    assert!(bar(&h, 1) && !bar(&h, 119), "only the change on screen has a bar");
    let (bar_rect, _) = h.rt.presented(&format!("{key}/mark1")).unwrap();
    let (line_rect, _) = h.rt.presented(&format!("{key}/l1")).unwrap();
    assert!((bar_rect[1] - line_rect[1]).abs() < 1.0 && bar_rect[3] > 8.0, "the bar runs beside its line: {bar_rect:?} {line_rect:?}");

    h.step(|ws| ws.open_file(&id, file.clone(), Some(119)));
    h.settle();
    assert!(bar(&h, 119) && !bar(&h, 1), "scrolled down, the later change has the bar");
}

/// The wheel eases the text but never past its ends: up at the top it stays put, and down past the end it stops with
/// the last line still on screen. A double click selects the word under the pointer.
#[test]
fn the_wheel_stops_at_both_ends_and_a_double_click_selects_a_word() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    let file = cwd.join("lines.txt");
    std::fs::write(&file, (0..300).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    h.step(|ws| ws.open_file(&id, file.clone(), None));
    h.wait("opening", |ws| ws.panes[&id].shown().is_some());
    h.settle();
    let key = h.shown_tab(&id).unwrap().editor.key.clone();
    let scroll = |h: &Harness| h.shown_tab(&id).unwrap().editor.scroll[1];
    let (x, y) = h.rt.find(&key).unwrap();
    let wheel = |h: &mut Harness, dy: f32| {
        h.rt.pointer_move(x, y);
        h.rt.wheel_event(rune::WheelEvent::new(0.0, dy).at(x, y));
        (0..40)
            .map(|_| {
                std::thread::sleep(Duration::from_millis(8));
                h.pump();
                scroll(h)
            })
            .collect::<Vec<f32>>()
    };
    let seen = wheel(&mut h, 360.0);
    assert!(seen.iter().all(|&s| s == 0.0), "up at the top, nothing moves: {seen:?}");

    let (line, _) = h.rt.presented(&format!("{key}/l0")).unwrap();
    // The file ends with a line break, so its last line is the empty line 300, which may scroll up to the top.
    let end = 300.0 * line[3];
    let mut seen = vec![];
    for _ in 0..8 {
        seen.extend(wheel(&mut h, -3000.0));
    }
    assert!(seen.iter().all(|&s| s <= end + 0.5), "never past the end ({end}): {seen:?}");
    assert!((seen.last().unwrap() - end).abs() < 0.5, "it reaches the end: {:?}", seen.last());
    h.settle();
    assert!(h.drawn(&format!("{key}/l300")), "the last line stays on screen");

    // Back at the top, a double click on "line" of the first line selects that word.
    h.press(rune::Key::Home, |m| if cfg!(target_os = "macos") { m.logo = true } else { m.ctrl = true });
    h.settle();
    let (row, _) = h.rt.presented(&format!("{key}/l0")).unwrap();
    let (editor, _) = h.rt.presented(&key).unwrap();
    let e = &h.shown_tab(&id).unwrap().editor;
    let y = row[1] + row[3] / 2.0;
    let x = (0..2000).map(|x| x as f32).find(|&x| e.pos_at(x, y - editor[1]) == crate::editor::buffer::Pos::new(0, 2)).unwrap() + editor[0];
    for _ in 0..2 {
        h.rt.pointer_move(x, y);
        h.rt.pointer_down(x, y);
        h.rt.pointer_up(x, y);
    }
    h.pump();
    let e = &h.shown_tab(&id).unwrap().editor;
    assert_eq!(e.buffer.slice(e.sel.range().0, e.sel.range().1), "line", "the word under the pointer");
}

/// As in main's editor, a line wider than the editor wraps onto more rows, and Down moves to its next row. A block's
/// chevron shows while the pointer is over the gutter; clicking it hides the block's inner lines and clicking again
/// shows them.
#[test]
fn a_long_line_wraps_and_a_block_folds_from_its_chevron() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    let file = cwd.join("wrap.rs");
    let long = format!("// {}", "word ".repeat(120));
    std::fs::write(&file, format!("{long}\nfn main() {{\n    let a = 1;\n    let b = 2;\n}}\nfn after() {{}}\n")).unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    h.step(|ws| ws.open_file(&id, file.clone(), None));
    h.wait("the parser finding `main`'s block", |ws| ws.panes[&id].shown().is_some_and(|t| t.editor.fold_spot(1) == Some((1, 4))));
    h.settle();
    let key = h.shown_tab(&id).unwrap().editor.key.clone();
    let (editor, _) = h.rt.presented(&key).unwrap();
    let (first, _) = h.rt.presented(&format!("{key}/l0")).unwrap();
    let (second, _) = h.rt.presented(&format!("{key}/l1")).unwrap();
    let line_h = second[3];
    assert!(first[3] >= line_h * 3.0, "the long line takes several rows: {} for rows of {line_h}", first[3]);
    assert!(first[0] + first[2] <= editor[0] + editor[2] + 0.5, "and fits the editor's width");
    assert!((second[1] - (first[1] + first[3])).abs() < 0.5, "the next line starts below its last row");

    h.step(|ws| ws.focus_pane());
    h.press(rune::Key::Down, |_| {});
    let caret = h.shown_tab(&id).unwrap().editor.caret();
    assert_eq!(caret.line, 0, "Down goes to the long line's second row");
    assert!(caret.col > 0);

    // Over the gutter the chevron shows; a click folds the block, leaving its first and last lines.
    let (gx, gy) = (editor[0] + 4.0, second[1] + line_h / 2.0);
    h.rt.pointer_move(gx, gy);
    h.pump();
    let fold = format!("{key}/fold1");
    let (x, y) = h.rt.find(&fold).expect("the chevron of `main`'s block");
    h.rt.pointer_move(x, y);
    h.rt.pointer_down(x, y);
    h.rt.pointer_up(x, y);
    h.settle();
    assert!(h.shown_tab(&id).unwrap().editor.is_folded(1));
    assert!(!h.drawn(&format!("{key}/l2")) && !h.drawn(&format!("{key}/l3")), "the block's inner lines are hidden");
    let (closing, _) = h.rt.presented(&format!("{key}/l4")).unwrap();
    assert!((closing[1] - (second[1] + line_h)).abs() < 0.5, "its last line comes right after its first");
    assert_eq!(h.shown_tab(&id).unwrap().editor.caret().line, 0, "the click selected nothing");

    h.rt.pointer_down(x, y);
    h.rt.pointer_up(x, y);
    h.settle();
    assert!(!h.shown_tab(&id).unwrap().editor.is_folded(1));
    assert!(h.drawn(&format!("{key}/l2")), "a second click shows them again");
}

/// The caret blinks while the editor has the keyboard, as in main's editor: on while it moves, then off and on every
/// half second. Once the keyboard is elsewhere it stays on and nothing wakes the app for it.
#[test]
fn the_caret_blinks_only_while_the_editor_has_the_keyboard() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    let file = cwd.join("blink.txt");
    std::fs::write(&file, "one two\n").unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    h.step(|ws| ws.open_file(&id, file.clone(), None));
    h.wait("opening", |ws| ws.panes[&id].shown().is_some());
    h.step(|ws| ws.focus_pane());
    h.press(rune::Key::Right, |_| {});
    let shown = |h: &Harness| h.shown_tab(&id).unwrap().editor.caret_shown();
    assert!(shown(&h), "on right after a move");
    h.wait("the caret blinking off", |ws| ws.panes[&id].shown().is_some_and(|t| !t.editor.caret_shown()));
    h.wait("and on again", |ws| ws.panes[&id].shown().is_some_and(|t| t.editor.caret_shown()));
    h.wait("and off", |ws| ws.panes[&id].shown().is_some_and(|t| !t.editor.caret_shown()));
    h.press(rune::Key::Right, |_| {});
    assert!(shown(&h), "a move shows it at once");

    let e = &mut h.rt.state.panes.get_mut(&id).unwrap().shown_mut().unwrap().editor;
    let later = std::time::Instant::now() + Duration::from_secs(5);
    assert_eq!(e.blink(later, false), None, "without the keyboard nothing is scheduled");
    assert!(e.caret_shown());
}

/// Primary+F opens the search bar over the file: matches are counted and highlighted without moving the caret, Enter
/// and Shift+Enter step through them, case can matter, the replace row replaces one or all as one undo step, and
/// Escape gives the keyboard back to the editor.
#[test]
fn the_search_bar_finds_steps_and_replaces_in_the_file() {
    use crate::editor::buffer::Pos;
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    let file = cwd.join("words.txt");
    std::fs::write(&file, "alpha beta alpha\nALPHA gamma\n").unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    h.step(|ws| ws.open_file(&id, file.clone(), None));
    h.wait("opening", |ws| ws.panes[&id].shown().is_some());
    h.settle();
    let key = h.shown_tab(&id).unwrap().editor.key.clone();
    let bar = |h: &Harness| {
        let t = h.shown_tab(&id).unwrap();
        (t.find.open, t.find.label(), t.editor.found.len(), t.editor.found_current)
    };

    h.press(rune::Key::Command('f'), secondary);
    h.settle();
    assert!(h.drawn("find-bar") && h.drawn("find-query"), "the bar is drawn: {:?}", h.keys());
    h.type_text("alpha");
    h.pump();
    assert_eq!(bar(&h), (true, "1/3".into(), 3, Some(0)), "case is ignored by default");
    assert_eq!(h.shown_tab(&id).unwrap().editor.caret(), Pos::new(0, 0), "the caret stays");
    assert_eq!(h.shown_text(&id), "alpha beta alpha\nALPHA gamma\n");
    h.press(rune::Key::Enter, |_| {});
    assert_eq!(bar(&h).1, "2/3");
    h.press(rune::Key::Enter, |m| m.shift = true);
    h.press(rune::Key::Enter, |m| m.shift = true);
    assert_eq!(bar(&h).1, "3/3", "back past the first wraps to the last");
    h.click("find-case");
    h.pump();
    assert_eq!(bar(&h).1.split('/').nth(1), Some("2"), "matching case leaves the lowercase two");

    // The replace row: one, then all, and one undo takes all back.
    h.click("find-case");
    h.click("find-replace-toggle");
    h.settle();
    assert!(h.shown_tab(&id).unwrap().find.replacement.focused, "the replacement has the keyboard");
    h.type_text("omega");
    h.pump();
    h.click("find-replace-one");
    h.pump();
    assert_eq!(h.shown_text(&id), "omega beta alpha\nALPHA gamma\n");
    assert_eq!(bar(&h).1, "1/2", "the next match becomes current");
    h.click("find-replace-all");
    h.pump();
    assert_eq!(h.shown_text(&id), "omega beta omega\nomega gamma\n");
    assert_eq!(bar(&h).1, "0/0");
    h.step(|ws| ws.close_find());
    h.press(rune::Key::Command('z'), secondary);
    assert_eq!(h.shown_text(&id), "omega beta alpha\nALPHA gamma\n", "replace all is one step");
    assert_eq!(h.shown_tab(&id).unwrap().editor.found.len(), 0, "a closed bar highlights nothing");
    assert_eq!(h.ws().keys_to.as_deref(), Some(key.as_str()), "the editor has the keyboard again");
}

/// The real servers on PATH: gleam's reports a type error, and one that isn't installed says so. Starts programs from
/// the machine, so it runs on request: `cargo test -- --ignored real_language_servers`.
#[test]
#[ignore]
fn real_language_servers_start_and_report() {
    let mut h = Harness::new(|_| vec![]);
    let cwd = h.work.clone();
    std::fs::create_dir_all(cwd.join("src")).unwrap();
    std::fs::write(cwd.join("gleam.toml"), "name = \"probe\"\nversion = \"1.0.0\"\n").unwrap();
    std::fs::write(cwd.join("src/probe.gleam"), "pub fn main() -> Int {\n  \"not a number\"\n}\n").unwrap();
    std::fs::write(cwd.join("probe.ts"), "export const n: number = \"x\";\n").unwrap();
    h.step(|ws| ws.new_claude(cwd.clone()));
    let id = h.ws().active.clone().unwrap();
    h.step(|ws| ws.toggle_editor());
    let settled = |ws: &Workspace| ws.server_status(&ws.active.clone().unwrap()).is_some_and(|s| !s.contains("starting"));
    for (file, program) in [("src/probe.gleam", "gleam"), ("probe.ts", "tsc")] {
        h.step(|ws| ws.open_file(&id, cwd.join(file), None));
        h.wait(&format!("{program} starting"), settled);
        let status = h.ws().server_status(&id).unwrap();
        eprintln!("{program}: {status}");
        if crate::platform::find_executable(program).is_none() {
            assert_eq!(status, format!("{program} isn't installed, so this file has no diagnostics or hover."));
            continue;
        }
        assert_eq!(status, program);
        h.wait(&format!("{program}'s type error"), |ws| ws.panes[&id].shown().is_some_and(|t| t.editor.problems.iter().any(|p| p.error)));
        eprintln!("{program} says: {:?}", h.shown_tab(&id).unwrap().editor.problems[0].message);
    }
}

mod github {
    use super::*;
    use crate::github::cli::*;
    use crate::github::model::*;
    use crate::github::view::*;
    use serde_json::json;

    impl Harness {
        /// The GitHub pane opened on `cwd`, by id.
        fn open_github_pane(&mut self, cwd: &Path) -> String {
            let cwd = cwd.to_path_buf();
            self.step(|ws| ws.open_github(cwd));
            self.ws().right_gh.clone().expect("the GitHub pane is shown")
        }

        fn pane(&self, id: &str) -> &GithubView {
            &self.ws().gh_pane(id).expect("the pane").view
        }

        fn with_pane(&mut self, id: &str, f: impl FnOnce(&mut GithubView)) {
            let id = id.to_string();
            self.step(move |ws| f(ws.gh_view_mut(&id).expect("the pane")));
        }
    }

    #[test]
    fn opening_a_pull_request_selects_the_pull_requests_tab() {
        let mut h = Harness::new(|_| vec![]);
        let work = h.work.clone();
        let pane = h.open_github_pane(&work);
        for (kind, number) in [(Kind::Pull, 7), (Kind::Issue, 3)] {
            h.with_pane(&pane, |v| v.open_item(kind, number));
            assert_eq!(h.pane(&pane).kind, kind);
        }
    }

    /// Draws the pane in every state, at two widths, with real GitHub data, to catch panics in `github/ui`. Only
    /// reads from GitHub. Needs a signed-in `gh`: `cargo test -- --ignored pane_draws_every_state`.
    #[test]
    #[ignore]
    fn pane_draws_every_state() {
        let gh = Gh::new(locate().expect("gh is installed"), std::env::current_dir().unwrap());
        let (owner, name) = ("zed-industries", "zed");
        let info = parse_repo(&gh.graphql(REPO_QUERY, json!({"owner": owner, "name": name})).unwrap()).unwrap();
        let mut lists = vec![];
        for kind in [Kind::Issue, Kind::Pull] {
            let (q, qa, qb) = (scoped("zed-industries/zed", kind, "is:open"), scoped("zed-industries/zed", kind, "is:open"), scoped("zed-industries/zed", kind, "is:closed"));
            lists.push(parse_page(&gh.graphql(LIST_QUERY, json!({"q": q, "qa": qa, "qb": qb, "after": null})).unwrap()));
        }
        let detail = |n: u32| parse_detail(&gh.graphql(detail_query_text(), json!({"owner": owner, "name": name, "number": n, "after": null})).unwrap()).unwrap();
        let (issue, pull) = (detail(lists[0].items[0].number), detail(lists[1].items[0].number));
        let repo = RepoRef { owner: owner.into(), name: name.into(), url: String::new(), host: None };
        let files = parse_files(&pull_files(&gh, &repo, pull.item.number, 1).unwrap());

        let mut h = Harness::new(|_| vec![]);
        h.rt.set_viewport((2000.0, 1100.0), 2.0);
        let work = h.work.clone();
        let pane = h.open_github_pane(&work);
        h.wait("the first look at the folder", |ws| ws.gh_pane(&pane).is_some_and(|p| p.view.phase != Phase::Loading));
        for phase in [Phase::Loading, Phase::NoCli, Phase::SignedOut, Phase::NotGithub, Phase::Failed("boom".into())] {
            h.with_pane(&pane, |v| v.phase = phase);
            h.settle();
        }
        for width in [1100.0, 620.0] {
            h.step(|ws| ws.store.data.layout.right_gh_w = Some(width));
            h.with_pane(&pane, |v| {
                v.phase = Phase::Ready;
                v.info = Some(info.clone());
                v.repo = Some(repo.clone());
                for (i, p) in lists.iter().enumerate() {
                    let l = &mut v.lists[i];
                    (l.items, l.total, l.open, l.closed, l.loaded) = (p.items.clone(), p.total, p.open, p.closed, true);
                }
            });
            h.settle();
            for (kind, d) in [(Kind::Issue, &issue), (Kind::Pull, &pull)] {
                for tab in [DetailTab::Conversation, DetailTab::Commits, DetailTab::Files] {
                    h.with_pane(&pane, |v| {
                        v.kind = kind;
                        let mut s = DetailState::new(kind, d.item.number);
                        (s.data, s.loading, s.tab) = (Some(d.clone()), false, tab);
                        s.files = Some(files.clone());
                        s.expanded = files.iter().map(|f| f.path.clone()).collect();
                        v.detail = Some(s);
                    });
                    h.settle();
                }
            }
            h.with_pane(&pane, |v| v.mode = Mode::New(Kind::Issue));
            h.settle();
            h.with_pane(&pane, |v| v.mode = Mode::New(Kind::Pull));
            h.settle();
            h.with_pane(&pane, |v| {
                v.mode = Mode::Browse;
                v.detail = None;
            });
        }
    }

    /// Every feature of the pane against a real, throwaway private repository on the signed-in account, driven through
    /// the pane's own actions (`GithubView::act`). `cargo test -- --ignored every_feature_against_a_scratch_repo
    /// --nocapture`. It CREATES a private repo (named `claudiu-gh-pane-test-<time>`) and tries to delete it at the end,
    /// which needs the `delete_repo` scope; if that is missing, delete it by hand.
    #[test]
    #[ignore]
    fn every_feature_against_a_scratch_repo() {
        let exe = locate().expect("gh is installed");
        let work = tempfile::tempdir().unwrap();
        let setup = Gh::new(exe.clone(), work.path().to_path_buf());
        let me = setup.rest("GET", "user", None).unwrap()["login"].as_str().unwrap().to_string();
        let name = format!("claudiu-gh-pane-test-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
        let full = format!("{me}/{name}");
        eprintln!("scratch repository: {full}");
        setup.rest("POST", "user/repos", Some(json!({"name": name, "private": true, "auto_init": true, "allow_auto_merge": true}))).unwrap();
        std::thread::sleep(Duration::from_secs(3));

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&exe, work.path(), &setup, &me, &full)));

        match setup.rest("DELETE", &format!("repos/{full}"), None) {
            Ok(_) => eprintln!("scratch repository deleted"),
            Err(e) => eprintln!("could not delete {full} ({}). Delete it by hand: gh repo delete {full} --yes", e.msg),
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    fn cmd(exe: &Path, dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new(exe).args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "gh {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The pane, driven by its actions, with every error toast it shows checked.
    struct Rig {
        h: Harness,
        pane: String,
    }

    impl Rig {
        fn view(&self) -> &GithubView {
            self.h.pane(&self.pane)
        }

        /// Run until every background request has landed.
        fn settle(&mut self) {
            for _ in 0..600 {
                self.h.pump();
                let v = self.view();
                let idle = v.busy.is_none()
                    && v.lists.iter().all(|l| !l.loading)
                    && v.detail.as_ref().is_none_or(|d| !d.loading && !d.files_loading && !d.more_loading);
                if idle {
                    // A few more frames: a finished request may have started the next one.
                    for _ in 0..3 {
                        self.h.pump();
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            panic!("the pane never went idle");
        }

        /// Do `a`, wait, and assert that an error was shown only if `expect_error`.
        fn act(&mut self, step: &str, a: GhAct, expect_error: bool) {
            self.h.step(|ws| ws.toast = None);
            self.h.with_pane(&self.pane.clone(), |v| v.act(a));
            self.settle();
            let error = self.h.ws().toast.as_ref().filter(|t| t.error).map(|t| t.text.clone());
            eprintln!("  {step}{}", error.as_ref().map(|e| format!("  -> {e}")).unwrap_or_default());
            if expect_error {
                assert!(error.is_some(), "{step}: expected an error");
            } else {
                assert!(error.is_none(), "{step}: {error:?}");
            }
        }

        fn text(&mut self, which: fn(&mut GithubView) -> &mut TextEdit, text: &str) {
            let text = text.to_string();
            self.h.with_pane(&self.pane.clone(), move |v| which(v).replace_text(&text));
        }

        /// GitHub's search index lags a few seconds behind changes: refresh until `cond` holds.
        fn until(&mut self, what: &str, cond: impl Fn(&GithubView) -> bool) {
            for _ in 0..15 {
                if cond(self.view()) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(3));
                self.act("refresh (waiting for the search index)", GhAct::Refresh, false);
            }
            panic!("never became true: {what}");
        }

        fn detail(&self) -> Detail {
            self.view().detail.as_ref().and_then(|d| d.data.clone()).expect("a detail is open")
        }
    }

    fn run(exe: &Path, work: &Path, setup: &Gh, me: &str, full: &str) {
        // Branches f1..f5 off main, each adding its own file, so nothing conflicts.
        let main_sha = setup.rest("GET", &format!("repos/{full}/git/ref/heads/main"), None).unwrap()["object"]["sha"].as_str().unwrap().to_string();
        for i in 1..=5 {
            setup.rest("POST", &format!("repos/{full}/git/refs"), Some(json!({"ref": format!("refs/heads/f{i}"), "sha": main_sha}))).unwrap();
            setup
                .rest("PUT", &format!("repos/{full}/contents/f{i}.txt"), Some(json!({"message": format!("add f{i}"), "content": "aGVsbG8K", "branch": format!("f{i}")})))
                .unwrap();
        }
        setup.rest("POST", &format!("repos/{full}/milestones"), Some(json!({"title": "M1"}))).unwrap();
        setup.rest("PATCH", &format!("repos/{full}"), Some(json!({"allow_auto_merge": true}))).unwrap();
        cmd(exe, work, &["repo", "clone", full, "clone"]);
        let dir = dunce::canonicalize(work.join("clone")).unwrap();

        let mut h = Harness::new(|_| vec![]);
        let pane = h.open_github_pane(&dir);
        let mut r = Rig { h, pane };
        r.settle();
        assert_eq!(r.view().phase.clone(), Phase::Ready, "connected");
        assert!(r.view().info.as_ref().is_some_and(|i| i.can_write()), "write access");
        assert_eq!(r.view().info.as_ref().map(|i| i.viewer.clone()).as_deref(), Some(me));
        let at = || "gh-test-anchor".to_string();

        eprintln!("issues");
        r.act("new issue form", GhAct::New(Kind::Issue), false);
        r.text(|v| &mut v.new_title, "Issue A");
        r.text(|v| &mut v.new_body, "body of A");
        r.act("create issue", GhAct::SubmitNew, false);
        let d = r.detail();
        assert_eq!((d.item.title.as_str(), d.body.as_str(), d.item.state), ("Issue A", "body of A", State::Open));
        let issue_id = d.item.id.clone();
        r.text(|v| &mut v.composer, "first comment");
        r.act("comment", GhAct::Comment(None), false);
        let first_comment = |r: &Rig| r.detail().timeline.iter().find_map(|e| if let Entry::Comment(c) = e { Some(c.clone()) } else { None });
        let comment = first_comment(&r).expect("comment shown");
        assert_eq!(comment.body, "first comment");
        r.act("edit comment: start", GhAct::EditStart(Edit::Comment(comment.id.clone())), false);
        r.text(|v| &mut v.edit_body, "edited comment");
        r.act("edit comment: save", GhAct::EditSave, false);
        let edited = first_comment(&r).unwrap();
        assert!(edited.body == "edited comment" && edited.edited);
        r.act("react to comment", GhAct::React(comment.id.clone(), "THUMBS_UP".into(), false), false);
        assert!(first_comment(&r).unwrap().reactions.iter().any(|x| x.content == "THUMBS_UP" && x.mine));
        r.act("react to issue", GhAct::React(issue_id.clone(), "HEART".into(), false), false);
        assert!(r.detail().reactions.iter().any(|x| x.content == "HEART" && x.mine));
        r.act("unreact", GhAct::React(issue_id.clone(), "HEART".into(), true), false);
        assert!(r.detail().reactions.iter().all(|x| x.content != "HEART" || x.count == 0));

        r.act("labels picker", GhAct::Popup(PopKind::Labels, at()), false);
        let label = r.view().options.labels.clone().expect("labels loaded").into_iter().find(|l| l.name == "bug").expect("bug label");
        r.act("add label", GhAct::ToggleLabel(label.clone()), false);
        assert!(r.detail().item.labels.iter().any(|l| l.name == "bug"));
        r.act("remove label", GhAct::ToggleLabel(label.clone()), false);
        assert!(r.detail().item.labels.is_empty());
        r.act("add label again", GhAct::ToggleLabel(label.clone()), false);

        r.act("people picker", GhAct::Popup(PopKind::Assignees, at()), false);
        let me_user = r.view().options.users.clone().expect("users loaded").into_iter().find(|u| u.login == me).expect("me among assignable users");
        r.act("assign me", GhAct::ToggleAssignee(me_user.clone()), false);
        assert!(r.detail().item.assignees.iter().any(|u| u.login == me));
        r.act("unassign", GhAct::ToggleAssignee(me_user.clone()), false);
        assert!(r.detail().item.assignees.is_empty());

        r.act("milestone picker", GhAct::Popup(PopKind::Milestone, at()), false);
        let ms = r.view().options.milestones.clone().expect("milestones loaded").into_iter().next().expect("M1");
        r.act("set milestone", GhAct::SetMilestone(Some(ms.clone())), false);
        assert_eq!(r.detail().item.milestone.as_ref().map(|m| m.1.as_str()), Some("M1"));
        r.act("clear milestone", GhAct::SetMilestone(None), false);
        assert!(r.detail().item.milestone.is_none());

        r.act("edit title: start", GhAct::EditStart(Edit::Title), false);
        r.text(|v| &mut v.edit_title, "Issue A (renamed)");
        r.act("edit title: save", GhAct::EditSave, false);
        assert_eq!(r.detail().item.title, "Issue A (renamed)");
        assert_eq!(r.detail().body, "body of A", "editing the title leaves the body alone");
        r.act("edit body: start", GhAct::EditStart(Edit::Body), false);
        r.text(|v| &mut v.edit_body, "new body");
        r.act("edit body: save", GhAct::EditSave, false);
        assert_eq!((r.detail().body.as_str(), r.detail().item.title.as_str()), ("new body", "Issue A (renamed)"));

        r.act("lock", GhAct::Lock(true), false);
        assert!(r.detail().locked);
        r.act("unlock", GhAct::Lock(false), false);
        assert!(!r.detail().locked);
        r.act("close as not planned", GhAct::CloseItem("NOT_PLANNED"), false);
        assert!(r.detail().item.state == State::Closed && r.detail().item.not_planned);
        r.act("reopen", GhAct::ReopenItem, false);
        assert_eq!(r.detail().item.state, State::Open);
        r.text(|v| &mut v.composer, "closing note");
        r.act("comment and close", GhAct::Comment(Some(true)), false);
        assert_eq!(r.detail().item.state, State::Closed);
        r.act("reopen (2)", GhAct::ReopenItem, false);
        r.act("delete comment", GhAct::DeleteComment(comment.id.clone()), false);
        assert!(r.detail().timeline.iter().all(|e| !matches!(e, Entry::Comment(c) if c.id == comment.id)));

        eprintln!("search and lists");
        r.act("back to list", GhAct::Back, false);
        r.act("search by label", GhAct::SetQuery("label:bug".into()), false);
        assert!(r.view().lists[0].items.iter().any(|i| i.title.starts_with("Issue A")));
        r.act("search no match", GhAct::SetQuery("label:enhancement".into()), false);
        assert!(r.view().lists[0].items.is_empty());
        r.act("text search", GhAct::SetQuery("renamed in:title".into()), false);
        assert_eq!(r.view().lists[0].total, 1);
        r.act("closed tab", GhAct::SetState(Some("closed")), false);
        r.act("reset search", GhAct::SetQuery("is:open".into()), false);

        eprintln!("pull requests");
        r.act("pull requests tab", GhAct::SetKind(Kind::Pull), false);
        let mut numbers = vec![];
        for i in 1..=5 {
            r.act("new pr form", GhAct::New(Kind::Pull), false);
            r.text(|v| &mut v.new_title, &format!("PR {i}"));
            r.text(|v| &mut v.new_body, "pr body");
            r.text(|v| &mut v.new_head, &format!("f{i}"));
            r.text(|v| &mut v.new_base, "main");
            r.act(&format!("create PR {i}"), GhAct::SubmitNew, false);
            let d = r.detail();
            assert!(d.pr.is_some() && d.item.head == format!("f{i}") && d.item.base == "main");
            numbers.push(d.item.number);
        }
        r.until("five open PRs", |v| v.lists[1].open == 5);

        r.act("open PR 1", GhAct::Open(Kind::Pull, numbers[0]), false);
        r.act("files tab", GhAct::Tab(DetailTab::Files), false);
        let files = r.view().detail.as_ref().and_then(|d| d.files.clone()).expect("files loaded");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "f1.txt");
        assert!(files[0].lines.iter().any(|l| l.kind == DiffKind::Add));
        r.act("toggle file", GhAct::ToggleFile("f1.txt".into()), false);
        r.act("commits tab", GhAct::Tab(DetailTab::Commits), false);
        r.act("conversation tab", GhAct::Tab(DetailTab::Conversation), false);
        assert!(r.detail().timeline.iter().any(|e| matches!(e, Entry::Commit(_))));

        r.act("to draft", GhAct::ToDraft, false);
        assert!(r.detail().item.draft);
        r.act("ready for review", GhAct::Ready, false);
        assert!(!r.detail().item.draft);
        r.text(|v| &mut v.composer, "review note");
        r.act("review: comment", GhAct::Review("COMMENT"), false);
        assert!(r.detail().timeline.iter().any(|e| matches!(e, Entry::Review(rv) if rv.state == "COMMENTED")));
        r.act("review: approving your own PR is refused", GhAct::Review("APPROVE"), true);
        r.act("pr label", GhAct::ToggleLabel(label.clone()), false);
        assert!(r.detail().item.labels.iter().any(|l| l.name == "bug"));
        r.act("pr assignee", GhAct::ToggleAssignee(me_user.clone()), false);
        r.act("pr milestone", GhAct::SetMilestone(Some(ms.clone())), false);
        r.act("pr title", GhAct::EditStart(Edit::Title), false);
        r.text(|v| &mut v.edit_title, "PR 1 renamed");
        r.act("pr title save", GhAct::EditSave, false);
        assert_eq!(r.detail().item.title, "PR 1 renamed");
        r.act("close pr", GhAct::CloseItem(""), false);
        assert_eq!(r.detail().item.state, State::Closed);
        r.act("reopen pr", GhAct::ReopenItem, false);
        assert_eq!(r.detail().item.state, State::Open);

        r.act("merge PR 1 (squash)", GhAct::Merge("SQUASH"), false);
        assert_eq!(r.detail().item.state, State::Merged);
        r.act("delete merged branch", GhAct::DeleteBranch, false);
        assert!(setup.rest("GET", &format!("repos/{full}/branches/f1"), None).is_err(), "branch f1 is gone");

        // PR 2 is now behind main.
        r.act("open PR 2", GhAct::Open(Kind::Pull, numbers[1]), false);
        r.act("update branch", GhAct::UpdateBranch, false);
        r.act("merge PR 2 (merge commit)", GhAct::Merge("MERGE"), false);
        assert_eq!(r.detail().item.state, State::Merged);
        r.act("open PR 3", GhAct::Open(Kind::Pull, numbers[2]), false);
        r.act("update branch (3)", GhAct::UpdateBranch, false);
        r.act("merge PR 3 (rebase)", GhAct::Merge("REBASE"), false);
        assert_eq!(r.detail().item.state, State::Merged);

        r.act("open PR 4", GhAct::Open(Kind::Pull, numbers[3]), false);
        // Free private repositories cannot use auto-merge ("not allowed"), and with no required checks it is pointless
        // ("clean status"); either answer proves the call is well formed.
        r.h.step(|ws| ws.toast = None);
        r.h.with_pane(&r.pane.clone(), |v| v.act(GhAct::AutoMerge(true)));
        r.settle();
        let said = r.h.ws().toast.as_ref().map(|t| (t.text.clone(), t.error));
        eprintln!("  auto-merge on: {said:?}");
        assert!(said.as_ref().is_none_or(|t| !t.1 || t.0.contains("clean status") || t.0.contains("Auto merge is not allowed")), "{said:?}");

        let pr4 = r.detail().item.id.clone();
        r.act("react to PR", GhAct::React(pr4, "ROCKET".into(), false), false);
        r.act("lock PR", GhAct::Lock(true), false);
        r.act("unlock PR", GhAct::Lock(false), false);
        r.act("ask claude prompt", GhAct::Claude("review"), false);
        assert!(Gh::new(exe.to_path_buf(), dir.clone()).checkout(numbers[3]).is_ok(), "gh pr checkout");

        eprintln!("merged and open lists");
        r.act("back", GhAct::Back, false);
        r.act("merged PRs", GhAct::SetQuery("is:merged".into()), false);
        r.until("three merged PRs", |v| v.lists[1].items.len() == 3);
        r.act("open PRs", GhAct::SetQuery("is:open".into()), false);
        r.until("two open PRs", |v| v.lists[1].items.len() == 2);
        r.act("refresh", GhAct::Refresh, false);
        r.act("escape", GhAct::Escape, false);
    }
}
