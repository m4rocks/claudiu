#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod claude;
mod commit;
mod git;
mod glyphs;
mod keys;
mod mcp;
mod platform;
mod sidebar;
mod store;
mod terminal;
mod terminal_view;
mod theme;
mod statusline;
mod updater;
mod views;
mod widgets;

use gpui::{App, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};

use app::{
    CloseSession, FocusNextPane, FocusSearch, HideApp, HideOthers, ImportProject, NewClaude, NewShell, NextSession, PrevSession, Quit, ShowAllApps,
    SplitPane, ToggleSidebar, Workspace,
};

/// The macOS menu bar. Every item is an existing Claudiu action, so menu, shortcut and sidebar share one code path;
/// shortcuts shown here come from the keymap bound in `main`.
#[cfg(target_os = "macos")]
fn menu_bar() -> Vec<gpui::Menu> {
    use gpui::{Menu, MenuItem, OsAction, SystemMenuType};
    use gpui_kit::component::input;
    vec![
        Menu::new("Claudiu").items([
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Claudiu", HideApp),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAllApps),
            MenuItem::separator(),
            MenuItem::action("Quit Claudiu", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Claude Session", NewClaude),
            MenuItem::action("New Shell", NewShell),
            MenuItem::separator(),
            MenuItem::action("Add Project…", ImportProject),
            MenuItem::separator(),
            MenuItem::action("Close Session", CloseSession),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Find Sessions", FocusSearch),
            MenuItem::separator(),
            MenuItem::action("Next Session", NextSession),
            MenuItem::action("Previous Session", PrevSession),
            MenuItem::separator(),
            MenuItem::action("Split Pane", SplitPane),
            MenuItem::action("Focus Next Pane", FocusNextPane),
        ]),
    ]
}

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

fn main() {
    // Hidden helper mode: Claude Code runs this as its status line (see statusline.rs). Keep it instant.
    if std::env::args().nth(1).as_deref() == Some(statusline::TEE_FLAG) {
        std::process::exit(statusline::run_tee());
    }
    // Hidden helper mode: the MCP server that lets Claude name its own tab (see mcp.rs).
    if std::env::args().nth(1).as_deref() == Some(mcp::FLAG) {
        std::process::exit(mcp::run());
    }
    install_crash_log();
    // Velopack install/update hooks must run before anything else. A no-op for dev builds.
    velopack::VelopackApp::build().run();

    // TERM=xterm-256color / COLORTERM=truecolor for child processes.
    alacritty_terminal::tty::setup_env();
    terminal::prepare_child_env();

    // `claudiu <folder>...` registers folders as projects (also what a shell "open with" would pass).
    let initial: Vec<std::path::PathBuf> = std::env::args().skip(1).map(std::path::PathBuf::from).filter(|p| p.is_dir()).collect();

    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
        // Claudiu-global shortcuts. Deliberately Ctrl+Shift (Cmd+Shift on macOS) so every plain Ctrl/Alt
        // chord, e.g. Alt+P, still reaches Claude Code untouched.
        cx.bind_keys([
            KeyBinding::new("secondary-shift-n", NewClaude, None),
            KeyBinding::new("secondary-shift-t", NewShell, None),
            KeyBinding::new("secondary-shift-w", CloseSession, None),
            KeyBinding::new("secondary-shift-d", SplitPane, None),
            KeyBinding::new("secondary-shift-b", ToggleSidebar, None),
            KeyBinding::new("secondary-shift-f", FocusSearch, None),
            KeyBinding::new("secondary-shift-o", ImportProject, None),
            KeyBinding::new("secondary-shift-]", FocusNextPane, None),
            KeyBinding::new("ctrl-tab", NextSession, None),
            KeyBinding::new("ctrl-shift-tab", PrevSession, None),
        ]);
        #[cfg(target_os = "macos")]
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None), KeyBinding::new("cmd-h", HideApp, None), KeyBinding::new("cmd-alt-h", HideOthers, None)]);

        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        // Opened through gpui_kit so the window gets the component library's Root (text inputs need it).
        let (window, workspace) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(720.0), px(420.0))),
                titlebar: Some(TitlebarOptions { title: Some("Claudiu".into()), ..Default::default() }),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Workspace::new(window, initial, cx)),
        )
        .expect("open window");

        let quit_target = workspace.clone();
        // Closing the window with live sessions asks first instead of silently killing them.
        window
            .update(cx, |_, window, cx| {
                window.on_window_should_close(cx, move |window, cx| workspace.update(cx, |ws, cx| ws.request_quit(window, cx)));
            })
            .ok();

        // Cmd+Q / the Quit menu item go through the same confirmation as closing the window.
        cx.on_action(move |_: &Quit, cx| {
            let quit = window.update(cx, |_, window, cx| quit_target.update(cx, |ws, cx| ws.request_quit(window, cx))).unwrap_or(true);
            if quit {
                cx.quit();
            }
        });
        cx.on_action(|_: &HideApp, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
        #[cfg(target_os = "macos")]
        cx.set_menus(menu_bar());

        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
