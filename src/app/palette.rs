//! Command Palette (`Ctrl+K` / `Cmd+K`): fuzzy jump across sessions, worktrees, models, permissions, files, and actions.

use std::path::PathBuf;

use gpui_kit::component::input::{InputEvent, InputState};

use super::*;

#[derive(Clone, Debug)]
pub enum PaletteAction {
    SelectSession(Id),
    SwitchWorktree(PathBuf),
    SetModel(Harness, String),
    SetPermission(Permission),
    OpenFile(PathBuf),
    ToggleSidebar,
    CompactActive,
    RunAct(Box<Act>),
}

#[derive(Clone, Debug)]
pub struct PaletteItem {
    pub badge: &'static str,
    pub title: String,
    pub subtitle: String,
    pub action: PaletteAction,
}

pub struct CommandPalette {
    pub input: Entity<InputState>,
    pub items: Vec<PaletteItem>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    _sub: Subscription,
}

impl Workspace {
    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type a command, session, worktree, model, or file…"));
        let sub = cx.subscribe_in(&input, window, |ws: &mut Workspace, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => ws.filter_palette(cx),
            InputEvent::PressEnter { .. } => ws.run_palette_selected(window, cx),
            _ => {}
        });
        let items = self.build_palette_items(cx);
        let filtered: Vec<usize> = (0..items.len().min(40)).collect();
        input.update(cx, |i, cx| i.focus(window, cx));
        self.palette = Some(CommandPalette { input, items, filtered, selected: 0, _sub: sub });
        cx.notify();
    }

    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    pub fn move_palette(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(p) = self.palette.as_mut() else { return };
        if p.filtered.is_empty() {
            p.selected = 0;
            return;
        }
        let len = p.filtered.len() as isize;
        p.selected = (p.selected as isize + by).rem_euclid(len) as usize;
        cx.notify();
    }

    pub fn pick_palette(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(p) = self.palette.as_mut() {
            p.selected = ix.min(p.filtered.len().saturating_sub(1));
            cx.notify();
        }
    }

    pub fn run_palette_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let action = self
            .palette
            .as_ref()
            .and_then(|p| p.filtered.get(p.selected).and_then(|&ix| p.items.get(ix)))
            .map(|item| item.action.clone());
        self.palette = None;
        cx.notify();
        let Some(action) = action else { return };
        match action {
            PaletteAction::SelectSession(id) => self.act(Act::Select(id), window, cx),
            PaletteAction::SwitchWorktree(folder) => {
                if let Some(id) = self.active_id().cloned() {
                    self.switch_session(id, folder, Base::Main, false, window, cx);
                }
            }
            PaletteAction::SetModel(harness, model_id) => {
                if let Some(id) = self.active_id().cloned()
                    && let Some(thread) = self.live_view(&id).and_then(SessionView::thread).cloned()
                {
                    let cur = thread.read(cx).harness;
                    if cur == harness {
                        if let Some(opt_id) = thread.read(cx).thread.option("model").map(|o| o.id.clone()) {
                            thread.update(cx, |t, cx| t.set_option(opt_id, model_id, cx));
                        }
                    } else {
                        self.dynamic_handoff(&id, harness, (!model_id.is_empty()).then_some(model_id), String::new(), false, None, window, cx);
                    }
                }
            }
            PaletteAction::SetPermission(permission) => {
                if let Some(id) = self.active_id().cloned()
                    && let Some(thread) = self.live_view(&id).and_then(SessionView::thread).cloned()
                {
                    thread.update(cx, |t, cx| t.set_permission(permission, cx));
                }
            }
            PaletteAction::OpenFile(path) => {
                if let Some(id) = self.editor_owner() {
                    if !self.editor_mode {
                        self.toggle_editor(window, cx);
                    }
                    self.open_file(&id, path, None, window, cx);
                }
            }
            PaletteAction::ToggleSidebar => {
                self.store.data.layout.sidebar_visible = !self.store.data.layout.sidebar_visible;
                self.store.mark_dirty();
                cx.notify();
            }
            PaletteAction::CompactActive => {
                if let Some(id) = self.active_id().cloned()
                    && let Some(thread) = self.live_view(&id).and_then(SessionView::thread).cloned()
                {
                    let transcript = thread.read(cx).thread.transcript(40);
                    self.compact_session(&id, transcript, None, window, cx);
                }
            }
            PaletteAction::RunAct(act) => self.act(*act, window, cx),
        }
    }

    fn filter_palette(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.palette.as_mut() else { return };
        let query = p.input.read(cx).value().trim().to_string();
        if query.is_empty() {
            p.filtered = (0..p.items.len().min(40)).collect();
        } else {
            let mut scored: Vec<(i32, usize)> = p
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    score_palette(&query, &item.title, &item.subtitle, item.badge).map(|s| (s, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            scored.truncate(40);
            p.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }
        p.selected = 0;
        cx.notify();
    }

    fn build_palette_items(&self, cx: &App) -> Vec<PaletteItem> {
        let mut items = Vec::new();
        let active_id = self.active_id().cloned();
        let has_active = active_id.is_some();

        // Core workspace actions
        items.push(PaletteItem {
            badge: "Action",
            title: "New Session".into(),
            subtitle: "Start a new coding-agent session".into(),
            action: PaletteAction::RunAct(Box::new(Act::NewSession { cwd: self.new_session_cwd() })),
        });
        if let Some(id) = active_id.clone() {
            items.push(PaletteItem {
                badge: "Action",
                title: if self.editor_mode { "Close Editor Mode".into() } else { "Open Editor Mode".into() },
                subtitle: "Toggle split file explorer and code editor".into(),
                action: PaletteAction::RunAct(Box::new(Act::ToggleEditor)),
            });
            items.push(PaletteItem {
                badge: "Action",
                title: "Toggle Shell Panel".into(),
                subtitle: "Show or hide the embedded shell".into(),
                action: PaletteAction::RunAct(Box::new(Act::ToggleShell(id))),
            });
            items.push(PaletteItem {
                badge: "Action",
                title: "Compact Context".into(),
                subtitle: "Summarize this conversation into a fresh context window".into(),
                action: PaletteAction::CompactActive,
            });
            items.push(PaletteItem {
                badge: "Action",
                title: "Open GitHub Pane".into(),
                subtitle: "Inspect issues and pull requests for this repository".into(),
                action: PaletteAction::RunAct(Box::new(Act::OpenGithub { cwd: self.active_cwd() })),
            });
        }
        items.push(PaletteItem {
            badge: "Action",
            title: "Toggle Sidebar".into(),
            subtitle: "Show or hide the sessions sidebar".into(),
            action: PaletteAction::ToggleSidebar,
        });
        items.push(PaletteItem {
            badge: "Action",
            title: "Open Settings".into(),
            subtitle: "Configure harnesses, models, skills, and shortcuts".into(),
            action: PaletteAction::RunAct(Box::new(Act::OpenSettings)),
        });
        items.push(PaletteItem {
            badge: "Action",
            title: "Add Project Folder".into(),
            subtitle: "Import a repository into m4code".into(),
            action: PaletteAction::RunAct(Box::new(Act::ImportProject)),
        });

        // Sessions
        for rec in &self.store.data.sessions {
            let running = self.is_running(&rec.id, cx);
            let subtitle = format!(
                "{}{}",
                rec.agent.as_ref().map_or("Shell", |m| m.harness.name()),
                if running { " · running" } else { " · recent" }
            );
            items.push(PaletteItem {
                badge: "Session",
                title: rec.display_title().to_string(),
                subtitle,
                action: PaletteAction::SelectSession(rec.id.clone()),
            });
        }

        // Worktrees for active session
        if let Some(git) = self.shown().project.as_ref().and_then(|p| p.git.as_ref()) {
            for folder in crate::git::worktree::folders(git) {
                let label = if git.is_main(folder) {
                    "main".to_string()
                } else {
                    crate::git::worktree::label(git, folder)
                        .or_else(|| folder.file_name().map(|n| n.to_string_lossy().into_owned()))
                        .unwrap_or_else(|| folder.display().to_string())
                };
                items.push(PaletteItem {
                    badge: "Worktree",
                    title: format!("Switch worktree: {label}"),
                    subtitle: folder.display().to_string(),
                    action: PaletteAction::SwitchWorktree(folder.to_path_buf()),
                });
            }
        }

        // Permissions & Models for active session
        if has_active {
            for perm in [Permission::ReadOnly, Permission::Auto, Permission::FullAccess] {
                items.push(PaletteItem {
                    badge: "Mode",
                    title: format!("Permission: {}", perm.label()),
                    subtitle: "Change permission mode for this session".into(),
                    action: PaletteAction::SetPermission(perm),
                });
            }
            for m in self.store.data.settings.aggregated_models() {
                items.push(PaletteItem {
                    badge: "Model",
                    title: m.name.clone(),
                    subtitle: format!("{} · {}", m.harness.name(), if m.id.is_empty() { "default" } else { &m.id }),
                    action: PaletteAction::SetModel(m.harness, m.id),
                });
            }
        }

        // Indexed files from active editor pane
        if let Some(owner) = self.editor_owner()
            && let Some(pane) = self.panes.get(&owner)
        {
            for (path, rel) in pane.files.iter().zip(pane.relative.iter()).take(300) {
                items.push(PaletteItem {
                    badge: "File",
                    title: path.file_name().map_or_else(|| rel.clone(), |n| n.to_string_lossy().into_owned()),
                    subtitle: rel.clone(),
                    action: PaletteAction::OpenFile(path.clone()),
                });
            }
        }

        items
    }
}

/// Fuzzy match score for a palette item; `None` when the query does not match.
pub fn score_palette(query: &str, title: &str, subtitle: &str, badge: &str) -> Option<i32> {
    let q = query.to_ascii_lowercase();
    let t = title.to_ascii_lowercase();
    let s = subtitle.to_ascii_lowercase();
    let b = badge.to_ascii_lowercase();
    if t.starts_with(&q) {
        return Some(1000 - (t.len() as i32));
    }
    if let Some(pos) = t.find(&q) {
        return Some(700 - pos as i32);
    }
    if b.starts_with(&q) {
        return Some(550);
    }
    if let Some(pos) = s.find(&q) {
        return Some(400 - pos as i32);
    }
    // Subsequence match on "badge title subtitle"
    let haystack = format!("{b} {t} {s}");
    let mut chars = haystack.chars();
    let mut matched = 0i32;
    for qc in q.chars() {
        if chars.any(|hc| hc == qc) {
            matched += 1;
        } else {
            return None;
        }
    }
    Some(100 + matched * 10 - (haystack.len() as i32 / 8))
}

#[cfg(test)]
mod tests {
    use super::score_palette;

    #[test]
    fn palette_ranks_prefix_above_substring_and_subsequence() {
        let prefix = score_palette("set", "Settings", "Open settings", "Action").unwrap();
        let substr = score_palette("set", "Open Settings", "Configure", "Action").unwrap();
        let subseq = score_palette("ost", "Open Settings", "Configure", "Action").unwrap();
        assert!(prefix > substr);
        assert!(substr > subseq);
        assert!(score_palette("zzz", "Open Settings", "Configure", "Action").is_none());
    }
}
