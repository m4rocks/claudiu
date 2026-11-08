//! The app's keyboard shortcuts and menu bar commands. Each is a `Command`; `Workspace::command` runs it, whether it came
//! from a key or the menu bar.

use super::*;
use crate::terminal::keys::Keystroke;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NewClaude,
    ToggleShell,
    CloseSession,
    NextSession,
    PrevSession,
    ToggleSidebar,
    ToggleRight,
    FocusSearch,
    ImportProject,
    SwapFocus,
    OpenGithub,
    OpenSettings,
    /// Show the running session with this number, counted down the sidebar's RUNNING list.
    ShowSession(usize),
}

/// The shortcuts, written as `Keystroke::parse` reads them. `secondary` is Cmd on macOS and Ctrl elsewhere.
const CHORDS: &[(&str, Command)] = &[
    ("secondary-shift-n", Command::NewClaude),
    ("ctrl-`", Command::ToggleShell),
    ("secondary-j", Command::ToggleShell),
    ("secondary-shift-w", Command::CloseSession),
    ("secondary-shift-b", Command::ToggleSidebar),
    ("secondary-shift-r", Command::ToggleRight),
    ("secondary-shift-f", Command::FocusSearch),
    ("secondary-shift-o", Command::ImportProject),
    ("secondary-shift-g", Command::OpenGithub),
    ("secondary-,", Command::OpenSettings),
    ("secondary-shift-]", Command::SwapFocus),
    ("ctrl-tab", Command::NextSession),
    ("ctrl-shift-tab", Command::PrevSession),
];

fn secondary() -> &'static str {
    if cfg!(target_os = "macos") { "cmd" } else { "ctrl" }
}

/// `secondary-shift-n` as this platform writes it: `cmd-shift-n` on macOS, `ctrl-shift-n` elsewhere.
fn resolve(spec: &str) -> String {
    spec.replace("secondary", secondary())
}

/// The modifiers and key of a keystroke as a chord spec, to compare with `CHORDS`.
fn chord_of(ks: &Keystroke) -> String {
    let m = &ks.modifiers;
    let mut parts: Vec<&str> = vec![];
    if m.control {
        parts.push("ctrl");
    }
    if m.alt {
        parts.push("alt");
    }
    // The order `CHORDS` writes them in.
    if m.platform {
        parts.push("cmd");
    }
    if m.shift {
        parts.push("shift");
    }
    let key = match ks.key.as_str() {
        // Shift+] types `}`; the chord names the key.
        "}" => "]",
        "~" => "`",
        "<" => ",",
        k => k,
    };
    parts.push(key);
    parts.join("-")
}

impl Command {
    /// The command a key press runs, if any.
    pub fn for_key(ks: &Keystroke) -> Option<Command> {
        let chord = chord_of(ks);
        if let Some((_, c)) = CHORDS.iter().find(|(spec, _)| resolve(spec) == chord) {
            return Some(*c);
        }
        // Secondary+1 to Secondary+9.
        let m = &ks.modifiers;
        let secondary_alone = if cfg!(target_os = "macos") { m.platform && !m.control } else { m.control && !m.platform };
        if secondary_alone && !m.shift && !m.alt {
            let n = ks.key.parse::<usize>().ok().filter(|n| (1..=9).contains(n))?;
            return Some(Command::ShowSession(n));
        }
        None
    }

    /// The shortcut shown beside the command in the menu bar, as Rune's `Shortcut` reads it.
    fn accelerator(self) -> Option<&'static str> {
        Some(match self {
            Command::NewClaude => "mod+shift+n",
            Command::ToggleShell => "mod+j",
            Command::CloseSession => "mod+shift+w",
            Command::ToggleSidebar => "mod+shift+b",
            Command::ToggleRight => "mod+shift+r",
            Command::FocusSearch => "mod+shift+f",
            Command::ImportProject => "mod+shift+o",
            Command::OpenGithub => "mod+shift+g",
            Command::OpenSettings => "mod+,",
            Command::SwapFocus => "mod+shift+]",
            _ => return None,
        })
    }
}

/// The menu bar: Claudiu (Settings), File, View. Edit, Window and Quit come from Rune's standard menus.
const QUIT: &str = "Quit Claudiu";

impl Workspace {
    /// Move ⌘Q to the app's Quit once the menu bar is up, looking again for up to 10 s: a run with no menu bar
    /// (headless, tests) must not keep waking the app.
    pub(super) fn take_quit_key(&mut self, tries_left: u32) {
        if !platform::take_quit_key(QUIT) && tries_left > 0 {
            self.rt.after(Duration::from_millis(250), move |ws: &mut Workspace| ws.take_quit_key(tries_left - 1));
        }
    }
}

