//! Settings → Skills page: browse/install `SKILL.md` packages from `skills.sh`, toggle them globally or per project,
//! and show which active harnesses receive synced symlinks.

use gpui::{AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div, px};
use gpui_kit::{assets::IconName, base::{Tab, Tabs}};
use gpui_kit::component::input::{Input, InputState};

use crate::app::{Act, SkillScope, SkillsTab, Workspace, WorkspacePicker};
use crate::panels::settings_pages::{settings_group, toggle_switch};
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{Trigger, button, button_disabled, fade_in, focusable, icon, empty_panel, skeleton_rows};
use crate::skills::{CatalogStatus, InstalledSkill};
use crate::skills::registry::{CatalogEntry, format_installs};

fn faint(text: impl Into<SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(text.into())
}

fn warn(text: impl Into<SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::WARN)).child(text.into())
}

fn empty_state(text: impl Into<SharedString>) -> Div {
    empty_panel(IconName::Search, text)
}

fn search_group(input: &gpui::Entity<InputState>, action: Option<AnyElement>) -> Div {
    div().flex().flex_1().min_w_0().items_center().gap(px(6.0)).px(px(8.0)).py(px(3.0))
        .rounded(px(7.0)).border_1().border_color(hsla(theme::PANEL_BORDER))
        .bg(hsla(theme::PANEL_BG))
        .child(div().flex_none().child(icon(IconName::Search, 14.0).text_color(hsla(theme::TEXT_DIM))))
        .child(div().flex_1().min_w_0().child(Input::new(input).appearance(false)))
        .children(action)
}

fn skill_row(name: &str, description: &str, actions: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .px(px(12.0))
        .py(px(9.0))
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(3.0))
            .child(div().text_size(crate::panels::scale::px(13.0)).font_weight(FontWeight::MEDIUM).child(name.to_string()))
            .children((!description.is_empty()).then(|| faint(description.to_string()))))
        .children((!actions.is_empty()).then(|| div().flex().flex_none().items_center().gap(px(5.0)).children(actions)))
}

