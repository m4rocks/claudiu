#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod claude;
mod datadir;
mod editor;
mod files;
mod git;
mod github;
mod panels;
mod platform;
mod priority;
mod store;
mod terminal;
mod updater;
mod visual;

use app::Workspace;

/// Write panics to `crash.log` next to the state file: release builds have no console to show them.
fn install_crash_log() {
    let path = store::Store::default_path().with_file_name("crash.log");
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let report = format!(
            "[{}] Claudiu {} panicked
{info}
{}

",
            store::now(),
            env!("CARGO_PKG_VERSION"),
            std::backtrace::Backtrace::force_capture()
        );
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            use std::io::Write;
            let _ = f.write_all(report.as_bytes());
        }
        default_hook(info);
    }));
}

/// The window's first size: the one saved last time, else 1280×800.
fn first_size() -> ((f32, f32), bool) {
    match app::saved_window_geom() {
        Some(g) if g.w >= 720.0 && g.h >= 420.0 => ((g.w, g.h), g.maximized),
        Some(g) => ((1280.0, 800.0), g.maximized),
        None => ((1280.0, 800.0), false),
    }
}

/// Where the window was left last time, in the desktop's pixels, when a display still shows it there.
fn saved_position(displays: &[rune::Display]) -> Option<(i32, i32)> {
    let g = app::saved_window_geom()?;
    // Saved in points; each display has its own scale.
    let scale = |d: &rune::Display| d.scale.max(1.0) as f32;
    let in_points: Vec<_> = displays.iter().map(|d| {
        let s = scale(d);
        (d.position.0 as f32 / s, d.position.1 as f32 / s, d.size.0 as f32 / s, d.size.1 as f32 / s)
    }).collect();
    let s = scale(&displays[g.display_of(&in_points)?]);
    Some(((g.x * s).round() as i32, (g.y * s).round() as i32))
}

fn main() {
    // Hidden helper mode: the MCP server that lets Claude name its own tab (see claude/mcp.rs).
    if std::env::args().nth(1).as_deref() == Some(claude::mcp::FLAG) {
        std::process::exit(claude::mcp::run());
    }
    install_crash_log();
    #[cfg(windows)]
    priority::restore_own_priority();
    // Velopack install/update hooks must run before anything else. A no-op for dev builds.
    velopack::VelopackApp::build().run();

    // TERM=xterm-256color / COLORTERM=truecolor for child processes.
    alacritty_terminal::tty::setup_env();
    terminal::prepare_child_env();

    // `claudiu <folder>...` registers folders as projects (also what a shell "open with" would pass).
    let initial: Vec<std::path::PathBuf> = std::env::args().skip(1).map(std::path::PathBuf::from).filter(|p| p.is_dir()).collect();
    // A first launch (no state file) starts with the setup window alone. Asked before anything loads the store.
    let first_launch = store::Store::is_first_launch(&store::Store::default_path());
    let workspace = Workspace::new(initial);
    let mailbox = workspace.rt.mailbox();
    let ((w, h), maximized) = first_size();

    let spec = rune::AppSpec {
        min_size: Some((720.0, 420.0)),
        setup: Some(|ws: &mut Workspace, waker: rune::Waker| {
            app::rt::set_waker(waker.clone());
            ws.rt.set_waker(waker);
            ws.start();
        }),
        poll: Some(|ws: &mut Workspace, _t: f64, tx: &mut rune::Tx| ws.poll(tx)),
        on_key: Some(|ws: &mut Workspace, ev: &rune::KeyEvent, tx: &mut rune::Tx| ws.on_key(ev, tx)),
        families: (platform::ui_font(), platform::monospace_font()),
        ..rune::AppSpec::new("Claudiu", (w, h), || unreachable!("the workspace is built before the app"), panels::workspace_view::view)
    };
    rune::set_app_id(platform::APP_ID);
    let mut app = rune::App::new(spec)
        .init(move || workspace)
        .titlebar(rune::Titlebar::new(panels::theme::TOPBAR_HEIGHT).traffic_lights(14.0, (panels::theme::TOPBAR_HEIGHT - rune::Chrome::TRAFFIC_LIGHT) / 2.0))
        .keep_running()
        .standard_menus()
        .maximized(maximized)
        .on_ime(|ws: &mut Workspace, ime: &rune::Ime, tx: &mut rune::Tx| ws.on_ime(ime, tx))
        .on_modifiers(|ws: &mut Workspace, mods: rune::Mods, tx: &mut rune::Tx| ws.on_modifiers(mods, tx))
        .on_focus(|ws: &mut Workspace, active: bool, tx: &mut rune::Tx| ws.on_window_focus(active, tx))
        // Closing the window with sessions still working asks first; otherwise Claudiu ends with its window.
        .on_close(|ws: &mut Workspace, tx: &mut rune::Tx| ws.quit_or_ask(tx))
        // Whatever ends the app (Quit in the Dock, logout), the open sessions are remembered and the state saved.
        .on_quit(|ws: &mut Workspace, _tx: &mut rune::Tx| {
            if !ws.quitting {
                ws.remember_window();
                ws.finalize();
            }
        })
        .on_ready(|handle: &rune::WindowHandle| app::set_main_window(handle));
    for item in app::commands::menu_bar() {
        app = app.menu_item(item);
    }
    if first_launch {
        app::open_first_setup(&mut app, mailbox);
    }
    // A factory, because only it is told the displays, which the saved position is checked against.
    rune::run_windows(move |displays| {
        // On a first launch the main window waits, hidden, for the setup window's Done.
        let options = rune::WindowOptions { visible: !first_launch, position: saved_position(displays), ..Default::default() };
        vec![rune::WindowSpec::from_app("main", app.window_options(options))]
    });
}

#[cfg(test)]
mod source_tests {
    /// A NUL byte in a source file makes git treat it as binary, hiding it from diffs and blame.
    #[test]
    fn no_source_file_holds_a_nul_byte() {
        fn walk(dir: &std::path::Path, bad: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, bad);
                } else if path.extension().is_some_and(|e| e == "rs") && std::fs::read(&path).unwrap().contains(&0) {
                    bad.push(path);
                }
            }
        }
        let mut bad = vec![];
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut bad);
        assert!(bad.is_empty(), "NUL byte in {bad:?}; write '\0' instead");
    }
}
