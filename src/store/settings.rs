use super::*;

/// Where the window was last left, in logical pixels. `w`/`h` are the restore size while `maximized`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub maximized: bool,
}

impl WindowGeom {
    /// Which of the `displays` (x, y, w, h) holds the window's center. A monitor that was unplugged or rearranged since
    /// would otherwise open the window out of reach.
    pub fn display_of(&self, displays: &[(f32, f32, f32, f32)]) -> Option<usize> {
        let (cx, cy) = (self.x + self.w / 2.0, self.y + self.h / 2.0);
        displays.iter().position(|&(x, y, w, h)| cx >= x && cx < x + w && cy >= y && cy < y + h)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// Last window size and position; the default centered window when unset.
    #[serde(default)]
    pub window: Option<WindowGeom>,
    pub selected_session: Option<Id>,
    pub sidebar_visible: bool,
    /// No longer used (the "Other sessions" group is gone); still written because older Claudiu versions require it.
    pub other_collapsed: bool,
    /// The sidebar's RECENT list is open. It starts closed on every launch, so it isn't saved.
    #[serde(skip)]
    pub recent_open: bool,
    /// The project the sidebar list is filtered to.
    #[serde(default)]
    pub project_filter: Option<Id>,
    /// The right sidebar is hidden (stored negated so older state files keep it on).
    #[serde(default)]
    pub right_hidden: bool,
    /// Width of the right sidebar while it shows GitHub; the menu keeps its own, `right_menu_w`.
    #[serde(default)]
    pub right_gh_w: Option<f32>,
    /// Width of the left sidebar when the user has dragged it; the default otherwise.
    #[serde(default)]
    pub left_w: Option<f32>,
    /// Width of the right sidebar's menu when the user has dragged it; the default otherwise.
    #[serde(default)]
    pub right_menu_w: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Explicit path to the `claude` executable; discovery is used when empty.
    pub claude_path: Option<PathBuf>,
    /// Program for new terminal sessions (a path or a name on PATH); the platform default shell when empty.
    #[serde(default)]
    pub shell: Option<String>,
    /// Update feed base URL. Empty/None disables update checks entirely.
    pub update_url: Option<String>,
    /// Editor the "Open in" button uses; the last one picked.
    #[serde(default)]
    pub editor: Option<crate::platform::Editor>,
    /// UI scale in percent (text and terminal font); 100 when unset.
    #[serde(default)]
    pub scale: Option<u32>,
    /// Whether new Claude sessions start with Claude in Chrome (`--chrome`); each session can still switch it.
    #[serde(default)]
    pub chrome_default: bool,
    /// The harness new sessions run in.
    #[serde(default)]
    pub harness: crate::harness::Harness,
    /// Harnesses enabled under Settings → Harnesses. `None` (older state files) enables Claude Code plus the default
    /// harness and any harness previously enabled under Custom subagents.
    #[serde(default)]
    pub enabled_harnesses: Option<Vec<crate::harness::Harness>>,
    /// Model, effort and permission mode for new sessions; empty fields leave the harness's own default.
    #[serde(default)]
    pub new_session: crate::claude::defaults::NewSessionDefaults,
    /// Parts of Claudiu's own MCP server switched off for new sessions (`claude/mcp.rs`). Empty means everything is on,
    /// which is also what a state file from before this setting loads as.
    #[serde(default)]
    pub claudiu_tools_off: Vec<crate::claude::mcp::Feature>,
    /// The trims an earlier version wrote into Claude Code's own `settings.json`, kept only until
    /// `take_back_global_trims` has taken them out again; sessions now get `trim::FLUFF` themselves.
    #[serde(default)]
    pub trimmed_tools: Vec<crate::claude::trim::Trim>,
    /// The lines Claudiu added to Claude Code's `settings.json` (old trims and the skill limits), so it removes only them.
    #[serde(default)]
    pub applied_trims: Vec<String>,
    /// Output caps and turned-off skills for Claude Code (`claude/trim.rs`); empty leaves Claude Code's own behavior.
    #[serde(default)]
    pub limits: crate::claude::trim::Limits,
    /// Agent types that run a task in another coding agent (Settings → Harnesses); all off by default.
    #[serde(default)]
    pub custom_subagents: crate::claude::subagents::CustomSubagents,
    /// Last model list reported by Claude Code, used to populate the unified model picker before a session opens.
    #[serde(default)]
    pub claude_models: Vec<crate::claude::subagents::AgentModel>,
    /// The Claude Code models ticked in Settings → Harnesses; empty shows every model in the picker.
    #[serde(default)]
    pub claude_picked: Vec<String>,
    /// Last permission modes `(id, name)` reported by each harness's ACP adapter, keyed by harness serde name.
    #[serde(default)]
    pub harness_modes: BTreeMap<String, Vec<(String, String)>>,
    /// Installed `SKILL.md` packages and their global / per-project toggles (Settings → Skills).
    #[serde(default)]
    pub skills: crate::skills::SkillsConfig,
}

impl Settings {
    fn harness_key(harness: crate::harness::Harness) -> &'static str {
        harness.key()
    }

    /// Permission modes `(id, name)` available for `harness`: the live list reported by its ACP adapter, or sensible
    /// fallback defaults until its adapter answers.
    pub fn modes_for(&self, harness: crate::harness::Harness) -> Vec<(String, String)> {
        if let Some(modes) = self.harness_modes.get(Self::harness_key(harness))
            && !modes.is_empty()
        {
            return modes.clone();
        }
        match harness {
            crate::harness::Harness::Claude => crate::claude::defaults::PermissionMode::ALL
                .into_iter()
                .map(|m| (m.acp_id().to_string(), m.label().to_string()))
                .collect(),
            crate::harness::Harness::Codex | crate::harness::Harness::OpenCode | crate::harness::Harness::Antigravity => {
                vec![("default".into(), "Ask Before Editing".into()), ("fullAccess".into(), "Full Access".into())]
            }
        }
    }

