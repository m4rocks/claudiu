//! The setup window: a short checklist of the two tools Claudiu runs (`claude`, required; `gh`, optional). It opens
//! alone on a first launch (see `main.rs`) and again from Settings or the missing-`claude` toast. It installs nothing.
//! It never owns the store: it hands the path the user typed back in `SetupEvent::Done`, and the main window applies it.

use super::*;

use gpui::{
    Bounds, ClickEvent, EventEmitter, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, TitlebarOptions,
    WindowBounds, WindowOptions, div, prelude::FluentBuilder, px, size,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Input;

use crate::github::cli::AuthState;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, button_disabled, clawd, icon, section_label, spinner};

const CLAUDE_INSTALL_PAGE: &str = "https://code.claude.com/docs/en/setup";
const GH_INSTALL_PAGE: &str = "https://cli.github.com";

/// Said after Check again when a tool is still missing. Every check reads the PATH again (`platform::reload_path`),
/// so what is left is the install itself.
const STILL_MISSING: &str = "Still not found? Finish the install, then press Check again.";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Nothing exists yet: Continue is off until `claude` is found, and closing the window quits.
    FirstLaunch,
    /// Opened from the running app: Close always works.
    Reopened,
}

pub enum SetupEvent {
    /// The user is done. Holds the `claude` path they gave, `None` for "find it yourself".
    Done(Option<PathBuf>),
    /// Run `gh auth login` in a terminal tab of the main window (only sent in `Mode::Reopened`).
    SignIn,
}

pub fn window_options(cx: &App) -> WindowOptions {
    let window = size(px(560.0), px(620.0));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, window, cx))),
        window_min_size: Some(window),
        is_resizable: false,
        titlebar: Some(TitlebarOptions { title: Some("Set up Claudiu".into()), ..Default::default() }),
        ..Default::default()
    }
}

/// What the user typed for the `claude` path: empty means "find it yourself", anything else must be a file or a
/// program on PATH (the same rule as Settings).
pub fn parse_path(text: &str) -> Result<Option<PathBuf>, String> {
    let text = text.trim().trim_matches('"');
    if text.is_empty() {
        return Ok(None);
    }
    platform::resolve_program(text).map(Some).ok_or_else(|| format!("“{text}” is not a file or a program on PATH"))
}

struct Checked {
    claude: Option<PathBuf>,
    gh: AuthState,
}

pub struct SetupView {
    mode: Mode,
    path_input: Entity<InputState>,
    /// The path the last check used.
    configured: Option<PathBuf>,
    path_error: Option<String>,
    /// `None` while the tools are being looked for.
    checked: Option<Checked>,
    /// Check again was pressed, so a tool that is still missing may be waiting for its install to finish.
    asked_again: bool,
    /// Counts the checks, so the answer of an older `gh` call cannot overwrite a newer one.
    checks: u64,
    /// Reads the PATH again before each check; `platform::reload_path`, except in tests.
    reload_path: fn(),
    done: bool,
}

impl EventEmitter<SetupEvent> for SetupView {}

impl SetupView {
    pub fn new(mode: Mode, current: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_reload(mode, current, platform::reload_path, window, cx)
    }

