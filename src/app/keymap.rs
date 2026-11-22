//! m4code's keyboard shortcuts: the bindings `main` installs, and the list Settings → Keyboard shows. The chat's own
//! keys are read in `thread/keys.rs` rather than bound, so the list names them by hand. A test keeps every binding
//! in the list.

use gpui::{KeyBinding, Keystroke};

use super::*;

/// m4code-global shortcuts. Deliberately Ctrl+Shift (Cmd+Shift on macOS) so every plain Ctrl/Alt chord, e.g. Alt+P,
/// still reaches the agent untouched.
pub fn bindings() -> Vec<KeyBinding> {
    let mut keys = vec![
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
        // Only over the editor pane: elsewhere Ctrl+P belongs to the agent.
        KeyBinding::new("secondary-p", QuickOpen, Some("EditorPane")),
        KeyBinding::new("secondary-s", SaveFile, Some("EditorPane")),
        KeyBinding::new("ctrl-g", GoToLine, Some("EditorPane")),
        KeyBinding::new("secondary-w", CloseTab, Some("EditorPane")),
        // VS Code's keys: Alt+arrows move by word on macOS.
        KeyBinding::new(GO_BACK, GoBack, Some("EditorPane")),
        KeyBinding::new(GO_FORWARD, GoForward, Some("EditorPane")),
        // The editor answers it from its language server; Ctrl+click (Cmd+click) does the same.
        KeyBinding::new("f12", gpui_kit::component::input::GoToDefinition, Some("Input")),
        // Completions for the word before the cursor, from the file's language server.
        KeyBinding::new("ctrl-space", gpui_kit::base::input::TriggerCompletion, Some("Input")),
        KeyBinding::new("ctrl-tab", NextSession, None),
        KeyBinding::new("ctrl-shift-tab", PrevSession, None),
        // The editor (Input) and, off macOS, a shell (Terminal) keep it for undo and job control.
        KeyBinding::new("secondary-z", ReopenClosed, None),
    ];
    keys.extend((1..=9).map(|n| KeyBinding::new(&format!("secondary-{n}"), ShowSession(n), None)));
    #[cfg(target_os = "macos")]
    keys.extend([KeyBinding::new("cmd-q", Quit, None), KeyBinding::new("cmd-h", HideApp, None), KeyBinding::new("cmd-alt-h", HideOthers, None)]);
    keys.extend(line_action_bindings());
    keys
}

const GO_BACK: &str = if cfg!(target_os = "macos") { "ctrl--" } else { "alt-left" };
const GO_FORWARD: &str = if cfg!(target_os = "macos") { "ctrl-shift--" } else { "alt-right" };

/// One row of Settings → Keyboard.
pub struct Shortcut {
    pub group: &'static str,
    /// In gpui's notation (`secondary-shift-n`, `escape escape`). Several are alternatives, or a range when more
    /// than two.
    pub keys: Vec<String>,
    pub what: &'static str,
}

