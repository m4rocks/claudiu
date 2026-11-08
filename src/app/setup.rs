//! The setup window: a short checklist of the two tools Claudiu runs (`claude`, required; `gh`, optional). It opens
//! alone on a first launch (see `main.rs`) and again from Settings or the missing-`claude` toast. It installs nothing.
//! It never owns the store: it hands the path the user typed and the recommended-trims choice back in `SetupEvent::Done`,
//! and the main window applies them.

use super::*;

use rune::tok::motion;
use rune::{Color, Cursor, Cx, El, ScrollSpec, Transition, col, el, row, text, text_input};

use crate::app::rt::Mailbox;
use crate::github::cli::AuthState;
use crate::panels::icons;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{button, button_disabled, clawd, font, icon, label, section_label, spinner};

const CLAUDE_INSTALL_PAGE: &str = "https://code.claude.com/docs/en/setup";
const GH_INSTALL_PAGE: &str = "https://cli.github.com";
const SIZE: (f32, f32) = (560.0, 620.0);

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

#[derive(Debug, PartialEq)]
pub enum SetupEvent {
    /// The user is done. `claude_path` is the path they gave, `None` for "find it yourself". `recommended_trims` asks
    /// the main window to turn off `trim::RECOMMENDED` once, as if they were switched off in Settings.
    Done { claude_path: Option<PathBuf>, recommended_trims: bool },
    /// Run `gh auth login` in a terminal tab of the main window (only sent in `Mode::Reopened`).
    SignIn,
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
    rt: Rt<SetupView>,
    mode: Mode,
    path_input: TextEdit,
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
    /// The "recommended trims" switch; off until the user turns it on.
    recommended_trims: bool,
    done: bool,
    /// Where the window's answers go: the main window's workspace (a log in tests).
    hand_back: Box<dyn Fn(SetupEvent)>,
}

impl HasRt for SetupView {
    fn rt(&mut self) -> &mut Rt<Self> {
        &mut self.rt
    }
}

/// Answers of a setup window go to the workspace. On a first launch the main window is hidden until then.
pub(super) fn to_workspace(mailbox: Mailbox<Workspace>, mode: Mode) -> Box<dyn Fn(SetupEvent)> {
    Box::new(move |ev| {
        mailbox.send(move |ws: &mut Workspace| {
            match ev {
                SetupEvent::Done { claude_path, recommended_trims } => {
                    ws.setup_window = None;
                    ws.apply_setup(claude_path, recommended_trims);
                }
                SetupEvent::SignIn => ws.sign_in_to_github(),
            }
            if mode == Mode::FirstLaunch {
                show_main_window();
            }
        });
    })
}

/// The setup window's spec: its own state, view and key handling.
fn window_spec(key: String, view: SetupView) -> rune::WindowSpec<SetupView> {
    let spec = rune::AppSpec {
        min_size: Some(SIZE),
        setup: Some(|v: &mut SetupView, waker: rune::Waker| v.rt.set_waker(waker)),
        poll: Some(|v: &mut SetupView, _t: f64, tx: &mut rune::Tx| {
            let ran = v.run_jobs();
            v.flush(tx);
            ran
        }),
        families: (platform::ui_font(), platform::monospace_font()),
        ..rune::AppSpec::new("Set up Claudiu", SIZE, || unreachable!("the view is built before the window"), render)
    };
    let app = rune::App::new(spec).init(move || view).on_close(|v: &mut SetupView, tx: &mut rune::Tx| {
        if v.closed() {
            tx.quit();
        }
    });
    rune::WindowSpec::from_app(key, app).options(rune::WindowOptions { resizable: false, min_size: Some(SIZE), ..Default::default() })
}

/// On a first launch: the setup window, before the (hidden) main window shows.
pub fn open_first_setup(app: &mut rune::App<Workspace>, mailbox: Mailbox<Workspace>) {
    let view = SetupView::new(Mode::FirstLaunch, None, to_workspace(mailbox, Mode::FirstLaunch));
    app.window(window_spec("setup".into(), view));
}