    fn with_reload(mode: Mode, current: Option<PathBuf>, reload_path: fn(), window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = current.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let path_input = cx.new(|cx| InputState::new(window, cx).default_value(text).placeholder("Found automatically"));
        cx.subscribe(&path_input, |view: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                view.check_again(cx);
            }
        })
        .detach();
        if mode == Mode::Reopened {
            // The X gives the path back like Close does, which then closes the window itself. On a first launch it just
            // closes, which quits the app.
            let view = cx.entity().downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                // A view that is already gone has nothing to hand back, so the window may just close.
                view.update(cx, |view, cx| view.close(window, cx)).is_err()
            });
        }
        let mut view = Self { mode, path_input, configured: current, path_error: None, checked: None, asked_again: false, checks: 0, reload_path, done: false };
        view.run_checks(cx);
        view
    }

    /// Look for both tools off the UI thread, after reading the PATH again: a Finder-launched macOS app starts with a
    /// minimal one, and a tool installed since Claudiu started is not on any.
    fn run_checks(&mut self, cx: &mut Context<Self>) {
        self.checked = None;
        self.checks += 1;
        let check = self.checks;
        let configured = self.configured.clone();
        let reload_path = self.reload_path;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    reload_path();
                    Checked { claude: claude::discover(configured.as_deref()), gh: crate::github::cli::check_auth() }
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.checks == check {
                    view.checked = Some(found);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn check_again(&mut self, cx: &mut Context<Self>) {
        match parse_path(self.path_input.read(cx).value().as_ref()) {
            Ok(path) => (self.configured, self.path_error) = (path, None),
            Err(message) => self.path_error = Some(message),
        }
        self.asked_again = true;
        self.run_checks(cx);
    }

    /// The path to give back: what the field says now, or the last good one when the field holds something unusable.
    fn path_to_hand_back(&self, cx: &App) -> Option<PathBuf> {
        parse_path(self.path_input.read(cx).value().as_ref()).unwrap_or_else(|_| self.configured.clone())
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        self.done = true;
        cx.emit(SetupEvent::Done(self.path_to_hand_back(cx)));
    }

    /// Hand the path back and close the window. The window goes in a deferred step: the main window listens through a
    /// subscription only, and a view dropped before its queued event is delivered takes the event with it.
    fn close(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.finish(cx);
        cx.defer_in(window, |_, window, _| window.remove_window());
    }

    fn can_continue(&self) -> bool {
        self.mode == Mode::Reopened || self.checked.as_ref().is_some_and(|found| found.claude.is_some())
    }

    fn needs_restart_hint(&self) -> bool {
        self.asked_again && self.checked.as_ref().is_some_and(|found| found.claude.is_none() || found.gh == AuthState::Missing)
    }
}

fn card() -> gpui::Div {
    div().flex().flex_col().gap(px(10.0)).p(px(14.0)).rounded(px(8.0)).bg(hsla(0x111111)).border_1().border_color(hsla(0x242424))
}

fn status_line(glyph: IconName, color: u32, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(crate::panels::scale::px(13.0))
        .child(div().flex_none().text_color(hsla(color)).child(icon(glyph, 15.0)))
        .child(div().min_w_0().child(text.into()))
}

fn checking_line(id: &'static str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(crate::panels::scale::px(13.0))
        .text_color(hsla(theme::TEXT_DIM))
        .child(spinner(id, 15.0))
        .child("Checking…")
}

fn faint(text: impl Into<SharedString>) -> gpui::Div {
    div().text_size(crate::panels::scale::px(11.5)).text_color(hsla(theme::TEXT_FAINT)).child(text.into())
}

fn tool_title(name: &'static str, tag: &'static str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(div().text_size(crate::panels::scale::px(14.0)).font_weight(gpui::FontWeight::SEMIBOLD).child(name))
        .child(section_label(tag))
}

impl SetupView {
    fn open_page(id: &'static str, label: &'static str, url: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        button(id, label, true).on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.open_url(url)))
    }

    fn claude_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let found = self.checked.as_ref().map(|found| &found.claude);
        let state = match found {
            None => checking_line("setup-claude-spin"),
            Some(Some(path)) => status_line(IconName::CircleCheck, theme::OK, format!("Found at {}", path.display())),
            Some(None) => status_line(IconName::CircleX, theme::DANGER, "Not found. Claudiu cannot start sessions without it."),
        };
        card()
            .child(tool_title("Claude Code", "REQUIRED"))
            .child(state)
            .when(found == Some(&None), |row| row.child(div().flex().child(Self::open_page("setup-get-claude", "Get Claude Code", CLAUDE_INSTALL_PAGE, cx))))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(section_label("PATH TO CLAUDE (ONLY IF IT IS SOMEWHERE UNUSUAL)"))
                    .child(Input::new(&self.path_input))
                    .children(self.path_error.clone().map(|message| div().text_size(crate::panels::scale::px(12.0)).text_color(hsla(theme::DANGER)).child(message))),
            )
    }

    fn gh_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (state, action) = match self.checked.as_ref().map(|found| &found.gh) {
            None => (checking_line("setup-gh-spin"), None),
            Some(AuthState::Ready) => (status_line(IconName::CircleCheck, theme::OK, "Installed and signed in"), None),
            Some(AuthState::Missing) => (
                status_line(IconName::CircleSlash, theme::TEXT_DIM, "Not found. Only the GitHub pane needs it."),
                Some(Self::open_page("setup-get-gh", "Get GitHub CLI", GH_INSTALL_PAGE, cx).into_any_element()),
            ),
            Some(AuthState::SignedOut) => {
                let sign_in = match self.mode {
                    Mode::Reopened => button("setup-gh-sign-in", "Sign in with gh", true).on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(SetupEvent::SignIn))).into_any_element(),
                    Mode::FirstLaunch => div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(button_disabled("setup-gh-sign-in", "Sign in with gh"))
                        .child(faint("Sign in from the GitHub pane after you continue."))
                        .into_any_element(),
                };
                (status_line(IconName::CircleX, theme::WARN, "Installed, but not signed in."), Some(sign_in))
            }
            Some(AuthState::Unknown(message)) => {
                let first_line = message.lines().next().unwrap_or_default().to_string();
                (status_line(IconName::CircleX, theme::WARN, format!("Could not check: {first_line}")), None)
            }
        };
        card().child(tool_title("GitHub CLI", "OPTIONAL")).child(state).children(action.map(|a| div().flex().child(a)))
    }
}