/// Everything Settings → Keyboard lists, in its order.
pub fn shortcuts() -> Vec<Shortcut> {
    let row = |group, keys: &[&str], what| Shortcut { group, keys: keys.iter().map(|k| k.to_string()).collect(), what };
    let mut rows = vec![
        row("General", &["secondary-shift-n"], "New session"),
        row("General", &["secondary-,"], "Settings"),
        row("General", &["secondary-shift-o"], "Add a project"),
        row("General", &["secondary-shift-f"], "Find sessions"),
        row("General", &["secondary-shift-b"], "Show or hide the sidebar"),
        row("General", &["secondary-shift-r"], "Show or hide the right sidebar"),
        row("General", &["secondary-j", "ctrl-`"], "Show or hide the shell"),
        row("General", &["secondary-shift-]"], "Switch focus between the chat and the shell"),
        row("General", &["secondary-shift-e"], "Editor mode"),
        row("General", &["secondary-shift-g"], "GitHub issues and pull requests"),
    ];
    if cfg!(target_os = "macos") {
        rows.extend([row("General", &["cmd-q"], "Quit m4code"), row("General", &["cmd-h"], "Hide m4code"), row("General", &["cmd-alt-h"], "Hide other apps")]);
    }
    let numbered: Vec<String> = (1..=9).map(|n| format!("secondary-{n}")).collect();
    rows.push(Shortcut { group: "Sessions", keys: numbered, what: "Show a running session by its place in the sidebar" });
    rows.extend([
        row("Sessions", &["ctrl-tab"], "Next session"),
        row("Sessions", &["ctrl-shift-tab"], "Previous session"),
        row("Sessions", &["secondary-shift-w"], "Close the session"),
        row("Sessions", &["secondary-z"], "Reopen the last closed session"),
        row("Chat", &["enter"], "Send the message"),
        row("Chat", &["escape", "ctrl-c"], "Stop the agent's turn"),
        row("Chat", &["escape escape"], "Edit your last message, or clear the one you're typing"),
        row("Chat", &["shift-tab"], "Next permission mode"),
        row("Chat", &["up", "down"], "Earlier messages you sent"),
        row("Chat", &["alt-p"], "Choose the model"),
        row("Chat", &["ctrl-o"], "Show every tool's output in full"),
        row("Chat", &["ctrl-t"], "Show or hide the plan"),
        row("Editor", &["secondary-p"], "Open a file"),
        row("Editor", &["secondary-s"], "Save"),
        row("Editor", &["ctrl-g"], "Go to line"),
        row("Editor", &["secondary-w"], "Close the tab"),
        row("Editor", &[GO_BACK], "Go back"),
        row("Editor", &[GO_FORWARD], "Go forward"),
        row("Editor", &["f12"], "Go to definition"),
        row("Editor", &["ctrl-space"], "Show completions"),
        row("Editor", &["secondary-x"], "Cut the line"),
        row("Editor", &["secondary-c"], "Copy the line"),
        row("Editor", &["alt-up", "alt-down"], "Move the lines up or down"),
        row("Editor", &["shift-alt-up", "shift-alt-down"], "Duplicate the lines up or down"),
        row("Editor", &["secondary-shift-k"], "Delete the lines"),
        row("Editor", &["secondary-l"], "Select the line"),
        row("Editor", &["secondary-shift-up", "secondary-shift-down"], "Select whole lines up or down"),
    ]);
    rows
}

/// A shortcut's keys as the keyboard shows them: `Ctrl+Shift+N`, or `⌘⇧N` on macOS.
pub fn label(keys: &[String]) -> String {
    let one = |keys: &str| keys.split(' ').map(|stroke| stroke_label(stroke, cfg!(target_os = "macos"))).collect::<Vec<_>>().join(" ");
    match keys {
        [] => String::new(),
        [first, .., last] if keys.len() > 2 => format!("{} … {}", one(first), one(last)),
        _ => keys.iter().map(|k| one(k)).collect::<Vec<_>>().join(" or "),
    }
}

fn stroke_label(stroke: &str, macos: bool) -> String {
    let Ok(parsed) = Keystroke::parse(stroke) else { return stroke.to_string() };
    let m = parsed.modifiers;
    let key = match parsed.key.as_str() {
        "escape" => "Esc".to_string(),
        "enter" => "Enter".to_string(),
        "tab" => "Tab".to_string(),
        "space" => "Space".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        other => other.to_uppercase(),
    };
    if macos {
        let mut out = String::new();
        for (on, mark) in [(m.control, "⌃"), (m.alt, "⌥"), (m.shift, "⇧"), (m.platform, "⌘")] {
            if on {
                out.push_str(mark);
            }
        }
        return out + &key;
    }
    let mut parts: Vec<String> = [(m.control, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift"), (m.platform, "Super")].into_iter().filter(|(on, _)| *on).map(|(_, name)| name.to_string()).collect();
    parts.push(key);
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binding_is_listed_in_settings() {
        let listed: Vec<Vec<gpui::KeybindingKeystroke>> = shortcuts()
            .iter()
            .flat_map(|row| row.keys.clone())
            .map(|keys| KeyBinding::new(&keys, gpui::NoAction, None).keystrokes().to_vec())
            .collect();
        for binding in bindings() {
            assert!(listed.iter().any(|keys| keys.as_slice() == binding.keystrokes()), "{:?} is bound but not listed", binding.keystrokes());
        }
    }

    #[test]
    fn keys_read_as_the_keyboard_shows_them() {
        assert_eq!(stroke_label("ctrl-shift-n", false), "Ctrl+Shift+N");
        assert_eq!(stroke_label("cmd-shift-n", true), "⇧⌘N");
        assert_eq!(stroke_label("ctrl--", false), "Ctrl+-");
        assert_eq!(stroke_label("alt-left", false), "Alt+←");
        assert_eq!(stroke_label("escape", false), "Esc");
        assert_eq!(label(&["a".into(), "b".into()]), "A or B");
        assert_eq!(label(&["ctrl-1".into(), "ctrl-2".into(), "ctrl-9".into()]), if cfg!(target_os = "macos") { "⌃1 … ⌃9" } else { "Ctrl+1 … Ctrl+9" });
        assert_eq!(label(&["escape escape".into()]), "Esc Esc");
    }
}