    /// Update the cached permission modes for `harness`; returns `true` when the list actually changed.
    pub fn set_harness_modes(&mut self, harness: crate::harness::Harness, modes: Vec<(String, String)>) -> bool {
        if modes.is_empty() {
            return false;
        }
        let key = Self::harness_key(harness).to_string();
        if self.harness_modes.get(&key) == Some(&modes) {
            return false;
        }
        self.harness_modes.insert(key, modes);
        true
    }
    /// Cached model catalog for `harness` (empty until its adapter or a live session reports models).
    pub fn harness_models(&self, harness: crate::harness::Harness) -> &[crate::claude::subagents::AgentModel] {
        match harness.agent() {
            None => &self.claude_models,
            Some(agent) => &self.custom_subagents.get(agent).catalog,
        }
    }

    /// Update the cached model catalog for `harness`; returns `true` when the list actually changed.
    pub fn set_harness_models(&mut self, harness: crate::harness::Harness, models: Vec<crate::claude::subagents::AgentModel>) -> bool {
        let slot = match harness.agent() {
            None => &mut self.claude_models,
            Some(agent) => &mut self.custom_subagents.get_mut(agent).catalog,
        };
        if *slot == models {
            return false;
        }
        *slot = models;
        true
    }

    /// The models ticked for `harness` in Settings → Harnesses. For Codex, OpenCode and Antigravity the same ticks are
    /// the subagents Claude Code gets while "Allow use as subagents" is on.
    pub fn picked_models(&self, harness: crate::harness::Harness) -> &[String] {
        match harness.agent() {
            None => &self.claude_picked,
            Some(agent) => &self.custom_subagents.get(agent).models,
        }
    }

    /// All selectable models across every enabled harness, aggregated into one flat list.
    pub fn aggregated_models(&self) -> Vec<crate::harness::CatalogModel> {
        self.aggregated_models_for(&self.enabled_harnesses_list())
    }

    /// All selectable models across `enabled`, aggregated into one flat list.
    pub fn aggregated_models_for(&self, enabled: &[crate::harness::Harness]) -> Vec<crate::harness::CatalogModel> {
        use crate::harness::{CatalogModel, Harness};
        let mut out = Vec::new();
        for harness in Harness::ALL.into_iter().filter(|h| enabled.contains(h)) {
            let catalog = self.harness_models(harness);
            if catalog.is_empty() {
                match harness {
                    Harness::Claude => out.extend(CatalogModel::default_claude_models()),
                    Harness::Codex | Harness::OpenCode | Harness::Antigravity => {
                        out.push(CatalogModel { harness, id: String::new(), name: format!("{} (default)", harness.name()) });
                    }
                }
            } else {
                let all = catalog.iter().map(|m| CatalogModel { harness, id: m.id.clone(), name: m.name.clone() }).collect();
                out.extend(keep_picked(all, self.picked_models(harness), |m| &m.id));
            }
        }
        out
    }

    /// Resolve a `(harness, model_id)` pair against `models`, falling back to a model from `harness` or the first
    /// available model. Keeps an unknown `model_id` when its harness is present in `models`.
    pub fn resolve_model(models: &[crate::harness::CatalogModel], harness: crate::harness::Harness, model_id: Option<&str>) -> crate::harness::CatalogModel {
        use crate::harness::CatalogModel;
        if let Some(id) = model_id.filter(|id| !id.is_empty()) {
            if let Some(found) = models.iter().find(|m| m.harness == harness && m.id == id) {
                return found.clone();
            }
            if let Some(found) = models.iter().find(|m| m.id == id) {
                return found.clone();
            }
            if models.iter().any(|m| m.harness == harness) {
                return CatalogModel { harness, id: id.to_string(), name: id.to_string() };
            }
        }
        models
            .iter()
            .find(|m| m.harness == harness)
            .or_else(|| models.first())
            .cloned()
            .unwrap_or_else(|| CatalogModel { harness, id: String::new(), name: harness.name().into() })
    }

    /// Default model for new sessions (across all enabled harnesses).
    pub fn default_model(&self) -> crate::harness::CatalogModel {
        let all = self.aggregated_models();
        Self::resolve_model(&all, self.harness, self.new_session.model.as_deref())
    }
    /// Harnesses enabled by the user (Settings → Harnesses). Always includes at least the configured default harness
    /// when none have been explicitly stored yet.
    pub fn enabled_harnesses_list(&self) -> Vec<crate::harness::Harness> {
        use crate::claude::subagents::Agent;
        use crate::harness::Harness;
        if let Some(list) = &self.enabled_harnesses {
            return Harness::ALL.into_iter().filter(|h| list.contains(h)).collect();
        }
        Harness::ALL
            .into_iter()
            .filter(|&h| {
                h == Harness::Claude
                    || h == self.harness
                    || match h {
                        Harness::Claude => false,
                        Harness::Codex => self.custom_subagents.get(Agent::Codex).on,
                        Harness::OpenCode => self.custom_subagents.get(Agent::OpenCode).on,
                        Harness::Antigravity => self.custom_subagents.get(Agent::Antigravity).on,
                    }
            })
            .collect()
    }

    pub fn harness_enabled(&self, harness: crate::harness::Harness) -> bool {
        self.enabled_harnesses_list().contains(&harness)
    }
}

