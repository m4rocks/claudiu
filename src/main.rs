#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod claude;
mod datadir;
mod files;
mod git;
mod lsp;
mod github;
mod panels;
mod platform;
mod priority;
mod store;
mod terminal;
mod updater;
mod visual;

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AnyWindowHandle, App, Bounds, Entity, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_kit::component::TitleBar;

// The Lucide icons the app draws (`IconName::` anywhere in `src`) and the title bar's window controls. The test
// below fails when one is used but not listed here, which would otherwise show as a blank spot.
gpui_kit::assets::icon_assets!(
    AppIcons,
    [
        WindowMinimize, WindowMaximize, WindowRestore, WindowClose, Archive, ArchiveRestore, ArrowDownToLine, ArrowLeft, Brackets, Check, ChevronDown, ChevronRight,
        CircleCheck, CircleDot, CircleSlash, CircleX, Clock, Ellipsis, Eye, ExternalLink, File, FileText, Folder, FolderGit2, FolderOpen, GitBranch, GitCommitHorizontal, GitMerge, GitPullRequest,
        GitPullRequestClosed, GitPullRequestDraft, HeartPlus, LoaderCircle, Lock, MessageSquare, FileCode, PanelBottom, PanelLeft, PanelRight, Pencil, RefreshCw, Search,
        Settings, Terminal, X
    ]
);

use app::{
    CloseSession, FocusSearch, HideApp, HideOthers, ImportProject, NewClaude, NextSession, OpenGithub, OpenSettings, PrevSession, Quit, ShowAllApps,
    SwapFocus, ToggleEditor, ToggleRight, ToggleShell, ToggleSidebar, Workspace,
};