impl Render for SetupView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let primary = if self.mode == Mode::Reopened {
            button("setup-done", "Close", true).on_click(cx.listener(|view, _: &ClickEvent, window, cx| view.close(window, cx)))
        } else if self.can_continue() {
            button("setup-done", "Continue", true).on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.finish(cx)))
        } else {
            button_disabled("setup-done", "Continue")
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(28.0))
            .bg(hsla(theme::APP_BG))
            .text_color(hsla(theme::TEXT))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(clawd(40.0))
                    .child(div().text_size(crate::panels::scale::px(20.0)).font_weight(gpui::FontWeight::SEMIBOLD).child("Set up Claudiu"))
                    .child(
                        div()
                            .text_size(crate::panels::scale::px(13.0))
                            .text_color(hsla(theme::TEXT_DIM))
                            .child("Claudiu runs your own claude and reaches GitHub through your own gh. It installs nothing; this page only checks that they are there."),
                    ),
            )
            .child(self.claude_row(cx))
            .child(self.gh_row(cx))
            .children(self.needs_restart_hint().then(|| faint(STILL_MISSING)))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.0))
                    .child(button("setup-check", "Check again", false).on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.check_again(cx))))
                    .child(primary),
            )
    }
}

impl Workspace {
    /// Show the setup window, or bring it forward when it is already open.
    pub(super) fn open_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Settings' unsaved fields go with the dialog; the setup window edits the same `claude` path.
        self.modal = None;
        self.focus_active(window, cx);
        if let Some(open) = self.setup_window
            && open.update(cx, |_, window, _| window.activate_window()).is_ok()
        {
            return;
        }
        let current = self.store.data.settings.claude_path.clone();
        let opened = gpui_kit::open_window(window_options(cx), cx, |window, cx| cx.new(|cx| SetupView::new(Mode::Reopened, current, window, cx)));
        let Ok((handle, view)) = opened else {
            self.toast("Could not open the setup window.", true, cx);
            return;
        };
        self.setup_window = Some(handle);
        cx.subscribe_in(&view, window, |ws: &mut Workspace, _, ev: &SetupEvent, window, cx| match ev {
            SetupEvent::Done(path) => {
                ws.setup_window = None;
                ws.apply_setup(path.clone(), cx);
            }
            SetupEvent::SignIn => {
                window.activate_window();
                ws.sign_in_to_github(window, cx);
            }
        })
        .detach();
    }

    /// Use what the setup window ended with: its `claude` path (`None` clears it), looked for again, and saved. On a
    /// first launch the save also creates the state file.
    pub fn apply_setup(&mut self, claude_path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.store.data.settings.claude_path = claude_path;
        self.claude_exe = claude::discover(self.store.data.settings.claude_path.as_deref());
        self.store.mark_dirty();
        self.store.save();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn the_window_draws_finds_the_given_claude_and_hands_its_path_back_once() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("claude.exe");
        std::fs::write(&file, "").unwrap();

        let mut cx = TestAppContext::single();
        cx.update(|app| {
            gpui_kit::init(app);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, app);
        });
        let given = Some(file.clone());
        let (window, view) = cx
            .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| SetupView::with_reload(Mode::FirstLaunch, given, || {}, window, cx))))
            .unwrap();
        let handed_back: Rc<RefCell<Vec<Option<PathBuf>>>> = Rc::default();
        let log = handed_back.clone();
        let _watch = cx.update(|app| {
            app.subscribe(&view, move |_, ev: &SetupEvent, _| {
                if let SetupEvent::Done(path) = ev {
                    log.borrow_mut().push(path.clone());
                }
            })
        });
        for _ in 0..3 {
            cx.update_window(window, |_, window, app| window.draw(app).clear(app)).unwrap();
            cx.run_until_parked();
        }

        view.read_with(&cx, |view, _| {
            let found = view.checked.as_ref().expect("the check has finished");
            assert_eq!(found.claude, Some(file.clone()), "a path that exists is the claude found");
            assert!(view.can_continue());
        });
        for _ in 0..2 {
            view.update(&mut cx, |view, cx| view.finish(cx));
        }
        assert_eq!(*handed_back.borrow(), vec![Some(file)], "one Done, carrying the path");
    }

    #[test]
    fn closing_the_window_still_hands_the_path_back() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let (window, view) = cx
            .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| SetupView::with_reload(Mode::Reopened, None, || {}, window, cx))))
            .unwrap();
        let handed_back: Rc<RefCell<Vec<Option<PathBuf>>>> = Rc::default();
        let log = handed_back.clone();
        let _watch = cx.update(|app| {
            app.subscribe(&view, move |_, ev: &SetupEvent, _| {
                if let SetupEvent::Done(path) = ev {
                    log.borrow_mut().push(path.clone());
                }
            })
        });

        // The main window holds no handle on the view, only a subscription, so neither may the test.
        let weak = view.downgrade();
        drop(view);
        cx.update_window(window, |_, window, app| weak.update(app, |view, cx| view.close(window, cx))).unwrap().unwrap();
        cx.run_until_parked();

        assert_eq!(handed_back.borrow().len(), 1, "the window's Done reached its listener before the window went");
        assert!(cx.update(|app| app.windows().is_empty()), "and the window closed");
    }

    #[test]
    fn closing_never_waits_on_claude_but_continuing_on_a_first_launch_does() {
        let mut cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        let modes = [(Mode::FirstLaunch, false), (Mode::Reopened, true)];
        for (mode, expected) in modes {
            // A path that does not exist leaves `claude` to discovery, so the machine decides whether it is found;
            // only the Reopened answer is the same everywhere.
            let (_, view) = cx
                .update(|app| gpui_kit::open_window(Default::default(), app, |window, app| app.new(|cx| SetupView::with_reload(mode, None, || {}, window, cx))))
                .unwrap();
            view.update(&mut cx, |view, _| {
                view.checked = Some(Checked { claude: None, gh: AuthState::Missing });
                assert_eq!(view.can_continue(), expected);
            });
        }
    }

    #[test]
    fn the_claude_path_field_is_empty_a_real_program_or_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("claude.exe");
        std::fs::write(&file, "").unwrap();

        assert_eq!(parse_path(""), Ok(None));
        assert_eq!(parse_path("   "), Ok(None));
        assert_eq!(parse_path(&file.to_string_lossy()), Ok(Some(file.clone())));
        assert_eq!(parse_path(&format!("\"{}\"", file.display())), Ok(Some(file)), "pasted quotes are dropped");
        let missing = tmp.path().join("nothing-here");
        let error = parse_path(&missing.to_string_lossy()).unwrap_err();
        assert!(error.contains("is not a file or a program on PATH"), "{error}");
    }
}
