//! The pages of the Settings dialog and its keyboard handling.

use super::*;
use gpui::KeyDownEvent;

/// A category in the Settings dialog's list, in display order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    #[default]
    General,
    Sessions,
    ClaudeTools,
    ClaudiuTools,
    Worktrees,
    About,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 6] = [Self::General, Self::Sessions, Self::ClaudeTools, Self::ClaudiuTools, Self::Worktrees, Self::About];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Sessions => "Sessions",
            Self::ClaudeTools => "Claude Code tools",
            Self::ClaudiuTools => "Claudiu tools",
            Self::Worktrees => "Worktrees",
            Self::About => "About",
        }
    }

    /// The page below this one; the last page stays put.
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(i + 1).min(Self::ALL.len() - 1)]
    }

    /// The page above this one; the first page stays put.
    pub fn prev(self) -> Self {
        let i = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[i.saturating_sub(1)]
    }
}

impl Workspace {
    /// Keys for the open Settings dialog, seen before the focused control. Up and Down change the page and Enter saves
    /// only while the category list has focus; Tab and Shift+Tab cycle the list and the current page's text fields.
    pub(crate) fn settings_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Settings { shell, claude, model, list_focus, .. }) = &self.modal else { return };
        let list_focus = list_focus.clone();
        let fields = match self.settings_page {
            SettingsPage::General => vec![shell.clone(), claude.clone()],
            SettingsPage::Sessions => vec![model.clone()],
            _ => Vec::new(),
        };
        let on_list = list_focus.is_focused(window);
        match ev.keystroke.key.as_str() {
            "up" if on_list => self.set_settings_page(self.settings_page.prev(), cx),
            "down" if on_list => self.set_settings_page(self.settings_page.next(), cx),
            "enter" if on_list => self.submit_modal(window, cx),
            "tab" => {
                let here = if on_list {
                    Some(0)
                } else {
                    fields.iter().position(|f| f.read(cx).focus_handle(cx).is_focused(window)).map(|i| i + 1)
                };
                let Some(here) = here else { return };
                let stops = fields.len() + 1;
                let there = if ev.keystroke.modifiers.shift { (here + stops - 1) % stops } else { (here + 1) % stops };
                match there.checked_sub(1) {
                    None => window.focus(&list_focus, cx),
                    Some(i) => fields[i].update(cx, |s, cx| s.focus(window, cx)),
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::SettingsPage::{self, *};

    #[test]
    fn next_and_prev_walk_the_list_and_stop_at_the_ends() {
        assert_eq!(General.next(), Sessions);
        assert_eq!(ClaudiuTools.next(), Worktrees);
        assert_eq!(Worktrees.next(), About);
        assert_eq!(About.next(), About);
        assert_eq!(About.prev(), Worktrees);
        assert_eq!(Sessions.prev(), General);
        assert_eq!(General.prev(), General);
    }

    #[test]
    fn opens_on_general() {
        assert_eq!(SettingsPage::default(), General);
    }
}