/// The macOS menu bar. Every item is an existing Claudiu action, so menu, shortcut and sidebar share one code path;
/// shortcuts shown here come from the keymap bound in `main`.
#[cfg(target_os = "macos")]
fn menu_bar() -> Vec<gpui::Menu> {
    use gpui::{Menu, MenuItem, OsAction, SystemMenuType};
    use gpui_kit::component::input;
    vec![
        Menu::new("Claudiu").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
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
            MenuItem::action("Show or Hide Shell", ToggleShell),
            MenuItem::action("Editor Mode", ToggleEditor),
            MenuItem::action("GitHub Issues & Pull Requests", OpenGithub),
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
            MenuItem::action("Toggle Right Sidebar", ToggleRight),
            MenuItem::action("Find Sessions", FocusSearch),
            MenuItem::separator(),
            MenuItem::action("Next Session", NextSession),
            MenuItem::action("Previous Session", PrevSession),
            MenuItem::separator(),
            MenuItem::action("Switch Focus Between Claude and Shell", SwapFocus),
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

type MainWindow = (AnyWindowHandle, Entity<Workspace>);

/// Open the main window with its workspace; closing it asks first when sessions are still working.
fn open_main_window(initial: Vec<std::path::PathBuf>, cx: &mut App) -> MainWindow {
    let bounds = app::saved_window_bounds(cx).unwrap_or_else(|| WindowBounds::Windowed(Bounds::centered(None, size(px(1280.0), px(800.0)), cx)));
    // AppKit's standard window buttons are 14 px tall; center them in the top bar so the
    // traffic lights line up with its controls (and get equal space above and below).
    let traffic_light_y = (panels::theme::TOPBAR_HEIGHT - 14.0) / 2.0;
    // Opened through gpui_kit so the window gets the component library's Root (text inputs need it).
    let (window, workspace) = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(bounds),
            window_min_size: Some(size(px(720.0), px(420.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("Claudiu".into()),
                traffic_light_position: Some(gpui::point(px(14.0), px(traffic_light_y))),
                ..TitleBar::title_bar_options()
            }),
            // The top bar owns dragging and double-click (see TitleBar::window_options).
            app_owns_titlebar_drag: true,
            ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| Workspace::new(window, initial, cx)),
    )
    .expect("open window");
    let closing = workspace.clone();
    // Closing the window with live sessions asks first instead of silently killing them.
    window.update(cx, |_, window, cx| window.on_window_should_close(cx, move |window, cx| closing.update(cx, |ws, cx| ws.request_quit(window, cx)))).ok();
    (window, workspace)
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

    gpui_kit::application().with_assets(AppIcons).run(move |cx: &mut App| {
        gpui_kit::init(cx);
        app::register_languages();
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
        // Claudiu-global shortcuts. Deliberately Ctrl+Shift (Cmd+Shift on macOS) so every plain Ctrl/Alt
        // chord, e.g. Alt+P, still reaches Claude Code untouched.
        cx.bind_keys([
            KeyBinding::new("secondary-shift-n", NewClaude, None),
            KeyBinding::new("ctrl-`", ToggleShell, None),
            KeyBinding::new("secondary-j", ToggleShell, None),
            KeyBinding::new("secondary-shift-w", CloseSession, None),
            KeyBinding::new("secondary-shift-b", ToggleSidebar, None),
            KeyBinding::new("secondary-shift-r", ToggleRight, None),
            KeyBinding::new("secondary-shift-f", FocusSearch, None),
            KeyBinding::new("secondary-shift-o", ImportProject, None),
            KeyBinding::new("secondary-shift-g", OpenGithub, None),
            KeyBinding::new("secondary-,", OpenSettings, None),
            KeyBinding::new("secondary-shift-]", SwapFocus, None),
            KeyBinding::new("secondary-shift-e", ToggleEditor, None),
            // Only over the editor pane: elsewhere Ctrl+P belongs to Claude Code.
            KeyBinding::new("secondary-p", app::QuickOpen, Some("EditorPane")),
            KeyBinding::new("secondary-s", app::SaveFile, Some("EditorPane")),
            KeyBinding::new("ctrl-g", app::GoToLine, Some("EditorPane")),
            KeyBinding::new("secondary-w", app::CloseTab, Some("EditorPane")),
            // VS Code's keys: Alt+arrows move by word on macOS.
            KeyBinding::new(if cfg!(target_os = "macos") { "ctrl--" } else { "alt-left" }, app::GoBack, Some("EditorPane")),
            KeyBinding::new(if cfg!(target_os = "macos") { "ctrl-shift--" } else { "alt-right" }, app::GoForward, Some("EditorPane")),
            // The editor answers it from its language server; Ctrl+click (Cmd+click) does the same.
            KeyBinding::new("f12", gpui_kit::component::input::GoToDefinition, Some("Input")),
            // Completions for the word before the cursor, from the file's language server.
            KeyBinding::new("ctrl-space", gpui_kit::base::input::TriggerCompletion, Some("Input")),
            KeyBinding::new("ctrl-tab", NextSession, None),
            KeyBinding::new("secondary-1", app::ShowSession(1), None),
            KeyBinding::new("secondary-2", app::ShowSession(2), None),
            KeyBinding::new("secondary-3", app::ShowSession(3), None),
            KeyBinding::new("secondary-4", app::ShowSession(4), None),
            KeyBinding::new("secondary-5", app::ShowSession(5), None),
            KeyBinding::new("secondary-6", app::ShowSession(6), None),
            KeyBinding::new("secondary-7", app::ShowSession(7), None),
            KeyBinding::new("secondary-8", app::ShowSession(8), None),
            KeyBinding::new("secondary-9", app::ShowSession(9), None),
            KeyBinding::new("ctrl-shift-tab", PrevSession, None),
        ]);
        cx.bind_keys(app::line_action_bindings());
        // The text-input library binds these globally (focus traversal, copy, select all, ...). A focused
        // terminal must receive them as plain keys, so unbind them inside the Terminal context.
        cx.bind_keys(
            ["tab", "shift-tab", "ctrl-a", "ctrl-c", "ctrl-x", "ctrl-y", "ctrl-z", "ctrl-v"]
                .map(|k| KeyBinding::new(k, gpui::NoAction, Some("Terminal"))),
        );
        #[cfg(target_os = "macos")]
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None), KeyBinding::new("cmd-h", HideApp, None), KeyBinding::new("cmd-alt-h", HideOthers, None)]);

        // The main window, once there is one. Cmd+Q / the Quit menu item go through the same confirmation as closing it.
        let main: Rc<RefCell<Option<MainWindow>>> = Rc::default();
        let quit_main = main.clone();
        cx.on_action(move |_: &Quit, cx| {
            let open = quit_main.borrow().clone();
            let quit = match open {
                Some((window, workspace)) => window.update(cx, |_, window, cx| workspace.update(cx, |ws, cx| ws.request_quit(window, cx))).unwrap_or(true),
                None => true,
            };
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

        // A first launch (no state file) starts with the setup window alone. Asked before anything loads the store.
        if store::Store::is_first_launch(&store::Store::default_path()) {
            let (setup_window, setup) = gpui_kit::open_window(app::setup_window_options(cx), cx, |window, cx| {
                cx.new(|cx| app::SetupView::new(app::SetupMode::FirstLaunch, None, window, cx))
            })
            .expect("open setup window");
            cx.subscribe(&setup, move |_, ev: &app::SetupEvent, cx| {
                let app::SetupEvent::Done(claude_path) = ev else { return };
                // The main window opens before the setup window closes: closing the last window quits the app.
                let (window, workspace) = open_main_window(initial.clone(), cx);
                workspace.update(cx, |ws, cx| ws.apply_setup(claude_path.clone(), cx));
                *main.borrow_mut() = Some((window, workspace));
                setup_window.update(cx, |_, window, _| window.remove_window()).ok();
            })
            .detach();
        } else {
            *main.borrow_mut() = Some(open_main_window(initial, cx));
        }
        cx.activate(true);
    });
}

#[cfg(test)]
mod icon_tests {
    use gpui::AssetSource;
    use gpui_kit::assets::IconName;

    fn kebab(name: &str) -> String {
        let mut out = String::new();
        let mut prev: Option<char> = None;
        for c in name.chars() {
            if (c.is_ascii_uppercase() && prev.is_some()) || (c.is_ascii_digit() && prev.is_some_and(|p| p.is_ascii_alphabetic())) {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            prev = Some(c);
        }
        out
    }

    fn sources(dir: &std::path::Path, out: &mut String) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                out.push_str(&std::fs::read_to_string(path).unwrap());
            }
        }
    }

    #[test]
    fn every_icon_the_app_uses_is_embedded() {
        let mut code = String::new();
        sources(std::path::Path::new("src"), &mut code);
        let dir = IconName::Search.path().to_string();
        let dir = dir.rsplit_once('/').map_or("", |(d, _)| d).to_string();
        let mut used = 0;
        for (i, _) in code.match_indices("IconName::") {
            let ident: String = code[i + 10..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if ident.is_empty() {
                continue; // this test's own pattern
            }
            let path = format!("{dir}/{}.svg", kebab(&ident));
            assert!(super::AppIcons.load(&path).unwrap().is_some(), "{ident} ({path}) is used but missing from icon_assets!");
            used += 1;
        }
        assert!(used > 20, "found only {used} icon uses; the scan is broken");
    }
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
