//! The pages of the Settings dialog and its keyboard handling.

use super::*;
use crate::terminal::keys::Keystroke;

/// A category in the Settings dialog's list, in display order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    #[default]
    General,
    Sessions,
    Projects,
    ClaudeTools,
    ClaudiuTools,
    Worktrees,
    About,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 7] = [Self::General, Self::Sessions, Self::Projects, Self::ClaudeTools, Self::ClaudiuTools, Self::Worktrees, Self::About];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Sessions => "Sessions",
            Self::Projects => "Projects",
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
    /// Keys for the open Settings dialog that no text field took. Up and Down change the page and Enter saves while no
    /// field has the keyboard (the category list has it); Tab and Shift+Tab cycle the list and the page's text fields.
    pub(crate) fn settings_key(&mut self, ks: &Keystroke) -> bool {
        let fields: &[usize] = match self.settings_page {
            SettingsPage::General => &[0, 1],
            SettingsPage::Sessions => &[2],
            SettingsPage::ClaudeTools => &[3],
            SettingsPage::Projects if modal_field(&mut self.modal, 4).is_some() => &[4],
            _ => &[],
        };
        let focused = (0..5).find(|n| modal_field(&mut self.modal, *n).is_some_and(|f| f.focused));
        let on_list = focused.is_none();
        let plain = !ks.modifiers.control && !ks.modifiers.alt && !ks.modifiers.platform;
        match ks.key.as_str() {
            "up" if on_list && plain => self.set_settings_page(self.settings_page.prev()),
            "down" if on_list && plain => self.set_settings_page(self.settings_page.next()),
            "tab" if plain => {
                // Stop 0 is the list, then the page's fields in order.
                let here = focused.and_then(|f| fields.iter().position(|x| *x == f)).map_or(0, |i| i + 1);
                let stops = fields.len() + 1;
                let there = if ks.modifiers.shift { (here + stops - 1) % stops } else { (here + 1) % stops };
                for n in 0..5 {
                    if let Some(f) = modal_field(&mut self.modal, n) {
                        f.focused = there.checked_sub(1).map(|i| fields[i]) == Some(n);
                    }
                }
                if there == 0 {
                    self.blur = true;
                }
            }
            _ => return false,
        }
        true
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
