//! The pages of the Settings dialog and its keyboard handling.

use super::*;
use crate::claude::subagents::Agent;
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
    CustomSubagents,
    Worktrees,
    About,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 8] = [Self::General, Self::Sessions, Self::Projects, Self::ClaudeTools, Self::ClaudiuTools, Self::CustomSubagents, Self::Worktrees, Self::About];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Sessions => "Sessions",
            Self::Projects => "Projects",
            Self::ClaudeTools => "Claude Code tools",
            Self::ClaudiuTools => "Claudiu tools",
            Self::CustomSubagents => "Custom subagents",
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

/// One agent's model lookup for the Custom subagents page.
#[derive(Default)]
pub struct Lookup {
    pub loading: bool,
    /// Why the last lookup failed.
    pub error: Option<String>,
}

/// The Custom subagents page's lookups of each agent's models.
pub struct ModelLookups {
    codex: Lookup,
    opencode: Lookup,
    /// How the page asks; tests answer without a real agent.
    pub list: fn(Agent, &std::path::Path) -> Result<Vec<claude::subagents::AgentModel>, String>,
}

impl Default for ModelLookups {
    fn default() -> Self {
        Self { codex: Lookup::default(), opencode: Lookup::default(), list: claude::acp::models }
    }
}

impl ModelLookups {
    pub fn get(&self, agent: Agent) -> &Lookup {
        match agent {
            Agent::Codex => &self.codex,
            Agent::OpenCode => &self.opencode,
        }
    }

    fn get_mut(&mut self, agent: Agent) -> &mut Lookup {
        match agent {
            Agent::Codex => &mut self.codex,
            Agent::OpenCode => &mut self.opencode,
        }
    }
}

impl Workspace {
    /// Asks each agent switched on for its models in the background while the Custom subagents page shows. The list
    /// the page showed last stays until the answer comes; a failure keeps it and says why.
    pub(crate) fn load_models(&mut self) {
        if self.settings_page != SettingsPage::CustomSubagents {
            return;
        }
        let Some(Modal::Settings { subagents, .. }) = &self.modal else { return };
        let wanted: Vec<Agent> = Agent::ALL.into_iter().filter(|a| subagents.get(*a).on && !self.model_lookups.get(*a).loading).collect();
        for agent in wanted {
            self.model_lookups.get_mut(agent).loading = true;
            let list = self.model_lookups.list;
            let folder = crate::datadir::data_dir();
            self.rt.spawn(
                move || list(agent, &folder),
                move |ws: &mut Workspace, found| {
                    let lookup = ws.model_lookups.get_mut(agent);
                    lookup.loading = false;
                    match found {
                        Ok(models) => {
                            lookup.error = None;
                            let catalog = &mut ws.store.data.settings.custom_subagents.get_mut(agent).catalog;
                            if *catalog != models {
                                *catalog = models;
                                ws.store.mark_dirty();
                            }
                        }
                        Err(why) => lookup.error = Some(why),
                    }
                },
            );
        }
    }

    /// Opens the model picker over the Custom subagents page, with the keyboard in its search field.
    pub(crate) fn open_model_picker(&mut self, agent: Agent) {
        if let Some(Modal::Settings { subagents, .. }) = self.modal.as_mut() {
            subagents.picking = Some((agent, super::modal::field("", true)));
        }
    }

    /// Closes the model picker, keeping what it switched; Settings' Save keeps it for good.
    pub(crate) fn close_model_picker(&mut self) {
        if let Some(Modal::Settings { subagents, .. }) = self.modal.as_mut() {
            subagents.picking = None;
        }
        self.blur = true;
    }

    pub(crate) fn model_picker_open(&self) -> bool {
        matches!(&self.modal, Some(Modal::Settings { subagents: SubagentsForm { picking: Some(_), .. }, .. }))
    }

    /// Keys for the open Settings dialog that no text field took. Up and Down change the page and Enter saves while no
    /// field has the keyboard (the category list has it); Tab and Shift+Tab cycle the list and the page's text fields.
    pub(crate) fn settings_key(&mut self, ks: &Keystroke) -> bool {
        // The picker over the page has only its search field, and no list of pages for Up and Down to move.
        if self.model_picker_open() {
            return false;
        }
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
        assert_eq!(ClaudiuTools.next(), CustomSubagents);
        assert_eq!(CustomSubagents.next(), Worktrees);
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
