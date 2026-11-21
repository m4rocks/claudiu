//! The Skills page of Settings (`skills.sh` browser, installer, global and per-project toggles, and harness sync).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::*;
use crate::skills::registry::CatalogEntry;
use crate::skills::{InstalledSkill, active_harnesses, skills_root, sync_all, uninstall};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkillsTab {
    #[default]
    Installed,
    Browse,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SkillScope {
    #[default]
    Global,
    Project(Id),
}

/// Ephemeral UI state for Settings → Skills. Not saved to disk.
pub struct SkillsPage {
    pub tab: SkillsTab,
    pub scope: SkillScope,
    /// Popular skills from `skills.sh`; `None` until first load completes.
    pub popular: Option<Vec<CatalogEntry>>,
    /// Active search query and its results from `skills.sh` or GitHub.
    pub results: Option<(String, Vec<CatalogEntry>)>,
    pub loading: bool,
    pub error: Option<String>,
    /// Keys (`source/skill_id`) currently being downloaded.
    pub installing: HashSet<String>,
    /// Injectable loaders so headless tests run without touching the network.
    pub fetch_popular: fn() -> Result<Vec<CatalogEntry>, String>,
    pub fetch_search: fn(&str) -> Result<Vec<CatalogEntry>, String>,
    pub install_pkg: fn(&Path, &CatalogEntry) -> Result<InstalledSkill, String>,
    pub install_repo_pkg: fn(&Path, &str) -> Result<Vec<InstalledSkill>, String>,
    /// Override for the user's home directory; tests use a tempdir so global symlinks stay isolated.
    #[cfg(test)]
    pub home_override: Option<PathBuf>,
}

impl Default for SkillsPage {
    fn default() -> Self {
        Self {
            tab: SkillsTab::default(),
            scope: SkillScope::default(),
            popular: None,
            results: None,
            loading: false,
            error: None,
            installing: HashSet::new(),
            fetch_popular: crate::skills::registry::popular,
            fetch_search: crate::skills::registry::search,
            install_pkg: crate::skills::registry::install_from_github,
            install_repo_pkg: crate::skills::registry::install_repo_from_github,
            #[cfg(test)]
            home_override: None,
        }
    }
}

impl Workspace {
    fn skills_home(&self) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(dir) = &self.skills_page.home_override {
            return Some(dir.clone());
        }
        platform::home_dir()
    }

    /// Active harnesses on this machine, given current settings and an optional harness being started right now.
    pub(crate) fn active_skill_harnesses(&self, extra: Option<Harness>) -> Vec<Harness> {
        let enabled = self.store.data.settings.enabled_harnesses_list();
        let Some(home) = self.skills_home() else {
            return enabled;
        };
        active_harnesses(&enabled, self.store.data.settings.harness, &self.store.data.settings.custom_subagents, &home, extra)
    }

    /// Fetch the popular `skills.sh` catalog once when the Skills page opens.
    pub(crate) fn load_skills_catalog(&mut self, cx: &mut Context<Self>) {
        if self.skills_page.popular.is_some() || self.skills_page.loading {
            return;
        }
        self.skills_page.loading = true;
        self.skills_page.error = None;
        let fetch = self.skills_page.fetch_popular;
        self.in_background(
            fetch,
            |ws, result, _| {
                ws.skills_page.loading = false;
                match result {
                    Ok(entries) => {
                        ws.skills_page.popular = Some(entries);
                        ws.skills_page.error = None;
                    }
                    Err(why) => ws.skills_page.error = Some(why),
                }
            },
            cx,
        );
    }

    /// Search `skills.sh` (or list a GitHub `owner/repo`) using the current search field text.
    pub(crate) fn search_skills(&mut self, cx: &mut Context<Self>) {
        let query = match &self.modal {
            Some(Modal::Settings { skill_search, .. }) => skill_search.read(cx).value().trim().to_string(),
            _ => return,
        };
        if query.is_empty() {
            self.skills_page.results = None;
            self.skills_page.error = None;
            if self.skills_page.popular.is_none() {
                self.load_skills_catalog(cx);
            }
            cx.notify();
            return;
        }
        self.skills_page.tab = SkillsTab::Browse;
        self.skills_page.loading = true;
        self.skills_page.error = None;
        let fetch = self.skills_page.fetch_search;
        let q = query.clone();
        self.in_background(
            move || fetch(&q),
            move |ws, result, _| {
                ws.skills_page.loading = false;
                match result {
                    Ok(entries) => {
                        ws.skills_page.results = Some((query, entries));
                        ws.skills_page.error = None;
                    }
                    Err(why) => ws.skills_page.error = Some(why),
                }
            },
            cx,
        );
    }

    /// Download `entry` into Claudiu's skills store, enable it in the currently selected scope, and sync symlinks.
    pub(crate) fn install_skill(&mut self, entry: CatalogEntry, cx: &mut Context<Self>) {
        let key = entry.key();
        if !self.skills_page.installing.insert(key.clone()) {
            return;
        }
        let scope = self.skills_page.scope.clone();
        let install_fn = self.skills_page.install_pkg;
        let root = skills_root();
        cx.notify();
        self.in_background(
            move || install_fn(&root, &entry),
            move |ws, result, cx| {
                ws.skills_page.installing.remove(&key);
                match result {
                    Ok(skill) => {
                        let name = skill.name.clone();
                        let project_target = match &scope {
                            SkillScope::Project(id) if ws.store.project(id).is_some() => Some(id.clone()),
                            _ => None,
                        };
                        let skills = &mut ws.store.data.settings.skills;
                        skills.upsert(skill);
                        match project_target {
                            Some(id) => skills.set_project(&id, &name, true),
                            None => skills.set_global(&name, true),
                        }
                        ws.store.mark_dirty();
                        ws.sync_skills(None, cx);
                        ws.toast(format!("Installed skill “{name}”."), false, cx);
                    }
                    Err(why) => ws.toast(format!("Couldn't install skill: {why}"), true, cx),
                }
            },
            cx,
        );
    }

    /// Download every `SKILL.md` package from a GitHub `owner/repo`, enable them in the selected scope, and sync symlinks.
    pub(crate) fn install_skill_repo(&mut self, source: String, cx: &mut Context<Self>) {
        let key = format!("repo:{source}");
        if !self.skills_page.installing.insert(key.clone()) {
            return;
        }
        let scope = self.skills_page.scope.clone();
        let install_fn = self.skills_page.install_repo_pkg;
        let root = skills_root();
        let repo = source.clone();
        cx.notify();
        self.in_background(
            move || install_fn(&root, &repo),
            move |ws, result, cx| {
                ws.skills_page.installing.remove(&key);
                match result {
                    Ok(list) => {
                        let count = list.len();
                        let project_target = match &scope {
                            SkillScope::Project(id) if ws.store.project(id).is_some() => Some(id.clone()),
                            _ => None,
                        };
                        let skills = &mut ws.store.data.settings.skills;
                        for skill in list {
                            let name = skill.name.clone();
                            skills.upsert(skill);
                            match &project_target {
                                Some(id) => skills.set_project(id, &name, true),
                                None => skills.set_global(&name, true),
                            }
                        }
                        ws.store.mark_dirty();
                        ws.sync_skills(None, cx);
                        let noun = if count == 1 { "skill" } else { "skills" };
                        ws.toast(format!("Installed {count} {noun} from {source}."), false, cx);
                    }
                    Err(why) => ws.toast(format!("Couldn't install {source}: {why}"), true, cx),
                }
            },
            cx,
        );
    }

    /// Flip a skill's toggle in the selected scope (Global or a specific project) and re-sync symlinks.
    pub(crate) fn toggle_skill(&mut self, name: String, cx: &mut Context<Self>) {
        let scope = self.skills_page.scope.clone();
        let skills = &mut self.store.data.settings.skills;
        match scope {
            SkillScope::Global => {
                skills.toggle_global(&name);
            }
            SkillScope::Project(id) => {
                skills.toggle_project(&id, &name);
            }
        }
        self.store.mark_dirty();
        self.store.save_if_dirty();
        self.sync_skills(None, cx);
        cx.notify();
    }

    /// Remove an installed skill, its symlinks across all harnesses and projects, and its stored files.
    pub(crate) fn uninstall_skill(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.store.data.settings.skills.remove(&name) {
            return;
        }
        self.store.mark_dirty();
        self.store.save_if_dirty();
        let root = skills_root();
        let home = self.skills_home();
        let projects: Vec<PathBuf> = self.store.data.projects.iter().map(|p| p.path.clone()).collect();
        let removed = name.clone();
        self.in_background(
            move || {
                let Some(home) = home else {
                    return Err(std::io::Error::other("home directory not found"));
                };
                uninstall(&root, &home, &removed, &projects)
            },
            move |ws, result, cx| match result {
                Ok(()) => ws.toast(format!("Removed skill “{name}”."), false, cx),
                Err(e) => ws.toast(format!("Removed “{name}” from settings, but cleanup failed: {e}"), true, cx),
            },
            cx,
        );
    }

    /// Reconcile symlinks for every active harness (global and per-project) off the UI thread.
    pub(crate) fn sync_skills(&mut self, extra_harness: Option<Harness>, cx: &mut Context<Self>) {
        let Some(home) = self.skills_home() else { return };
        let config = self.store.data.settings.skills.clone();
        // Skip spawning background work when no skills are installed and no toggles exist.
        if config.installed.is_empty() && config.global_enabled.is_empty() && config.project_enabled.is_empty() {
            return;
        }
        let active = self.active_skill_harnesses(extra_harness);
        let root = skills_root();
        let projects: Vec<(String, PathBuf)> = self.store.data.projects.iter().map(|p| (p.id.clone(), p.path.clone())).collect();
        self.in_background(
            move || sync_all(&root, &home, &active, &config, &projects),
            |ws, errors, cx| {
                if let Some(first) = errors.first() {
                    ws.toast(format!("Couldn't sync skills: {first}"), true, cx);
                }
            },
            cx,
        );
    }
}
