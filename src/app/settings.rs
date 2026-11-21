//! The pages of the Settings dialog and its keyboard handling.

use super::*;
use crate::claude::subagents::Agent;
use gpui::KeyDownEvent;

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
    agents: claude::subagents::AgentMap<Lookup>,
    /// How the page asks; tests answer without a real agent.
    pub list: fn(Agent, &std::path::Path) -> Result<Vec<claude::subagents::AgentModel>, String>,
}

impl Default for ModelLookups {
    fn default() -> Self {
        Self { agents: Default::default(), list: claude::acp::models }
    }
}

impl ModelLookups {
    pub fn get(&self, agent: Agent) -> &Lookup {
        self.agents.get(agent)
    }

    fn get_mut(&mut self, agent: Agent) -> &mut Lookup {
        self.agents.get_mut(agent)
    }
}

impl Workspace {
    /// Asks each agent switched on for its models in the background while the Custom subagents page shows. The list
    /// the page showed last stays until the answer comes; a failure keeps it and says why.
    pub(crate) fn load_models(&mut self, cx: &mut Context<Self>) {
        if self.settings_page != SettingsPage::CustomSubagents {
            return;
        }
        let Some(Modal::Settings { subagents, .. }) = &self.modal else { return };
        let wanted: Vec<Agent> = Agent::ALL.into_iter().filter(|a| subagents.get(*a).on && !self.model_lookups.get(*a).loading).collect();
        for agent in wanted {
            self.model_lookups.get_mut(agent).loading = true;
            let list = self.model_lookups.list;
            let folder = crate::datadir::data_dir();
            self.in_background(
                move || list(agent, &folder),
                move |ws, found, _| {
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
                cx,
            );
        }
    }

    /// Opens an agent's dialog over the Custom subagents page, with the keyboard in its model search.
    pub(crate) fn open_agent_dialog(&mut self, agent: Agent, window: &mut Window, cx: &mut Context<Self>) {
        let search = self.modal_input_then("", "Search models", Self::close_agent_dialog, window, cx);
        if let Some(Modal::Settings { subagents, .. }) = self.modal.as_mut() {
            subagents.picking = Some((agent, search));
        }
    }

    /// Closes the agent's dialog, keeping what it switched; Settings' Save keeps it for good.
    pub(crate) fn close_agent_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Modal::Settings { subagents, list_focus, .. }) = self.modal.as_mut() {
            subagents.picking = None;
            window.focus(list_focus, cx);
        }
    }

    pub(crate) fn agent_dialog_open(&self) -> bool {
        matches!(&self.modal, Some(Modal::Settings { subagents: SubagentsForm { picking: Some(_), .. }, .. }))
    }

    /// Keys for the open Settings dialog, seen before the focused control. Up and Down change the page and Enter saves
    /// only while the category list has focus; Tab and Shift+Tab cycle the list and the current page's text fields.
    pub(crate) fn settings_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The dialog over the page has only its search field, and no list of pages for Up and Down to move.
        if self.agent_dialog_open() {
            if ev.keystroke.key == "escape" {
                self.close_agent_dialog(window, cx);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        let Some(Modal::Settings { shell, claude, model, skills_field, list_focus, renaming, .. }) = &self.modal else { return };
        let list_focus = list_focus.clone();
        let fields = match self.settings_page {
            SettingsPage::General => vec![shell.clone(), claude.clone()],
            SettingsPage::Sessions => vec![model.clone()],
            SettingsPage::ClaudeTools => vec![skills_field.clone()],
            SettingsPage::Projects => renaming.iter().map(|(_, name)| name.clone()).collect(),
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