impl SetupView {
    pub fn new(mode: Mode, current: Option<PathBuf>, hand_back: Box<dyn Fn(SetupEvent)>) -> Self {
        Self::with_reload(mode, current, platform::reload_path, hand_back)
    }

    fn with_reload(mode: Mode, current: Option<PathBuf>, reload_path: fn(), hand_back: Box<dyn Fn(SetupEvent)>) -> Self {
        let text = current.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let mut view = Self {
            rt: Rt::default(),
            mode,
            path_input: TextEdit::new(text),
            configured: current,
            path_error: None,
            checked: None,
            asked_again: false,
            checks: 0,
            reload_path,
            recommended_trims: false,
            done: false,
            hand_back,
        };
        view.run_checks();
        view
    }

    /// Look for both tools off the UI thread, after reading the PATH again: a desktop-launched app starts with a
    /// minimal one, and a tool installed since Claudiu started is not on any.
    fn run_checks(&mut self) {
        self.checked = None;
        self.checks += 1;
        let check = self.checks;
        let configured = self.configured.clone();
        let reload_path = self.reload_path;
        self.rt.spawn(
            move || {
                reload_path();
                Checked { claude: claude::discover(configured.as_deref()), gh: crate::github::cli::check_auth() }
            },
            move |view: &mut SetupView, found| {
                if view.checks == check {
                    view.checked = Some(found);
                }
            },
        );
    }

    fn check_again(&mut self) {
        match parse_path(&self.path_input.text) {
            Ok(path) => (self.configured, self.path_error) = (path, None),
            Err(message) => self.path_error = Some(message),
        }
        self.asked_again = true;
        self.run_checks();
    }

    /// The path to give back: what the field says now, or the last good one when the field holds something unusable.
    fn path_to_hand_back(&self) -> Option<PathBuf> {
        parse_path(&self.path_input.text).unwrap_or_else(|_| self.configured.clone())
    }

    /// Hand the answer back, once.
    fn finish(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        (self.hand_back)(SetupEvent::Done { claude_path: self.path_to_hand_back(), recommended_trims: self.recommended_trims });
    }

    /// The X: it gives the path back like Close does. Returns whether the app should quit instead, which is on a first
    /// launch the user left before finishing.
    fn closed(&mut self) -> bool {
        if self.mode == Mode::FirstLaunch && !self.done {
            return true;
        }
        self.finish();
        false
    }

    fn can_continue(&self) -> bool {
        self.mode == Mode::Reopened || self.checked.as_ref().is_some_and(|found| found.claude.is_some())
    }

    fn needs_restart_hint(&self) -> bool {
        self.asked_again && self.checked.as_ref().is_some_and(|found| found.claude.is_none() || found.gh == AuthState::Missing)
    }
}