impl Workspace {
    pub(crate) fn skills_page(&self, filter: &gpui::Entity<InputState>, search: &gpui::Entity<InputState>, cx: &mut Context<Self>) -> Div {
        let state = &self.skills_page;
        let count = self.store.data.settings.skills.installed.len();
        let harnesses: Vec<_> = self.active_skill_harnesses(None).into_iter().map(|h| h.name()).collect();
        let tabs = Tabs::new("skills-tabs")
            .flex().items_center().gap(px(2.0)).p(px(3.0)).rounded(px(8.0))
            .bg(hsla(theme::SURFACE_HOVER))
            .children([
                ("skills-tab-installed", format!("Installed ({count})"), SkillsTab::Installed),
                ("skills-tab-browse", "Browse".into(), SkillsTab::Browse),
            ].into_iter().enumerate().map(|(i, (id, label, tab))| {
                Tab::new(id).set_position(i + 1, 2).selected(state.tab == tab)
                    .cursor_pointer().h(px(28.0)).px(px(10.0)).rounded(px(6.0))
                    .text_color(hsla(theme::TEXT_DIM)).text_size(crate::panels::scale::px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .styles(|styles| styles.selected(|s| s.bg(hsla(theme::PANEL_BG)).text_color(hsla(theme::TEXT))))
                    .child(label)
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::SetSkillsTab(tab), window, cx)))
            }));
        let header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(tabs)
            .child(faint(format!("Syncs to {}", harnesses.join(", "))));
        let (content, key) = match state.tab {
            SkillsTab::Installed => (self.installed_skills(filter, cx), "skills-installed-content"),
            SkillsTab::Browse => (self.browse_skills(search, cx), "skills-browse-content"),
        };
        div().flex().flex_col().gap(px(12.0)).child(header).child(self.skills_scope_row(cx))
            .child(fade_in(content, key, 150, 0.85))
    }

    fn installed_skills(&self, filter: &gpui::Entity<InputState>, cx: &mut Context<Self>) -> Div {
        let installed = &self.store.data.settings.skills;
        let mut page = div().flex().flex_col().gap(px(12.0));
        if installed.installed.is_empty() {
            page = page.child(settings_group("Installed skills", None, vec![
                empty_state("No skills installed yet.")
                    .child(button("skills-empty-browse", "Browse skills.sh", true).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                        ws.act(Act::SetSkillsTab(SkillsTab::Browse), window, cx)
                    })))
                    .into_any_element(),
            ]));
        } else {
            let all: Vec<_> = installed.installed.iter().map(|skill| skill.name.clone()).collect();
            page = page.child(div().flex().items_center().gap(px(8.0))
                .child(search_group(filter, None))
                .child(self.update_skills_button("skills-update-all", "Update all", all, cx)));
            let query = filter.read(cx).value().trim().to_lowercase();
            let mut matching = 0;
            for (source, skills) in installed.by_source() {
                let rows: Vec<_> = skills.iter()
                    .filter(|skill| query.is_empty() || source.to_lowercase().contains(&query)
                        || skill.name.to_lowercase().contains(&query) || skill.description.to_lowercase().contains(&query))
                    .map(|skill| self.installed_skill_row(skill, cx).into_any_element())
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                matching += rows.len();
                let names = skills.iter().map(|skill| skill.name.clone()).collect();
                let update = self.update_skills_button(format!("skills-update-repo-{source}"), "Update repo", names, cx);
                page = page.child(settings_group(source.to_string(), Some(update), rows));
            }
            if matching == 0 {
                page = page.child(empty_state("No installed skills match your filter."));
            }
        }
        page.children(self.repo_skills_section())
    }

    fn browse_skills(&self, search: &gpui::Entity<InputState>, cx: &mut Context<Self>) -> Div {
        let state = &self.skills_page;
        let mut search_bar = div().flex().items_center().gap(px(8.0))
            .child(search_group(search, Some(button("skills-search-go", "Search", true)
                .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::SearchSkills, window, cx)))
                .into_any_element())));
        if state.search_query.is_some() || state.error.is_some() {
            search_bar = search_bar.child(button("skills-search-clear", "Popular", false).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                if let Some(crate::app::Modal::Settings { skill_search, .. }) = &ws.modal {
                    skill_search.update(cx, |s, cx| s.set_value("", window, cx));
                }
                ws.show_popular_skills(cx);
            })));
        }
        let page = div().flex().flex_col().gap(px(12.0)).child(search_bar);
        if state.loading {
            let status = state.search_query.as_ref().map_or_else(
                || "Loading popular skills from skills.sh".into(),
                |query| format!("Searching for {query}"),
            );
            return page.child(settings_group(status, None, vec![skeleton_rows(5).into_any_element()]));
        }
        if let Some(error) = &state.error {
            return page.child(empty_state("Could not load skills").child(warn(error.clone())));
        }
        let (heading, entries) = if let (Some(query), Some(list)) = (&state.search_query, &state.results) {
            (format!("Results for {query} ({})", list.len()), list.as_slice())
        } else {
            let popular = state.popular.as_deref().unwrap_or_default();
            ("Popular on skills.sh".into(), &popular[..popular.len().min(40)])
        };
        let repo = state.search_query.as_ref().and_then(|query| {
            if crate::skills::valid_source(query) {
                Some(query.clone())
            } else {
                entries.first().filter(|first| entries.iter().all(|entry| entry.source == first.source)).map(|first| first.source.clone())
            }
        });
        let install_repo = repo.map(|repo| {
            if !state.installing.is_empty() {
                let label = if state.installing.contains(&format!("repo:{repo}")) { "Installing repository" } else { "Install repository" };
                button_disabled("skills-install-repo", label).into_any_element()
            } else {
                button("skills-install-repo", "Install repository", false)
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::InstallSkillRepo(repo.clone()), window, cx)))
                    .into_any_element()
            }
        });
        let mut rows: Vec<_> = entries.iter().map(|entry| self.catalog_skill_row(entry, cx).into_any_element()).collect();
        if rows.is_empty() {
            rows.push(empty_state("No skills found. Try a different search or enter a GitHub owner/repo.").into_any_element());
        }
        page.child(settings_group(heading, install_repo, rows))
    }

    fn catalog_skill_row(&self, entry: &CatalogEntry, cx: &mut Context<Self>) -> Div {
        let key = entry.key();
        let status = self.store.data.settings.skills.catalog_status(entry);
        let busy = self.skills_page.installing.contains(&key)
            || self.skills_page.installing.contains(&format!("repo:{}", entry.source));
        let action = match (busy, &status) {
            (true, _) => button_disabled(format!("skill-install-{key}"), "Installing").into_any_element(),
            (_, CatalogStatus::Installed) => button_disabled(format!("skill-install-{key}"), "Installed").into_any_element(),
            (_, CatalogStatus::NameInUse(_)) => button_disabled(format!("skill-install-{key}"), "Name in use").into_any_element(),
            (_, CatalogStatus::Available) if !self.skills_page.installing.is_empty() => {
                button_disabled(format!("skill-install-{key}"), "Install").into_any_element()
            }
            (_, CatalogStatus::Available) => {
                let entry = entry.clone();
                button(format!("skill-install-{key}"), "Install", false)
                    .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::InstallSkill(entry.clone()), window, cx)))
                    .into_any_element()
            }
        };
        let mut metadata = entry.source.clone();
        if entry.installs > 0 {
            metadata.push_str(&format!(" · {} installs", format_installs(entry.installs)));
        }
        if entry.is_official {
            metadata.push_str(" · Official");
        }
        if let CatalogStatus::NameInUse(source) = status {
            metadata.push_str(&format!(" · Name used by {source}"));
        }
        skill_row(&entry.name, &metadata, vec![action])
    }

    fn installed_skill_row(&self, skill: &InstalledSkill, cx: &mut Context<Self>) -> Div {
        let installed = &self.store.data.settings.skills;
        let scoped = self.effective_skill_scope();
        let inherited = matches!(scoped, SkillScope::Project(_)) && installed.is_global(&skill.name);
        let on = match scoped {
            SkillScope::Global => installed.is_global(&skill.name),
            SkillScope::Project(id) => installed.is_project(&id, &skill.name),
        };
        let toggle_name = skill.name.clone();
        let remove_name = skill.name.clone();
        let description = if inherited {
            if skill.description.is_empty() { "Enabled globally".into() } else { format!("Enabled globally · {}", skill.description) }
        } else {
            skill.description.clone()
        };
        let toggle = focusable(format!("skill-toggle-{}", skill.name))
            .accessibility_label(format!("{} {} for this scope", if on { "Disable" } else { "Enable" }, skill.name))
            .p(px(4.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
            .child(toggle_switch(on))
            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::ToggleSkill(toggle_name.clone()), window, cx)));
        skill_row(&skill.name, &description, vec![
            toggle.into_any_element(),
            self.update_skills_button(format!("skill-update-{}", skill.name), "Update", vec![skill.name.clone()], cx),
            if self.skills_page.installing.is_empty() {
                button(format!("skill-remove-{}", skill.name), "Remove", false).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                    ws.act(Act::UninstallSkill(remove_name.clone()), window, cx)
                })).into_any_element()
            } else {
                button_disabled(format!("skill-remove-{}", skill.name), "Remove").into_any_element()
            },
        ])
    }

    /// Button that updates `names`, shown as "Updating…" and disabled while any of them is downloading.
    fn update_skills_button(&self, id: impl Into<SharedString>, label: &'static str, names: Vec<String>, cx: &mut Context<Self>) -> gpui::AnyElement {
        let busy = names.iter().any(|n| self.skills_page.installing.contains(&Workspace::update_key(n)));
        if !self.skills_page.installing.is_empty() {
            return button_disabled(id, if busy { "Updating…" } else { label }).into_any_element();
        }
        button(id, label, false)
            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::UpdateSkills(names.clone()), window, cx)))
            .into_any_element()
    }

    /// Read-only list of skills the selected project's repository ships in its own skills folders.
    fn repo_skills_section(&self) -> Option<Div> {
        let SkillScope::Project(id) = self.effective_skill_scope() else { return None };
        let rows = match &self.skills_page.repo_skills {
            Some((for_id, found)) if *for_id == id && !found.is_empty() => found.iter().map(|skill| {
                let folder = skill.folder.display();
                let details = if skill.description.is_empty() { folder.to_string() } else { format!("{folder} · {}", skill.description) };
                skill_row(&skill.name, &details, vec![]).into_any_element()
            }).collect(),
            Some((for_id, _)) if *for_id == id => vec![empty_state("This repository has no skills of its own.").into_any_element()],
            _ => vec![empty_state("Looking for repository skills").into_any_element()],
        };
        Some(settings_group("Repository skills", None, rows))
    }

    /// Current scope, falling back to Global if a previously selected project was removed.
    pub(crate) fn effective_skill_scope(&self) -> SkillScope {
        match &self.skills_page.scope {
            SkillScope::Global => SkillScope::Global,
            SkillScope::Project(id) if self.store.project(id).is_some() => SkillScope::Project(id.clone()),
            SkillScope::Project(_) => SkillScope::Global,
        }
    }

    /// Picker row choosing whether toggles and installs target Global or a specific project (opens universal dialog).
    fn skills_scope_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.effective_skill_scope();
        let label = match &current {
            SkillScope::Global => "Global (all workspaces)".to_string(),
            SkillScope::Project(id) => self.store.project(id).map_or_else(|| "Global (all workspaces)".into(), |p| format!("Project · {}", p.name)),
        };
        let caption = match self.skills_page.tab {
            SkillsTab::Installed => "Manage enablement for",
            SkillsTab::Browse => "Enable newly installed skills for",
        };
        let open = matches!(self.picker_kind(), Some(WorkspacePicker::SkillsScope { .. }));
        div()
            .id("skills-scope")
            .min_h(px(36.0))
            .px(px(12.0))
            .py(px(6.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .rounded(px(8.0))
            .bg(hsla(theme::SURFACE_ELEVATED))
            .border_1()
            .border_color(hsla(if open { theme::ACCENT } else { theme::PANEL_BORDER }))
            .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
            .text_size(crate::panels::scale::px(13.0))
            .cursor_pointer()
            .child(caption)
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(6.0))
                    .child(div().min_w_0().text_ellipsis().overflow_hidden().whitespace_nowrap().text_color(hsla(theme::ACCENT)).child(label))
                    .child(div().flex_none().child(icon(IconName::ChevronDown, 11.0).text_color(hsla(theme::TEXT_FAINT)))),
            )
            .dropdown_right(cx, |ws, at, window, cx| ws.open_skills_scope_picker(at, window, cx))
    }
}
