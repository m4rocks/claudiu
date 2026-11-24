//! The pages of the Settings dialog and its keyboard handling.

use super::*;
use gpui::KeyDownEvent;

/// A category in the Settings dialog's sidebar, in display order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    #[default]
    General,
    Harnesses,
    Skills,
    M4codeTools,
    Projects,
    Keyboard,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 6] = [Self::General, Self::Harnesses, Self::Skills, Self::M4codeTools, Self::Projects, Self::Keyboard];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Harnesses => "Harnesses",
            Self::Skills => "Skills",
            Self::M4codeTools => "m4code tools",
            Self::Projects => "Projects",
            Self::Keyboard => "Keyboard",
        }
    }

    pub fn icon(self) -> gpui_kit::assets::IconName {
        use gpui_kit::assets::IconName;
        match self {
            Self::General => IconName::Settings,
            Self::Harnesses => IconName::Terminal,
            Self::Skills => IconName::FileCode,
            Self::M4codeTools => IconName::Brackets,
            Self::Projects => IconName::FolderOpen,
            Self::Keyboard => IconName::Keyboard,
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::General => "Defaults for new sessions, interface scale, shell, and updates.",
            Self::Harnesses => "Link coding agents to aggregate their models into one picker and show usage limits.",
            Self::Skills => "Install SKILL.md packages from skills.sh and sync them across active harnesses.",
            Self::M4codeTools => "Built-in MCP tools exposed to sessions. Applies to new sessions and after compaction.",
            Self::Projects => "Manage projects in the sidebar and git worktrees created by m4code.",
            Self::Keyboard => "Every keyboard shortcut m4code has. They can't be changed yet.",
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

/// One harness's background model lookup.
#[derive(Default)]
pub struct Lookup {
    pub loading: bool,
    /// True once a lookup succeeded in this run so opening a dropdown doesn't re-spawn the adapter every click.
    pub fetched: bool,
    /// Why the last lookup failed.
    pub error: Option<String>,
}

pub type ModelLister = fn(Harness, &std::path::Path, &[(String, String)]) -> Result<crate::harness::HarnessCatalog, String>;

/// Background model & mode lookups across every harness (Claude Code, Codex, OpenCode, and Antigravity).
pub struct ModelLookups {
    harnesses: crate::harness::HarnessMap<Lookup>,
    /// How the workspace asks a harness for its models and modes; tests can replace this without a real adapter.
    pub list: ModelLister,
}

impl Default for ModelLookups {
    fn default() -> Self {
        Self { harnesses: Default::default(), list: crate::harness::probe }
    }
}

impl ModelLookups {
    pub fn get(&self, harness: Harness) -> &Lookup {
        self.harnesses.get(harness)
    }

    pub fn get_mut(&mut self, harness: Harness) -> &mut Lookup {
        self.harnesses.get_mut(harness)
    }

    pub fn any_loading(&self, enabled: &[Harness]) -> bool {
        enabled.iter().any(|&h| self.harnesses.get(h).loading)
    }
}

impl Workspace {
    /// Earlier versions wrote the tool trims into Claude Code's own settings.json, so they reached Claude Code run in a
    /// terminal too. Each session now gets them itself (`trim::session_settings`), so take back out what was written.
    /// When the file can't be changed this runs again at the next start.
    pub(super) fn take_back_global_trims(&mut self) {
        let s = &mut self.store.data.settings;
        if s.trimmed_tools.is_empty() {
            return;
        }
        if let Ok(owned) = claude::trim::write_global_limits(&s.limits, &s.applied_trims) {
            s.applied_trims = owned;
            s.trimmed_tools.clear();
            self.store.mark_dirty();
        }
    }

    /// Environment passed to `harness`'s ACP server when probing its available models.
    pub(super) fn harness_model_env(&self, harness: Harness) -> Result<Vec<(String, String)>, String> {
        if harness != Harness::Claude {
            return Ok(vec![]);
        }
        let Some(exe) = self.claude_exe.clone() else {
            return Err("Could not find the `claude` CLI on PATH.".into());
        };
        Ok(harness::claude::probe_env(&exe))
    }

    /// Refreshes model catalogs in the background for every enabled harness (Claude Code, Codex, OpenCode,
    /// Antigravity) and any enabled subagent that hasn't loaded yet in this run.
    pub(crate) fn load_enabled_models(&mut self, cx: &mut Context<Self>) {
        let enabled = self.store.data.settings.enabled_harnesses_list();
        let wanted: Vec<Harness> = Harness::ALL
            .into_iter()
            .filter(|&h| {
                let subagent_on = h.agent().is_some_and(|a| self.store.data.settings.custom_subagents.get(a).on);
                let lookup = self.model_lookups.get(h);
                (enabled.contains(&h) || subagent_on) && !lookup.loading && !lookup.fetched
            })
            .collect();
        self.fetch_harness_models(wanted, cx);
    }

    /// Asks each harness switched on in Settings (as a harness or subagent) for its models in the background while
    /// General or Harnesses shows.
    pub(crate) fn load_models(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.settings_page, SettingsPage::General | SettingsPage::Harnesses) {
            return;
        }
        let Some(Modal::Settings { subagents, .. }) = &self.modal else { return };
        let wanted: Vec<Harness> = Harness::ALL
            .into_iter()
            .filter(|&h| {
                let subagent_on = h.agent().is_some_and(|a| subagents.get(a).on);
                let lookup = self.model_lookups.get(h);
                (subagents.enabled.contains(&h) || subagent_on) && !lookup.loading && (!lookup.fetched || lookup.error.is_some())
            })
            .collect();
        self.fetch_harness_models(wanted, cx);
    }

    fn fetch_harness_models(&mut self, wanted: Vec<Harness>, cx: &mut Context<Self>) {
        for harness in wanted {
            let env = match self.harness_model_env(harness) {
                Ok(env) => env,
                Err(why) => {
                    self.model_lookups.get_mut(harness).error = Some(why);
                    cx.notify();
                    continue;
                }
            };
            self.model_lookups.get_mut(harness).loading = true;
            let list = self.model_lookups.list;
            let folder = crate::datadir::data_dir();
            self.in_background(
                move || list(harness, &folder, &env),
                move |ws, found, cx| {
                    let lookup = ws.model_lookups.get_mut(harness);
                    lookup.loading = false;
                    match found {
                        Ok(catalog) => {
                            lookup.fetched = true;
                            lookup.error = None;
                            let models_changed = ws.store.data.settings.set_harness_models(harness, catalog.models);
                            let modes_changed = ws.store.data.settings.set_harness_modes(harness, catalog.modes);
                            if models_changed || modes_changed {
                                ws.store.mark_dirty();
                                ws.push_models_to_threads(cx);
                            }
                        }
                        Err(why) => lookup.error = Some(why),
                    }
                },
                cx,
            );
        }
    }

    /// Keep every open chat thread's aggregated model list up to date.
    pub(crate) fn push_models_to_threads(&self, cx: &mut Context<Self>) {
        let settings = &self.store.data.settings;
        let models = settings.aggregated_models();
        for live in &self.live {
            if let Some(thread) = live.view.thread() {
                thread.update(cx, |t, cx| {
                    t.all_models = models.clone();
                    t.picked = settings.picked_models(t.harness).to_vec();
                    cx.notify();
                });
            }
        }
    }

    /// Opens a harness's configuration popup over the Harnesses page.
    pub(crate) fn open_harness_dialog(&mut self, harness: Harness, window: &mut Window, cx: &mut Context<Self>) {
        let search = self.modal_input_then("", "Search models", Self::close_harness_dialog, window, cx);
        if let Some(Modal::Settings { subagents, .. }) = self.modal.as_mut() {
            subagents.picking = Some(HarnessDialog { harness, search });
        }
        self.load_models(cx);
    }

    /// Closes the harness popup, keeping what was switched; Settings' Save persists it.
    pub(crate) fn close_harness_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Modal::Settings { subagents, list_focus, .. }) = self.modal.as_mut() {
            subagents.picking = None;
            window.focus(list_focus, cx);
        }
    }

    pub(crate) fn harness_dialog_open(&self) -> bool {
        matches!(&self.modal, Some(Modal::Settings { subagents, .. }) if subagents.picking.is_some())
    }

    /// Keys for the open Settings dialog, seen before the focused control. Up and Down change the page and Enter saves
    /// only while the category list has focus; Tab and Shift+Tab cycle the list and the current page's text fields.
    pub(crate) fn settings_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The popup over Harnesses closes on Escape without closing Settings itself.
        if self.harness_dialog_open() {
            if ev.keystroke.key == "escape" {
                self.close_harness_dialog(window, cx);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        // Escape closes any active universal picker over Settings without closing Settings itself.
        if ev.keystroke.key == "escape" && self.picker.is_some() {
            self.dismiss_picker(window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let Some(Modal::Settings { shell, skill_search, skill_filter, list_focus, renaming, .. }) = &self.modal else { return };
        let list_focus = list_focus.clone();
        let fields = match self.settings_page {
            SettingsPage::General => vec![shell.clone()],
            SettingsPage::Skills => vec![match self.skills_page.tab {
                SkillsTab::Installed => skill_filter.clone(),
                SkillsTab::Browse => skill_search.clone(),
            }],
            SettingsPage::Projects => renaming.iter().map(|(_, name)| name.clone()).collect(),
            _ => Vec::new(),
        };
        let on_list = list_focus.is_focused(window);
        if self.settings_page == SettingsPage::Skills && self.picker.is_none()
            && matches!(ev.keystroke.key.as_str(), "pageup" | "pagedown")
            && ev.keystroke.modifiers.control
        {
            let tab = if ev.keystroke.key == "pageup" { SkillsTab::Installed } else { SkillsTab::Browse };
            self.act(Act::SetSkillsTab(tab), window, cx);
            if let Some(Modal::Settings { skill_search, skill_filter, .. }) = &self.modal {
                let input = if tab == SkillsTab::Installed { skill_filter } else { skill_search };
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            cx.stop_propagation();
            return;
        }
        match ev.keystroke.key.as_str() {
            "up" if on_list => self.set_settings_page(self.settings_page.prev(), cx),
            "down" if on_list => self.set_settings_page(self.settings_page.next(), cx),
            "enter" if on_list => self.submit_modal(window, cx),
            "tab" => {
                let here = if on_list {
                    Some(0)
                } else {
                    fields.iter().position(|f| f.focus_handle(cx).is_focused(window)).map(|i| i + 1)
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
        assert_eq!(General.next(), Harnesses);
        assert_eq!(Harnesses.next(), Skills);
        assert_eq!(Skills.next(), M4codeTools);
        assert_eq!(M4codeTools.next(), Projects);
        assert_eq!(Projects.next(), Keyboard);
        assert_eq!(Keyboard.next(), Keyboard);
        assert_eq!(Keyboard.prev(), Projects);
        assert_eq!(Projects.prev(), M4codeTools);
        assert_eq!(Harnesses.prev(), General);
        assert_eq!(General.prev(), General);
    }

    #[test]
    fn opens_on_general() {
        assert_eq!(SettingsPage::default(), General);
    }
}