/// Run `f` on the view from a press, then hand its effects to the frame.
fn press(f: impl Fn(&mut SetupView, &mut rune::Tx) + 'static) -> impl Fn(&mut SetupView, &mut rune::Tx) + 'static {
    move |v: &mut SetupView, tx: &mut rune::Tx| {
        f(v, tx);
        v.flush(tx);
    }
}

fn card(key: &str) -> El {
    col().key(key.to_string()).w_full().gap(10.0).padding(14.0).radius(8.0).fill(rgb(0x111113)).border(1.0, rgb(0x242428)).motion(motion::REFLOW)
}

fn status_line(key: &str, glyph: &'static rune::Icon, color: u32, t: impl Into<String>) -> El {
    // Each answer fades in where "Checking…" was.
    row().key(key.to_string()).w_full().gap(8.0).center_y().transition(Transition::opacity()).child(icon(glyph, 15.0, color)).child(label(t, 13.0, theme::TEXT).wrap().shrink())
}

fn checking_line(key: &str) -> El {
    row().key(key.to_string()).gap(8.0).center_y().transition(Transition::opacity()).child(spinner(15.0, theme::TEXT_DIM)).child(label("Checking…", 13.0, theme::TEXT_DIM))
}

fn faint(t: impl Into<String>) -> El {
    label(t, 11.5, theme::TEXT_FAINT).wrap()
}

fn tool_title(name: &'static str, tag: &'static str) -> El {
    row().gap(8.0).center_y().child(text(name).font(font(14.0, 600.0)).color(rgb(theme::TEXT))).child(section_label(tag))
}

fn open_page(key: &'static str, title: &'static str, url: &'static str) -> El {
    button(title, true).key(key).on_press_tx(press(move |_, tx| {
        let _ = tx.open_url(url);
    }))
}

impl SetupView {
    fn trims_row(&self) -> El {
        let on = self.recommended_trims;
        card("setup-recommended-trims")
            .cursor(Cursor::Pointer)
            .role("switch")
            .hover(Color::WHITE.alpha(0.02))
            .child(
                row()
                    .w_full()
                    .center_y()
                    .justify_between()
                    .child(tool_title("Recommended trims", "OPTIONAL"))
                    .child(
                        row()
                            .w(28.0)
                            .h(16.0)
                            .padding(2.0)
                            .radius(8.0)
                            .fill(rgb(if on { theme::ACCENT } else { 0x2c2c30 }))
                            .motion(motion::SNAP)
                            .child(el().size(12.0, 12.0).radius(6.0).fill(rgb(if on { 0x17100d } else { 0x8a8a8a })).ml(if on { 12.0 } else { 0.0 }).motion(motion::SNAP)),
                    ),
            )
            .child(faint("Fewer tokens in every request. Change it later in Settings → Claude Code tools."))
            .on_press_tx(press(|v, _| v.recommended_trims = !v.recommended_trims))
    }

    fn claude_row(&self) -> El {
        let found = self.checked.as_ref().map(|found| &found.claude);
        let state = match found {
            None => checking_line("setup-claude-checking"),
            Some(Some(path)) => status_line("setup-claude-found", &icons::CIRCLE_CHECK, theme::OK, format!("Found at {}", path.display())),
            Some(None) => status_line("setup-claude-missing", &icons::CIRCLE_X, theme::DANGER, "Not found. Claudiu cannot start sessions without it."),
        };
        let mut c = card("setup-claude").child(tool_title("Claude Code", "REQUIRED")).child(state);
        if found == Some(&None) {
            c = c.child(row().child(open_page("setup-get-claude", "Get Claude Code", CLAUDE_INSTALL_PAGE)));
        }
        let field = text_input(&self.path_input, font(13.0, 400.0))
            .key("setup-path")
            .single_line()
            .placeholder("Found automatically", rgb(theme::TEXT_FAINT))
            .color(rgb(theme::TEXT))
            .caret(rgb(theme::ACCENT), rgb(theme::SELECTION))
            .w_full()
            .on_edit(|v: &mut SetupView| &mut v.path_input)
            .on_submit(press(|v, _| v.check_again()));
        let mut path = col()
            .w_full()
            .gap(4.0)
            .child(section_label("PATH TO CLAUDE (ONLY IF IT IS SOMEWHERE UNUSUAL)"))
            .child(row().h(30.0).w_full().px(9.0).center_y().radius(6.0).fill(rgb(0x101012)).border(1.0, rgb(if self.path_input.focused { theme::ACCENT } else { 0x2a2a2e })).motion(motion::SNAP).child(field));
        if let Some(message) = self.path_error.clone() {
            path = path.child(label(message, 12.0, theme::DANGER).wrap().key("setup-path-error").transition(Transition::opacity()));
        }
        c.child(path)
    }

    fn gh_row(&self) -> El {
        let (state, action) = match self.checked.as_ref().map(|found| &found.gh) {
            None => (checking_line("setup-gh-checking"), None),
            Some(AuthState::Ready) => (status_line("setup-gh-ready", &icons::CIRCLE_CHECK, theme::OK, "Installed and signed in"), None),
            Some(AuthState::Missing) => (status_line("setup-gh-missing", &icons::CIRCLE_SLASH, theme::TEXT_DIM, "Not found. Only the GitHub pane needs it."), Some(open_page("setup-get-gh", "Get GitHub CLI", GH_INSTALL_PAGE))),
            Some(AuthState::SignedOut) => {
                let sign_in = match self.mode {
                    Mode::Reopened => button("Sign in with gh", true).key("setup-gh-sign-in").on_press_tx(press(|v, _| (v.hand_back)(SetupEvent::SignIn))),
                    Mode::FirstLaunch => row().center_y().gap(10.0).child(button_disabled("Sign in with gh").key("setup-gh-sign-in")).child(faint("Sign in from the GitHub pane after you continue.")),
                };
                (status_line("setup-gh-signed-out", &icons::CIRCLE_X, theme::WARN, "Installed, but not signed in."), Some(sign_in))
            }
            Some(AuthState::Unknown(message)) => {
                let first_line = message.lines().next().unwrap_or_default().to_string();
                (status_line("setup-gh-unknown", &icons::CIRCLE_X, theme::WARN, format!("Could not check: {first_line}")), None)
            }
        };
        let mut c = card("setup-gh").child(tool_title("GitHub CLI", "OPTIONAL")).child(state);
        if let Some(a) = action {
            c = c.child(row().child(a));
        }
        c
    }
}

fn render(v: &SetupView, _cx: &mut Cx<SetupView>) -> El {
    let primary = if v.mode == Mode::Reopened {
        button("Close", true).key("setup-done").on_press_tx(press(|v, tx| {
            v.finish();
            tx.close_window();
        }))
    } else if v.can_continue() {
        button("Continue", true).key("setup-done").on_press_tx(press(|v, tx| {
            v.finish();
            tx.close_window();
        }))
    } else {
        button_disabled("Continue").key("setup-done")
    };
    let mut cards = col().w_full().gap(14.0).child(v.claude_row()).child(v.gh_row());
    if v.needs_restart_hint() {
        cards = cards.child(faint(STILL_MISSING).key("setup-still-missing").transition(Transition::opacity()));
    }
    cards = cards.child(v.trims_row());
    col()
        .key("setup")
        .w_full()
        .h_full()
        .gap(14.0)
        .padding(28.0)
        .pt(40.0)
        .fill(rgb(theme::APP_BG))
        .child(
            col()
                .w_full()
                .gap(8.0)
                .child(clawd(40.0))
                .child(text("Set up Claudiu").font(font(20.0, 600.0)).color(rgb(theme::TEXT)))
                .child(label("Claudiu runs your own claude and reaches GitHub through your own gh. It installs nothing; this page only checks that they are there.", 13.0, theme::TEXT_DIM).wrap()),
        )
        // The cards scroll under a fixed header and buttons: the window has one size, and UI scale or a long error
        // can make them taller than it.
        .child(col().key("setup-body").w_full().grow(1.0).shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(cards))
        .child(
            row()
                .w_full()
                .justify_end()
                .gap(8.0)
                .child(button("Check again", false).key("setup-check").on_press_tx(press(|v, _| v.check_again())))
                .child(primary),
        )
}

impl Workspace {
    /// Show the setup window, or bring it forward when it is already open.
    pub(super) fn open_setup(&mut self) {
        // Settings' unsaved fields go with the dialog; the setup window edits the same `claude` path.
        self.modal = None;
        self.focus_active();
        if let Some(open) = &self.setup_window {
            open.show();
            open.focus();
            return;
        }
        let current = self.store.data.settings.claude_path.clone();
        let view = SetupView::new(Mode::Reopened, current, to_workspace(self.rt.mailbox(), Mode::Reopened));
        // A new key each time: a closed window keeps its old state (`App::window`), and a reopened one starts fresh.
        self.setup_serial += 1;
        self.setup_window = Some(rune::open_window(window_spec(format!("setup-{}", self.setup_serial), view)));
    }

    /// Use what the setup window ended with: its `claude` path (`None` clears it), looked for again, and saved, and
    /// the recommended trims when asked for. On a first launch the save also creates the state file.
    pub fn apply_setup(&mut self, claude_path: Option<PathBuf>, recommended_trims: bool) {
        self.store.data.settings.claude_path = claude_path;
        self.claude_exe = claude::discover(self.store.data.settings.claude_path.as_deref());
        if recommended_trims {
            self.add_recommended_trims();
        }
        self.store.mark_dirty();
        self.store.save();
    }

    /// Write the recommended trims into Claude Code's settings.json, as saving them in Settings would. When the file
    /// can't be changed the trims stay as they were, so Settings shows what Claude Code has.
    fn add_recommended_trims(&mut self) {
        let s = &mut self.store.data.settings;
        let trimmed = claude::trim::with_recommended(&s.trimmed_tools);
        if trimmed == s.trimmed_tools {
            return;
        }
        let written = claude::claude_home()
            .ok_or_else(|| std::io::Error::other("Claude Code's folder was not found"))
            .and_then(|home| claude::trim::write_global(&home.join("settings.json"), &trimmed, &s.limits, &s.applied_trims));
        match written {
            Ok(owned) => {
                s.applied_trims = owned;
                s.trimmed_tools = trimmed;
            }
            Err(e) => self.toast(format!("Claude Code's settings.json could not be changed ({e}), so the recommended trims are not on."), true),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A view whose answers go to a log; the PATH is left alone.
    fn view(mode: Mode, current: Option<PathBuf>) -> (SetupView, Rc<RefCell<Vec<SetupEvent>>>) {
        let log: Rc<RefCell<Vec<SetupEvent>>> = Rc::default();
        let sink = log.clone();
        (SetupView::with_reload(mode, current, || {}, Box::new(move |ev| sink.borrow_mut().push(ev))), log)
    }

    fn wait_for_checks(v: &mut SetupView) {
        let start = std::time::Instant::now();
        while v.checked.is_none() && start.elapsed() < std::time::Duration::from_secs(30) {
            v.run_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn the_window_draws_finds_the_given_claude_and_hands_its_path_back_once() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("claude.exe");
        std::fs::write(&file, "").unwrap();
        let (mut v, log) = view(Mode::FirstLaunch, Some(file.clone()));
        wait_for_checks(&mut v);
        let found = v.checked.as_ref().expect("the check has finished");
        assert_eq!(found.claude, Some(file.clone()), "a path that exists is the claude found");
        assert!(v.can_continue());

        let mut rt = rune::Runtime::new(v, render, SIZE, rune::Env { system_dark: true, scale: 2.0 });
        rt.frame(0.016);
        assert!(rt.find("setup-done").is_some(), "Continue is drawn");
        assert!(rt.elements().iter().any(|e| e.text.as_deref().is_some_and(|t| t.starts_with("Found at"))), "and the path it found");
        for _ in 0..2 {
            rt.state.finish();
        }
        assert_eq!(*log.borrow(), vec![SetupEvent::Done { claude_path: Some(file), recommended_trims: false }], "one Done, carrying the path");
    }

    #[test]
    fn closing_the_window_hands_the_path_back_once_but_a_first_launch_quits() {
        let (mut v, log) = view(Mode::Reopened, None);
        assert!(!v.closed() && !v.closed(), "a reopened window never quits the app");
        assert_eq!(*log.borrow(), vec![SetupEvent::Done { claude_path: None, recommended_trims: false }], "one Done");
        let (mut v, log) = view(Mode::FirstLaunch, None);
        assert!(v.closed(), "leaving the first setup quits");
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn closing_never_waits_on_claude_but_continuing_on_a_first_launch_does() {
        for (mode, expected) in [(Mode::FirstLaunch, false), (Mode::Reopened, true)] {
            let (mut v, _) = view(mode, None);
            v.checked = Some(Checked { claude: None, gh: AuthState::Missing });
            assert_eq!(v.can_continue(), expected);
        }
    }

    #[test]
    fn the_recommended_trims_are_off_unless_the_user_turns_them_on() {
        for turn_on in [false, true] {
            let (mut v, log) = view(Mode::Reopened, None);
            if turn_on {
                v.recommended_trims = !v.recommended_trims;
            }
            v.finish();
            assert_eq!(*log.borrow(), vec![SetupEvent::Done { claude_path: None, recommended_trims: turn_on }]);
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
