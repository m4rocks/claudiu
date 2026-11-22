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
    /// No longer used (the "Other sessions" group is gone); still written because older m4code versions require it.
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
    /// The right sidebar's cards top to bottom, as the user dragged them; `right_card_order` fills in the rest.
    #[serde(default)]
    pub right_cards: Vec<SideCard>,
}

/// One card of the right sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideCard {
    Account,
    Actions,
    Github,
    Subagents,
    Linked,
    Status,
    /// A blank card, kept in the layout for something later.
    Empty,
    /// A card this version doesn't know, from a newer state file; it keeps the rest of the file loading.
    #[serde(other)]
    Unknown,
}

impl SideCard {
    /// Every card in its default place.
    pub const ALL: [SideCard; 7] = [Self::Account, Self::Actions, Self::Github, Self::Subagents, Self::Linked, Self::Status, Self::Empty];
}

impl Layout {
    /// The cards in the order the user left them, then any the saved order doesn't name (every card, in a state file
    /// from before they could move) in their default order.
    pub fn right_card_order(&self) -> Vec<SideCard> {
        let mut order: Vec<SideCard> = vec![];
        for card in self.right_cards.iter().chain(SideCard::ALL.iter()) {
            if *card != SideCard::Unknown && !order.contains(card) {
                order.push(*card);
            }
        }
        order
    }
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
    /// Parts of m4code's own MCP server switched off for new sessions (`claude/mcp.rs`). Empty means everything is on,
    /// which is also what a state file from before this setting loads as.
    #[serde(default, alias = "claudiu_tools_off")]
    pub m4code_tools_off: Vec<crate::claude::mcp::Feature>,
    /// The trims an earlier version wrote into Claude Code's own `settings.json`, kept only until
    /// `take_back_global_trims` has taken them out again; sessions now get `trim::FLUFF` themselves.
    #[serde(default)]
    pub trimmed_tools: Vec<crate::claude::trim::Trim>,
    /// The lines m4code added to Claude Code's `settings.json` (old trims and the skill limits), so it removes only them.
    #[serde(default)]
    pub applied_trims: Vec<String>,
    /// Output caps and turned-off skills for Claude Code (`claude/trim.rs`); empty leaves Claude Code's own behavior.
    #[serde(default)]
    pub limits: crate::claude::trim::Limits,
    /// Agent types that run a task in another coding agent (Settings → Harnesses); Antigravity is opt-in.
    #[serde(default)]
    pub custom_subagents: crate::claude::subagents::CustomSubagents,
    #[serde(default)]
    pub claude_subagents: crate::harness::SubagentAccess,
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
    /// What a message sent while the agent works does (Settings → General).
    #[serde(default)]
    pub follow_up: FollowUp,
}

/// What a message sent while the agent works does. Cmd/Ctrl+Enter does the other one for a single message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUp {
    /// Wait for the turn to end.
    Queue,
    /// Reach the agent in the turn it is working on: Claude Code reads it between tool calls; an agent that can't
    /// stops its turn and starts again with the message.
    #[default]
    Steer,
}

impl FollowUp {
    pub fn other(self) -> Self {
        match self {
            Self::Queue => Self::Steer,
            Self::Steer => Self::Queue,
        }
    }
}

impl Settings {
    pub fn subagent_access(&self, harness: crate::harness::Harness) -> crate::harness::SubagentAccess {
        match harness.agent() {
            None => self.claude_subagents,
            Some(agent) => {
                let choice = self.custom_subagents.get(agent);
                crate::harness::SubagentAccess { on: choice.on, edits: choice.edits }
            }
        }
    }

    fn harness_key(harness: crate::harness::Harness) -> &'static str {
        harness.key()
    }

    /// Permission modes `(id, name)` available for `harness`: the live list reported by its ACP adapter, or the
    /// adapter's fallback defaults until it answers.
    pub fn modes_for(&self, harness: crate::harness::Harness) -> Vec<(String, String)> {
        if let Some(modes) = self.harness_modes.get(Self::harness_key(harness))
            && !modes.is_empty()
        {
            return modes.clone();
        }
        (harness.adapter().default_modes)()
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
                out.extend(keep_picked((harness.adapter().default_models)(), self.picked_models(harness), |model| &model.id));
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
        let enabled = self.enabled_harnesses_list();
        let all = self.aggregated_models_for(&enabled);
        let (preferred, model_id) = if enabled.contains(&self.harness) {
            (self.harness, self.new_session.model.as_deref())
        } else {
            (enabled.first().copied().unwrap_or(self.harness), None)
        };
        Self::resolve_model(&all, preferred, model_id)
    }
    /// Harnesses enabled by the user (Settings → Harnesses). Defaults to all supported harnesses equally when none
    /// have been explicitly stored yet.
    pub fn enabled_harnesses_list(&self) -> Vec<crate::harness::Harness> {
        use crate::harness::Harness;
        if let Some(list) = &self.enabled_harnesses {
            return Harness::ALL.into_iter().filter(|h| list.contains(h)).collect();
        }
        Harness::ALL.to_vec()
    }

    pub fn harness_enabled(&self, harness: crate::harness::Harness) -> bool {
        self.enabled_harnesses_list().contains(&harness)
    }
}


#[cfg(test)]
mod tests {
    use super::{FollowUp, Layout, SideCard};

    #[test]
    fn the_cards_keep_the_users_order_and_gain_the_ones_it_misses() {
        assert_eq!(Layout::default().right_card_order(), SideCard::ALL, "an older state file");
        let moved: Layout = serde_json::from_str(r#"{"selected_session":null,"sidebar_visible":true,"other_collapsed":false,"right_cards":["status","from_the_future","actions","status"]}"#).unwrap();
        assert_eq!(
            moved.right_card_order(),
            [SideCard::Status, SideCard::Actions, SideCard::Account, SideCard::Github, SideCard::Subagents, SideCard::Linked, SideCard::Empty]
        );
    }

    #[test]
    fn messages_mid_turn_steer_unless_the_user_chose_to_queue() {
        let older: super::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(older.follow_up, FollowUp::Steer, "a state file from before the setting steers, as t3code does");
        let queue: super::Settings = serde_json::from_str(r#"{"follow_up":"queue"}"#).unwrap();
        assert_eq!(queue.follow_up, FollowUp::Queue);
        assert_eq!(FollowUp::Queue.other(), FollowUp::Steer);
        assert_eq!(FollowUp::Steer.other(), FollowUp::Queue);
    }
}