pub fn menu_bar() -> Vec<rune::MenuItem<Workspace>> {
    fn item(title: &str, menu: &str, c: Command, run: fn(&mut Workspace, &mut Tx)) -> rune::MenuItem<Workspace> {
        let mut item = rune::MenuItem::new(title, run).menu(menu).keys_first();
        if let Some(shortcut) = c.accelerator().and_then(|a| a.parse::<rune::Shortcut>().ok()) {
            item = item.key_equivalent(shortcut);
        }
        item
    }
    vec![
        {
            let mut settings = rune::MenuItem::new("Settings…", |ws: &mut Workspace, tx: &mut Tx| ws.run(Command::OpenSettings, tx)).keys_first();
            if let Ok(s) = "mod+,".parse() {
                settings = settings.key_equivalent(s);
            }
            settings
        },
        rune::MenuItem::separator(),
        {
            // AppKit's own Quit cannot ask first, so this one takes its ⌘Q (`Workspace::take_quit_key`).
            rune::MenuItem::new(QUIT, |ws: &mut Workspace, tx: &mut Tx| ws.quit_or_ask(tx))
        },
        item("New Claude Session", "File", Command::NewClaude, |ws, tx| ws.run(Command::NewClaude, tx)),
        item("Add Project…", "File", Command::ImportProject, |ws, tx| ws.run(Command::ImportProject, tx)),
        rune::MenuItem::separator().menu("File"),
        item("Close Session", "File", Command::CloseSession, |ws, tx| ws.run(Command::CloseSession, tx)),
        item("Toggle Sidebar", "View", Command::ToggleSidebar, |ws, tx| ws.run(Command::ToggleSidebar, tx)),
        item("Toggle Right Sidebar", "View", Command::ToggleRight, |ws, tx| ws.run(Command::ToggleRight, tx)),
        item("Toggle Shell", "View", Command::ToggleShell, |ws, tx| ws.run(Command::ToggleShell, tx)),
        item("Swap Terminal and Shell", "View", Command::SwapFocus, |ws, tx| ws.run(Command::SwapFocus, tx)),
        rune::MenuItem::separator().menu("View"),
        item("Search Sessions", "View", Command::FocusSearch, |ws, tx| ws.run(Command::FocusSearch, tx)),
        item("GitHub", "View", Command::OpenGithub, |ws, tx| ws.run(Command::OpenGithub, tx)),
        rune::MenuItem::separator().menu("View"),
        item("Next Session", "View", Command::NextSession, |ws, tx| ws.run(Command::NextSession, tx)),
        item("Previous Session", "View", Command::PrevSession, |ws, tx| ws.run(Command::PrevSession, tx)),
    ]
}

impl Workspace {
    /// Run a command, then hand its effects to the frame.
    pub fn run(&mut self, c: Command, tx: &mut Tx) {
        self.command(c);
        self.after_input(tx);
    }

    pub fn command(&mut self, c: Command) {
        match c {
            Command::NewClaude => {
                let cwd = self.new_session_cwd();
                self.act(Act::NewClaude { cwd })
            }
            Command::ToggleShell => {
                if let Some(id) = self.active.clone() {
                    self.act(Act::ToggleShell(id))
                }
            }
            Command::CloseSession => {
                if let Some(id) = self.active.clone() {
                    self.act(Act::CloseLive(id))
                }
            }
            Command::NextSession => self.cycle(1),
            Command::PrevSession => self.cycle(-1),
            Command::ShowSession(n) => self.show_numbered(n),
            Command::ToggleSidebar => self.toggle_left(),
            Command::ToggleRight => self.toggle_right(),
            Command::FocusSearch => {
                self.store.data.layout.sidebar_visible = true;
                self.store.mark_dirty();
                self.search.focused = true;
            }
            Command::ImportProject => self.act(Act::ImportProject),
            Command::OpenGithub => {
                if self.active_id().is_some() {
                    let cwd = self.active_cwd();
                    self.act(Act::OpenGithub { cwd })
                }
            }
            Command::OpenSettings => self.act(Act::OpenSettings),
            Command::SwapFocus => self.swap_focus(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(spec: &str) -> Option<Command> {
        Command::for_key(&Keystroke::parse(&resolve(spec)).unwrap())
    }

    #[test]
    fn every_chord_runs_its_command() {
        for (spec, c) in CHORDS {
            assert_eq!(key(spec), Some(*c), "{spec}");
        }
    }

    #[test]
    fn secondary_and_a_digit_shows_that_session_but_shift_does_not() {
        assert_eq!(key("secondary-3"), Some(Command::ShowSession(3)));
        assert_eq!(key("secondary-0"), None);
        assert_eq!(key("secondary-shift-3"), None);
        assert_eq!(key("n"), None, "a letter alone is typing");
    }
}
