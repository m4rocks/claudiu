//! Settings → Skills page: browse/install `SKILL.md` packages from `skills.sh`, toggle them globally or per project,
//! and show which active harnesses receive synced symlinks.

use gpui::{ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputState};

use crate::app::{Act, SkillScope, SkillsTab, Workspace, WorkspacePicker};
use crate::panels::settings_pages::switch_row;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{Trigger, button, button_disabled, icon, section_label};
use crate::skills::registry::{CatalogEntry, format_installs};

fn faint(text: impl Into<SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(text.into())
}

fn warn(text: impl Into<SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::WARN)).child(text.into())
}

impl Workspace {
    pub(crate) fn skills_page(&self, search: &gpui::Entity<InputState>, cx: &mut Context<Self>) -> Div {
        let scaled = crate::panels::scale::px;
        let state = &self.skills_page;
        let installed = &self.store.data.settings.skills;
        let harnesses: Vec<&'static str> = self.active_skill_harnesses(None).into_iter().map(|h| h.name()).collect();

        let tabs = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                button("skills-tab-installed", format!("Installed ({})", installed.installed.len()), state.tab == SkillsTab::Installed)
                    .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::SetSkillsTab(SkillsTab::Installed), window, cx))),
            )
            .child(
                button("skills-tab-browse", "Browse skills.sh", state.tab == SkillsTab::Browse)
                    .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::SetSkillsTab(SkillsTab::Browse), window, cx))),
            );

        let top_bar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(tabs)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(faint(format!("Syncing to: {}", harnesses.join(", ")))),
            );

        let mut page = div().flex().flex_col().gap(px(10.0)).child(top_bar);

        match state.tab {
            SkillsTab::Installed => {
                page = page.child(self.skills_scope_row(cx));
                if installed.installed.is_empty() {
                    page = page
                        .child(faint("No skills installed yet."))
                        .child(div().flex().child(button("skills-empty-browse", "Browse skills.sh", true).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                            ws.act(Act::SetSkillsTab(SkillsTab::Browse), window, cx)
                        }))));
                    return page;
                }
                let scope = self.effective_skill_scope();
                for (i, skill) in installed.installed.iter().enumerate() {
                    let name = skill.name.clone();
                    let toggle_name = name.clone();
                    let remove_name = name.clone();
                    let on = match &scope {
                        SkillScope::Global => installed.is_global(&skill.name),
                        SkillScope::Project(id) => installed.is_project(id, &skill.name),
                    };
                    let tag = match &scope {
                        SkillScope::Project(_) if installed.is_global(&skill.name) => Some(SharedString::from(format!("{} · on globally", skill.source))),
                        _ => Some(SharedString::from(skill.source.clone())),
                    };
                    let row = switch_row(
                        SharedString::from(format!("skill-toggle-{i}")),
                        skill.name.clone(),
                        tag,
                        on,
                        move |ws, window, cx| ws.act(Act::ToggleSkill(toggle_name.clone()), window, cx),
                        cx,
                    );
                    let mut card = div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .child(div().flex_1().min_w_0().child(row))
                                .child(button(format!("skill-remove-{i}"), "Remove", false).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                                    ws.act(Act::UninstallSkill(remove_name.clone()), window, cx)
                                }))),
                        );
                    if !skill.description.is_empty() {
                        card = card.child(
                            div()
                                .px(px(10.0))
                                .text_size(scaled(11.0))
                                .text_color(hsla(theme::TEXT_FAINT))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(skill.description.clone()),
                        );
                    }
                    page = page.child(card);
                }
            }
            SkillsTab::Browse => {
                let mut search_bar = div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(div().flex_1().min_w_0().child(Input::new(search)))
                    .child(button("skills-search-go", "Search", true).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                        ws.act(Act::SearchSkills, window, cx)
                    })));
                if state.results.is_some() {
                    search_bar = search_bar.child(button("skills-search-clear", "Popular", false).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                        if let Some(crate::app::Modal::Settings { skill_search, .. }) = &ws.modal {
                            skill_search.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                        ws.skills_page.results = None;
                        ws.skills_page.error = None;
                        ws.load_skills_catalog(cx);
                        cx.notify();
                    })));
                }
                page = page.child(search_bar).child(self.skills_scope_row(cx));

                if let Some(err) = &state.error {
                    page = page.child(warn(err.clone()));
                }
                if state.loading {
                    page = page.child(faint("Loading skills.sh…"));
                }

                let (heading, entries): (String, &[CatalogEntry]) = match &state.results {
                    Some((q, list)) => (format!("RESULTS FOR “{q}” ({})", list.len()), list.as_slice()),
                    None => match &state.popular {
                        Some(list) => (format!("POPULAR ON SKILLS.SH ({})", list.len().min(40)), &list[..list.len().min(40)]),
                        None => (String::new(), &[]),
                    },
                };

                if !heading.is_empty() {
                    let single_repo: Option<String> = if !entries.is_empty() && entries.iter().all(|e| e.source == entries[0].source) {
                        Some(entries[0].source.clone())
                    } else if let Some((q, _)) = &state.results
                        && crate::skills::valid_source(q.trim())
                    {
                        Some(q.trim().to_string())
                    } else {
                        None
                    };
                    let mut bar = div().mt(px(4.0)).flex().items_center().justify_between().gap(px(8.0)).child(section_label(heading));
                    if let Some(repo) = single_repo {
                        let repo_key = format!("repo:{repo}");
                        let busy = state.installing.contains(&repo_key);
                        let label = if busy {
                            "Installing repo…".to_string()
                        } else if !entries.is_empty() {
                            format!("Install entire repo ({})", entries.len())
                        } else {
                            format!("Install {repo}")
                        };
                        let btn = if busy {
                            button_disabled("skills-install-repo", label).into_any_element()
                        } else {
                            button("skills-install-repo", label, true)
                                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::InstallSkillRepo(repo.clone()), window, cx)))
                                .into_any_element()
                        };
                        bar = bar.child(btn);
                    }
                    page = page.child(bar);
                }
                if !state.loading && state.error.is_none() && entries.is_empty() && (state.results.is_some() || state.popular.is_some()) {
                    page = page.child(faint("No matching skills found. Try another search or enter a GitHub owner/repo."));
                }
                for (i, entry) in entries.iter().enumerate() {
                    let key = entry.key();
                    let repo_key = format!("repo:{}", entry.source);
                    let folder = entry.folder_name().unwrap_or(&entry.name);
                    let already = installed.is_installed(folder);
                    let busy = state.installing.contains(&key);
                    let repo_busy = state.installing.contains(&repo_key);
                    let install_entry = entry.clone();
                    let install_repo = entry.source.clone();
                    let action = if busy {
                        button_disabled(format!("skill-install-{i}"), "Installing…").into_any_element()
                    } else if already {
                        button_disabled(format!("skill-install-{i}"), "Installed").into_any_element()
                    } else {
                        button(format!("skill-install-{i}"), "Install", false)
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::InstallSkill(install_entry.clone()), window, cx)))
                            .into_any_element()
                    };
                    let repo_btn = if repo_busy {
                        button_disabled(format!("skill-repo-{i}"), "Installing…").into_any_element()
                    } else {
                        button(format!("skill-repo-{i}"), "Install repo", false)
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::InstallSkillRepo(install_repo.clone()), window, cx)))
                            .into_any_element()
                    };
                    let mut meta = entry.source.clone();
                    if entry.installs > 0 {
                        meta.push_str(&format!(" · {} installs", format_installs(entry.installs)));
                    }
                    if entry.is_official {
                        meta.push_str(" · official");
                    }
                    page = page.child(
                        div()
                            .flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap(px(10.0))
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(6.0))
                            .bg(hsla(theme::SURFACE_ELEVATED))
                            .border_1()
                            .border_color(hsla(theme::PANEL_BORDER))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.0))
                                    .child(div().text_size(scaled(13.0)).font_weight(FontWeight::MEDIUM).child(entry.name.clone()))
                                    .child(faint(meta).overflow_hidden().text_ellipsis().whitespace_nowrap()),
                            )
                            .child(div().flex().flex_none().gap(px(6.0)).child(repo_btn).child(action)),
                    );
                }
            }
        }
        page
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
            SkillsTab::Installed => "Toggle scope",
            SkillsTab::Browse => "Enable on install",
        };
        let open = matches!(self.picker_kind(), Some(WorkspacePicker::SkillsScope { .. }));
        div()
            .id("skills-scope")
            .h(px(36.0))
            .px(px(14.0))
            .flex()
            .items_center()
            .justify_between()
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
                    .items_center()
                    .gap(px(6.0))
                    .child(div().text_color(hsla(theme::ACCENT)).child(label))
                    .child(icon(IconName::ChevronDown, 11.0).text_color(hsla(theme::TEXT_FAINT))),
            )
            .dropdown_right(cx, |ws, at, window, cx| ws.open_skills_scope_picker(at, window, cx))
    }
}
