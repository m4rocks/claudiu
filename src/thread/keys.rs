//! Claude Code's keyboard shortcuts in a thread's composer, the same for every harness. Enter and Shift+Enter are
//! the composer's own (`submit_on_enter`); this decides the rest before the composer sees the key.

use std::time::Duration;

use gpui::Keystroke;

/// Two presses of Escape closer together than this are a double press.
pub const DOUBLE_PRESS: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Escape while a picker popover (model/effort or permission) is open: close it.
    ClosePicker,
    /// Escape or Ctrl+C mid-turn: stop the turn.
    Interrupt,
    /// Escape on a permission question: turn it down.
    Decline,
    /// A digit on a permission question: pick that option (from 1).
    Choose(usize),
    /// Escape twice with nothing typed: bring the last message back to edit.
    EditLast,
    /// Escape twice, or Ctrl+C, with text typed: clear it.
    Clear,
    /// Shift+Tab: the next permission mode.
    CycleMode,
    /// Up and Down through the messages sent before.
    Older,
    Newer,
    /// Tab on a slash command being typed: complete it.
    Complete,
    /// Ctrl+O: show every tool's output in full, or fold it again.
    ToggleDetail,
    /// Ctrl+T: show or hide the plan.
    TogglePlan,
    /// Alt+P (Option+P): the model picker.
    PickModel,
}

/// What the thread knows when a key arrives.
#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    pub picker_open: bool,
    pub working: bool,
    /// How many options the open permission question has; 0 when none is open.
    pub question: usize,
    pub draft_empty: bool,
    /// The composer shows a message from the history rather than a new one.
    pub in_history: bool,
    /// Escape was pressed a moment ago (`DOUBLE_PRESS`).
    pub escape_just_pressed: bool,
    /// What is typed is the start of a slash command with a match.
    pub completing: bool,
}

pub fn key(keystroke: &Keystroke, s: State) -> Option<Key> {
    let m = &keystroke.modifiers;
    let plain = !m.control && !m.alt && !m.platform && !m.shift && !m.function;
    match keystroke.key.as_str() {
        "escape" if plain && s.picker_open => Some(Key::ClosePicker),
        "escape" if plain && s.question > 0 => Some(Key::Decline),
        "escape" if plain && s.working => Some(Key::Interrupt),
        "escape" if plain && s.escape_just_pressed => Some(if s.draft_empty { Key::EditLast } else { Key::Clear }),
        "c" if m.control && !m.platform && !m.alt && !m.shift && s.working => Some(Key::Interrupt),
        "c" if m.control && !m.platform && !m.alt && !m.shift && !s.draft_empty => Some(Key::Clear),
        "tab" if m.shift && !m.control && !m.alt && !m.platform => Some(Key::CycleMode),
        "tab" if plain && s.completing => Some(Key::Complete),
        "up" if plain && (s.draft_empty || s.in_history) => Some(Key::Older),
        "down" if plain && s.in_history => Some(Key::Newer),
        "o" if m.control && !m.platform && !m.alt && !m.shift => Some(Key::ToggleDetail),
        "t" if m.control && !m.platform && !m.alt && !m.shift => Some(Key::TogglePlan),
        "p" if m.alt && !m.control && !m.platform && !m.shift => Some(Key::PickModel),
        digit if plain && s.question > 0 && s.draft_empty => {
            let n: usize = digit.parse().ok()?;
            (1..=s.question).contains(&n).then_some(Key::Choose(n))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(keys: &str, s: State) -> Option<Key> {
        key(&Keystroke::parse(keys).unwrap(), s)
    }

    fn idle() -> State {
        State { draft_empty: true, ..Default::default() }
    }

    #[test]
    fn escape_closes_an_open_picker_before_anything_else() {
        let open = State { picker_open: true, question: 2, working: true, escape_just_pressed: true, ..idle() };
        assert_eq!(press("escape", open), Some(Key::ClosePicker));
    }

    #[test]
    fn escape_interrupts_a_turn_and_twice_edits_the_last_message() {
        assert_eq!(press("escape", State { working: true, ..idle() }), Some(Key::Interrupt));
        assert_eq!(press("escape", idle()), None, "once while idle does nothing yet");
        assert_eq!(press("escape", State { escape_just_pressed: true, ..idle() }), Some(Key::EditLast));
        assert_eq!(press("escape", State { escape_just_pressed: true, draft_empty: false, ..idle() }), Some(Key::Clear));
    }

    #[test]
    fn a_permission_question_takes_escape_and_digits() {
        let asking = State { question: 3, working: true, ..idle() };
        assert_eq!(press("escape", asking), Some(Key::Decline), "before the interrupt");
        assert_eq!(press("2", asking), Some(Key::Choose(2)));
        assert_eq!(press("4", asking), None, "no fourth option");
        assert_eq!(press("0", asking), None);
        assert_eq!(press("2", State { draft_empty: false, ..asking }), None, "typing a reply instead");
        assert_eq!(press("2", idle()), None, "no question: a digit is typed");
    }

    #[test]
    fn ctrl_c_interrupts_or_clears() {
        assert_eq!(press("ctrl-c", State { working: true, ..idle() }), Some(Key::Interrupt));
        assert_eq!(press("ctrl-c", State { draft_empty: false, ..idle() }), Some(Key::Clear));
        assert_eq!(press("ctrl-c", idle()), None);
        assert_eq!(press("cmd-c", State { working: true, ..idle() }), None, "Cmd+C copies");
    }

    #[test]
    fn history_walks_only_from_an_empty_composer() {
        assert_eq!(press("up", idle()), Some(Key::Older));
        assert_eq!(press("up", State { draft_empty: false, ..idle() }), None, "moves the cursor in a draft");
        assert_eq!(press("up", State { draft_empty: false, in_history: true, ..idle() }), Some(Key::Older));
        assert_eq!(press("down", idle()), None);
        assert_eq!(press("down", State { draft_empty: false, in_history: true, ..idle() }), Some(Key::Newer));
    }

    #[test]
    fn the_other_shortcuts() {
        assert_eq!(press("shift-tab", idle()), Some(Key::CycleMode));
        assert_eq!(press("tab", idle()), None, "tab is left to the composer");
        assert_eq!(press("tab", State { completing: true, ..idle() }), Some(Key::Complete));
        assert_eq!(press("ctrl-o", idle()), Some(Key::ToggleDetail));
        assert_eq!(press("ctrl-t", idle()), Some(Key::TogglePlan));
        assert_eq!(press("alt-p", idle()), Some(Key::PickModel));
        assert_eq!(press("p", idle()), None);
    }
}
