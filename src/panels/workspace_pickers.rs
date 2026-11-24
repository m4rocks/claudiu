//! The workspace's anchored dropdowns (`WorkspacePicker`): the Welcome screen's model and project, branch & worktree,
//! the Settings selectors, the Skills scope and the editor choice. `picker` draws the dropdown itself.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton,
    ParentElement, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px,
};

use crate::harness::defaults::Effort;
use crate::harness::permission::Permission;
use crate::app::{
    Act, GitPickerTarget, Modal, ProjectPickerMode, SkillScope, Workspace, WorkspacePicker,
};
use crate::panels::{logos, picker};
use crate::panels::theme::{self, hsla};
use gpui_kit::assets::IconName;
use crate::panels::widgets::{
    effort_slider, icon, icon_button, short_path, spinner,
};

impl Workspace {
    /// Universal anchored dropdown picker (based on the Model Picker) used for model, project, branch & worktree,
    /// Settings selectors, Skills scope, and editor selection.
    pub(super) fn render_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let active = self.picker.as_ref()?;
        let at = active.at;
        let scaled = crate::panels::scale::px;
        let accent = hsla(theme::ACCENT);
        let priority = if self.modal.is_some() { 25 } else { 22 };

        let (id, title, body, choices): (&'static str, &'static str, AnyElement, Option<picker::Choices<Self>>) = match &active.kind {
            WorkspacePicker::HomeModel { search } => {
                let query = search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let chosen_model = self.home_selected_model();
                let filtered: Vec<crate::harness::CatalogModel> = self
                    .store
                    .data
                    .settings
                    .aggregated_models()
                    .into_iter()
                    .filter(|m| {
                        let hay = format!("{} {} {}", m.name, m.id, m.harness.name()).to_lowercase();
                        words.iter().all(|w| hay.contains(w))
                    })
                    .collect();
                let hi = self.home.model_highlight.min(filtered.len().saturating_sub(1));
                let mut list = self.home.model_scroll.wire(div().id("home-model-list").role(gpui::Role::ListBox).aria_label("Models").max_h(px(176.0)), window).flex().flex_col().gap(px(2.0));
                let mut last_harness = None;
                for (n, m) in filtered.iter().enumerate() {
                    if last_harness != Some(m.harness) {
                        last_harness = Some(m.harness);
                        list = list.child(
                            div()
                                .px(px(8.0))
                                .pt(px(if n == 0 { 2.0 } else { 6.0 }))
                                .pb(px(2.0))
                                .text_size(scaled(10.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(m.harness.name()),
                        );
                    }
                    let selected = chosen_model.harness == m.harness && chosen_model.id == m.id;
                    let focused = n == hi;
                    let choice = m.clone();
                    let logo = logos::draw(logos::for_harness(m.harness), 11.5, if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    list = list.child(
                        picker::row(gpui::SharedString::from(format!("home-model-opt-{n}")), Some(logo), m.name.clone(), None, None, selected, false)
                            .role(gpui::Role::ListBoxOption)
                            .aria_selected(selected)
                            .when(focused && !selected, |d| d.bg(hsla(theme::SURFACE_HOVER)))
                            .border_1()
                            .border_color(if focused { hsla(theme::FOCUS_BORDER) } else { gpui::transparent_black() })
                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(Act::HomeSelectModel(choice.clone()), window, cx))),
                    );
                }
                if filtered.is_empty() {
                    list = list.child(picker::empty_note("No models match."));
                }

                let effort_labels: Vec<String> = vec!["Low".into(), "Med".into(), "High".into(), "XHigh".into(), "Max".into()];
                let effort_ix = Effort::ALL.iter().position(|x| *x == self.home.effort).unwrap_or(1);
                let on_effort: crate::panels::widgets::EffortPick<Self> = std::rc::Rc::new(|ws, ix, window, cx| {
                    if let Some(&e) = Effort::ALL.get(ix) {
                        ws.act(Act::HomeSetEffort(e), window, cx);
                    }
                });
                let effort_trailing = div().text_size(scaled(10.5)).text_color(hsla(theme::TEXT_FAINT)).child("←/→ or drag").into_any_element();
                let loading = self.model_lookups.any_loading(&self.store.data.settings.enabled_harnesses_list());
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(picker::search_input(search))
                    .child(list)
                    .when(loading, |d| {
                        d.child(
                            div()
                                .px(px(6.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(scaled(11.0))
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(spinner("home-models-loading", 11.0))
                                .child("Loading models…"),
                        )
                    })
                    .child(picker::divider())
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .child(picker::section_header("EFFORT", Some(effort_trailing)))
                            .child(effort_slider(
                                "home-effort",
                                &effort_labels,
                                Some(effort_ix),
                                accent,
                                self.home.effort_bounds.clone(),
                                self.home.effort_dragging.clone(),
                                on_effort,
                                cx,
                            )),
                    );
                ("home-model-dialog", "MODEL", body.into_any_element(), None)
            }
            WorkspacePicker::Project { mode, search } => {
                let mode = *mode;
                let query = search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let matches = |hay: &str| {
                    let low = hay.to_lowercase();
                    words.iter().all(|w| low.contains(w))
                };
                let current_id = match mode {
                    ProjectPickerMode::Home => self.home_selected_project().map(|p| p.id.clone()),
                    ProjectPickerMode::SidebarFilter => self.project_filter().map(|p| p.id.clone()),
                };
                let mut choices = picker::Choices::new(&self.picker_nav, &query);
                let mut list = choices.list("project-picker-list", window).max_h(px(260.0));
                let mut shown = 0usize;
                if mode == ProjectPickerMode::SidebarFilter && matches("all projects") {
                    let selected = current_id.is_none();
                    let lead = icon(IconName::FolderOpen, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row("project-picker-all", Some(lead), "All projects", Some("Show sessions from every project".into()), None, selected, false);
                    list = list.child(choices.add(row, |ws, window, cx| ws.act(Act::FilterProject(None), window, cx), cx));
                    shown += 1;
                }
                for (i, p) in self.store.data.projects.iter().enumerate() {
                    let path_str = short_path(&p.path);
                    if !matches(&format!("{} {path_str}", p.name)) {
                        continue;
                    }
                    let selected = current_id.as_ref() == Some(&p.id);
                    let lead = icon(IconName::FolderOpen, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let act = match mode {
                        ProjectPickerMode::Home => Act::HomeSelectProject(p.id.clone()),
                        ProjectPickerMode::SidebarFilter => Act::FilterProject(Some(p.id.clone())),
                    };
                    let row = picker::row(gpui::SharedString::from(format!("project-picker-{i}")), Some(lead), p.name.clone(), Some(path_str), None, selected, false);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(act.clone(), window, cx), cx));
                    shown += 1;
                }
                if shown == 0 {
                    list = list.child(picker::empty_note("No projects match."));
                }
                let add_btn = picker::action_pill("project-picker-add", "+ Add project", |ws, window, cx| ws.act(Act::ImportProject, window, cx), cx).into_any_element();
                let title = "PROJECT";
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(picker::search_input(search))
                    .child(picker::section_header("PROJECTS", Some(add_btn)))
                    .child(list);
                ("project-picker-dialog", title, body.into_any_element(), Some(choices))
            }
            WorkspacePicker::Git(gp) => {
                let query = gp.search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let matches = |hay: &str| {
                    let low = hay.to_lowercase();
                    words.iter().all(|w| low.contains(w))
                };
                let target = gp.target;
                let can_switch = gp.can_switch_worktree;
                let new_wt_disabled = gp.new_worktree_reason.is_some() || !can_switch;
                let main_checkout = gp.main_checkout.clone();
                let cwd = gp.cwd.clone();

                let mut choices = picker::Choices::new(&self.picker_nav, &query);
                // The quickest way to start work: a worktree named after what was typed (or a random name), made at once.
                let typed = gp.search.read(cx).value().trim().to_string();
                let quick = |base: crate::git::worktree::Base| Act::QuickWorktree { main_checkout: main_checkout.clone(), name: typed.clone(), base, for_home: target == GitPickerTarget::Home };
                let new_wt_title = if typed.is_empty() { "New worktree".to_string() } else { format!("New worktree “{typed}”") };
                let new_wt_row: AnyElement = if new_wt_disabled {
                    let why = gp.new_worktree_reason.clone().unwrap_or_else(|| "Resume the session to create one".into());
                    picker::row("git-picker-new-worktree", Some(icon(IconName::Plus, 12.0).into_any_element()), new_wt_title, Some(why), None, false, true).into_any_element()
                } else {
                    let detail = match (typed.is_empty(), target) {
                        (true, GitPickerTarget::Session) => "Random name, from main. The session moves into it; type above to name it",
                        (true, GitPickerTarget::Home) => "Random name, from main. Type above to name it",
                        (false, GitPickerTarget::Session) => "New branch from main. The session moves into it",
                        (false, GitPickerTarget::Home) => "New branch from main, for this session",
                    };
                    let from_main = quick(crate::git::worktree::Base::Main);
                    let from_current = quick(crate::git::worktree::Base::Current);
                    let current_pill = picker::action_pill("git-picker-new-worktree-current", "from current", move |ws, window, cx| ws.act(from_current.clone(), window, cx), cx)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .into_any_element();
                    let row = picker::row("git-picker-new-worktree", Some(icon(IconName::Plus, 12.0).into_any_element()), new_wt_title, Some(detail.into()), Some(current_pill), false, false)
                        .border_1()
                        .border_color(hsla(theme::PANEL_BORDER));
                    choices.add(row, move |ws, window, cx| ws.act(from_main.clone(), window, cx), cx)
                };

                let mut wt_list: Vec<AnyElement> = Vec::new();
                let mut wt_shown = 0usize;
                for (i, wt) in gp.worktrees.iter().enumerate() {
                    if !matches(&format!("{} {}", wt.label, wt.branch.as_deref().unwrap_or(""))) {
                        continue;
                    }
                    let current = wt.current;
                    let folder = wt.folder.clone();
                    let remove = wt.remove.clone();
                    let row_group = gpui::SharedString::from(format!("gp-wt-row-{i}"));
                    let lead = icon(IconName::FolderGit2, 12.0).text_color(if current { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let trailing = div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .when(wt.running, |d| d.child(div().w(px(5.0)).h(px(5.0)).rounded_full().bg(hsla(theme::OK))))
                        .when_some(wt.branch.clone(), |d, b| {
                            d.child(div().max_w(px(110.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(scaled(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(b))
                        })
                        .children(remove.map(|act| {
                            icon_button(gpui::SharedString::from(format!("git-picker-wt-rm-{i}")), IconName::X)
                                .invisible()
                                .group_hover(row_group.clone(), |s| s.visible())
                                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    ws.act(act.clone(), window, cx);
                                }))
                        }))
                        .into_any_element();
                    let detail = if wt.missing { format!("Missing folder: {}", wt.folder.display()) } else { wt.folder.display().to_string() };
                    let row = picker::row(gpui::SharedString::from(format!("git-picker-wt-{i}")), Some(lead), wt.label.clone(), Some(detail), Some(trailing), current, !can_switch || wt.missing).group(row_group);
                    wt_list.push(if can_switch && !wt.missing {
                        choices.add(
                            row,
                            move |ws, window, cx| {
                                if current {
                                    ws.dismiss_picker(window, cx);
                                } else {
                                    let act = match target {
                                        GitPickerTarget::Home => Act::HomeSelectWorktree(folder.clone()),
                                        GitPickerTarget::Session => Act::SwitchWorktree { folder: folder.clone(), base: Default::default() },
                                    };
                                    ws.act(act, window, cx);
                                }
                            },
                            cx,
                        )
                    } else {
                        row.into_any_element()
                    });
                    wt_shown += 1;
                }
                if wt_shown == 0 {
                    wt_list.push(picker::empty_note("No matching worktrees.").into_any_element());
                }

                let new_branch_cwd = cwd.clone();
                let new_branch_btn = picker::action_pill("git-picker-new-branch", "+ New branch", move |ws, window, cx| ws.act(Act::NewBranch(new_branch_cwd.clone()), window, cx), cx).into_any_element();
                // Spaces aren't allowed in branch names, as in the New branch dialog.
                let typed_branch = typed.split_whitespace().collect::<Vec<_>>().join("-");
                let branch_exists = matches!(&gp.branches, Some(Ok(list)) if list.iter().any(|b| b.name == typed_branch));
                let create_branch_row = (!typed_branch.is_empty() && !branch_exists).then(|| {
                    let act = Act::CreateBranch(cwd.clone(), typed_branch.clone());
                    let row = picker::row("git-picker-create-branch", Some(icon(IconName::Plus, 12.0).into_any_element()), format!("Create branch “{typed_branch}”"), Some("Switches this checkout to it".into()), None, false, false);
                    choices.add(
                        row,
                        move |ws, window, cx| {
                            ws.dismiss_picker(window, cx);
                            ws.act(act.clone(), window, cx);
                        },
                        cx,
                    )
                });

                let mut br_list: Vec<AnyElement> = Vec::new();
                match &gp.branches {
                    None => {
                        br_list.push(
                            div()
                                .px(px(8.0))
                                .py(px(8.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(scaled(11.5))
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(spinner("git-picker-br-loading", 12.0))
                                .child("Loading branches…")
                                .into_any_element(),
                        );
                    }
                    Some(Err(why)) => {
                        br_list.push(div().px(px(8.0)).py(px(6.0)).text_size(scaled(11.5)).text_color(hsla(theme::DANGER)).child(why.clone()).into_any_element());
                    }
                    Some(Ok(branches)) => {
                        let mut br_shown = 0usize;
                        for (i, br) in branches.iter().enumerate() {
                            let hay = match &br.held_tree {
                                Some((tree, _)) => format!("{} {tree}", br.name),
                                None => br.name.clone(),
                            };
                            if !matches(&hay) {
                                continue;
                            }
                            let current = br.current;
                            let held = br.held_tree.clone();
                            let name = br.name.clone();
                            let dir = cwd.clone();
                            let remove = br.remove.clone();
                            let row_group = gpui::SharedString::from(format!("gp-br-row-{i}"));
                            let act = match &held {
                                Some((_, folder)) => match target {
                                    GitPickerTarget::Home => Act::HomeSelectWorktree(folder.clone()),
                                    GitPickerTarget::Session => Act::SwitchWorktree { folder: folder.clone(), base: Default::default() },
                                },
                                None => Act::SwitchBranch(dir, name),
                            };
                            let lead = icon(IconName::GitBranch, 12.0).text_color(if current { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                            let trailing = (held.is_some() || remove.is_some()).then(|| {
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .when_some(held.map(|(tree, _)| tree), |d, tree| {
                                        d.child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(4.0))
                                                .text_size(scaled(11.0))
                                                .text_color(hsla(theme::TEXT_FAINT))
                                                .child(icon(IconName::FolderGit2, 11.0))
                                                .child(format!("opens {tree}")),
                                        )
                                    })
                                    .children(remove.map(|rm_act| {
                                        icon_button(gpui::SharedString::from(format!("git-picker-br-rm-{i}")), IconName::X)
                                            .invisible()
                                            .group_hover(row_group.clone(), |s| s.visible())
                                            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                                                cx.stop_propagation();
                                                ws.act(rm_act.clone(), window, cx);
                                            }))
                                    }))
                                    .into_any_element()
                            });
                            let row = picker::row(gpui::SharedString::from(format!("git-picker-br-{i}")), Some(lead), br.name.clone(), br.delete_reason.clone(), trailing, current, br.held_missing).group(row_group);
                            br_list.push(if br.held_missing { row.into_any_element() } else { choices.add(
                                row,
                                move |ws, window, cx| {
                                    if current {
                                        ws.dismiss_picker(window, cx);
                                    } else {
                                        ws.act(act.clone(), window, cx);
                                    }
                                },
                                cx,
                            ) });
                            br_shown += 1;
                        }
                        if br_shown == 0 {
                            br_list.push(picker::empty_note("No matching branches.").into_any_element());
                        }
                    }
                }

                // One scrolling list, so Up and Down move through worktrees and branches alike.
                let list = choices
                    .list("git-picker-list", window)
                    .max_h(px(320.0))
                    .child(new_wt_row)
                    .child(div().pt(px(6.0)).child(picker::section_header("WORKTREES", None)))
                    .when(!can_switch, |d| d.child(div().text_size(scaled(11.0)).text_color(hsla(theme::TEXT_FAINT)).child("Resume the session to switch worktrees.")))
                    .children(wt_list)
                    .child(div().py(px(4.0)).child(picker::divider()))
                    .child(picker::section_header("BRANCHES", Some(new_branch_btn)))
                    .children(create_branch_row)
                    .children(br_list);
                let body = div().flex().flex_col().gap(px(8.0)).child(picker::search_input(&gp.search)).child(list);
                ("git-picker-dialog", "BRANCH & WORKTREE", body.into_any_element(), Some(choices))
            }
            WorkspacePicker::SettingsModel { search, text_generation } => {
                let text_generation = *text_generation;
                let Some(Modal::Settings { default_model, text_generation_model, subagents, .. }) = &self.modal else { return None };
                let current = default_model.clone();
                let query = search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let mut all = self.store.data.settings.aggregated_models_for(&subagents.enabled);
                if text_generation {
                    all.retain(|m| !m.id.is_empty() && m.id != "default");
                } else if !all.iter().any(|m| m.harness == current.harness && m.id == current.id) {
                    all.insert(0, current.clone());
                }
                let mut choices = picker::Choices::new(&self.picker_nav, &query);
                let mut list = choices.list("settings-model-list", window).role(gpui::Role::ListBox).aria_label("Models").max_h(px(260.0));
                let mut shown = 0usize;
                if text_generation && words.iter().all(|word| "local summary".contains(word)) {
                    let row = picker::row("settings-model-local", None, "Local summary", None, None, text_generation_model.is_none(), false)
                        .role(gpui::Role::ListBoxOption)
                        .aria_selected(text_generation_model.is_none());
                    list = list.child(choices.add(row, |ws, window, cx| ws.act(Act::PickTextGenerationModel(None), window, cx), cx));
                    shown += 1;
                }
                let mut last_harness = None;
                for (n, m) in all.into_iter().enumerate() {
                    let hay = format!("{} {} {}", m.name, m.id, m.harness.name()).to_lowercase();
                    if !words.iter().all(|w| hay.contains(w)) {
                        continue;
                    }
                    if last_harness != Some(m.harness) {
                        last_harness = Some(m.harness);
                        list = list.child(
                            div()
                                .px(px(8.0))
                                .pt(px(if shown == 0 { 2.0 } else { 6.0 }))
                                .pb(px(2.0))
                                .text_size(scaled(10.5))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(m.harness.name()),
                        );
                    }
                    let selected = if text_generation {
                        text_generation_model.as_ref().is_some_and(|model| model.harness == m.harness && model.id == m.id)
                    } else {
                        m.harness == current.harness && m.id == current.id
                    };
                    let choice = if text_generation {
                        Act::PickTextGenerationModel(Some(crate::store::TextGenerationModel { harness: m.harness, id: m.id.clone() }))
                    } else {
                        Act::PickDefaultModel(m.clone())
                    };
                    let logo = logos::draw(logos::for_harness(m.harness), 11.5, if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row(gpui::SharedString::from(format!("settings-model-opt-{n}")), Some(logo), m.name.clone(), None, None, selected, false)
                        .role(gpui::Role::ListBoxOption)
                        .aria_selected(selected);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(choice.clone(), window, cx), cx));
                    shown += 1;
                }
                if shown == 0 {
                    list = list.child(picker::empty_note("No models match."));
                }
                let loading = self.model_lookups.any_loading(&subagents.enabled);
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(picker::search_input(search))
                    .child(list)
                    .when(loading, |d| {
                        d.child(
                            div()
                                .px(px(6.0))
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_size(scaled(11.0))
                                .text_color(hsla(theme::TEXT_FAINT))
                                .child(spinner("settings-models-loading", 11.0))
                                .child("Loading models…"),
                        )
                    });
                ("settings-model-dialog", if text_generation { "TEXT GENERATION MODEL" } else { "DEFAULT MODEL" }, body.into_any_element(), Some(choices))
            }
            WorkspacePicker::SettingsEffort => {
                let Some(Modal::Settings { effort, .. }) = &self.modal else { return None };
                let current = *effort;
                let mut choices = picker::Choices::new(&self.picker_nav, "");
                let mut list = choices.list("settings-effort-list", window);
                for (i, &opt) in Effort::ALL.iter().enumerate() {
                    let selected = current == opt;
                    let row = picker::row(gpui::SharedString::from(format!("settings-effort-{i}")), None, opt.label(), None, None, selected, false);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::PickEffort(opt), window, cx), cx));
                }
                ("settings-effort-dialog", "EFFORT", list.into_any_element(), Some(choices))
            }
            WorkspacePicker::HomePermission => {
                let current = self.home_selected_permission();
                let offered = self.store.data.settings.permissions_for(self.home_selected_model().harness);
                let mut choices = picker::Choices::new(&self.picker_nav, "");
                let mut list = choices.list("home-perm-list", window).max_h(px(340.0));
                for (i, mode) in offered.into_iter().enumerate() {
                    let selected = current == Some(mode);
                    let lead = icon(IconName::Lock, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row(
                        gpui::SharedString::from(format!("home-perm-{i}")),
                        Some(lead),
                        mode.label(),
                        Some(mode.description().to_string()),
                        None,
                        selected,
                        false,
                    );
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::HomeSelectPermission(mode), window, cx), cx));
                }
                ("home-perm-dialog", "PERMISSION MODE", list.into_any_element(), Some(choices))
            }
            WorkspacePicker::SettingsPermission => {
                let Some(Modal::Settings { permission, .. }) = &self.modal else { return None };
                let current = *permission;
                let mut choices = picker::Choices::new(&self.picker_nav, "");
                let mut list = choices.list("settings-perm-list", window).max_h(px(340.0));
                for (i, mode) in Permission::ALL.into_iter().enumerate() {
                    let selected = current == Some(mode);
                    let lead = icon(IconName::Lock, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row(
                            gpui::SharedString::from(format!("settings-perm-{i}")),
                            Some(lead),
                            mode.label(),
                            Some(mode.description().into()),
                            None,
                            selected,
                            false,
                        );
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::PickPermission(mode), window, cx), cx));
                }
                ("settings-perm-dialog", "PERMISSION MODE", list.into_any_element(), Some(choices))
            }
            WorkspacePicker::SkillsScope { search } => {
                let query = search.read(cx).value().to_lowercase();
                let words: Vec<&str> = query.split_whitespace().collect();
                let matches = |hay: &str| {
                    let low = hay.to_lowercase();
                    words.iter().all(|w| low.contains(w))
                };
                let current = self.effective_skill_scope();
                let mut choices = picker::Choices::new(&self.picker_nav, &query);
                let mut list = choices.list("skills-scope-list", window).role(gpui::Role::ListBox).aria_label("Skill scopes").max_h(px(250.0));
                let mut shown = 0usize;
                if matches("global all workspaces") {
                    let selected = current == SkillScope::Global;
                    let lead = icon(IconName::FolderOpen, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row("skills-scope-global", Some(lead), "Global (all workspaces)", Some("Symlink into global harness skills folders".into()), None, selected, false)
                        .role(gpui::Role::ListBoxOption).aria_selected(selected);
                    list = list.child(choices.add(row, |ws, window, cx| ws.act(Act::SetSkillScope(SkillScope::Global), window, cx), cx));
                    shown += 1;
                }
                for (i, p) in self.store.data.projects.iter().enumerate() {
                    let path_str = short_path(&p.path);
                    if !matches(&format!("{} {path_str}", p.name)) {
                        continue;
                    }
                    let scope = SkillScope::Project(p.id.clone());
                    let selected = current == scope;
                    let lead = icon(IconName::FolderOpen, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row(gpui::SharedString::from(format!("skills-scope-{i}")), Some(lead), p.name.clone(), Some(path_str), None, selected, false)
                        .role(gpui::Role::ListBoxOption).aria_selected(selected);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::SetSkillScope(scope.clone()), window, cx), cx));
                    shown += 1;
                }
                if shown == 0 {
                    list = list.child(crate::panels::widgets::empty_panel(IconName::Search, "No matching projects"));
                }
                let field = gpui_kit::component::input::Input::new(search)
                    .h(px(30.0)).appearance(false).aria_label("Find a project or choose Global");
                let body = div().flex().flex_col().gap(px(5.0))
                    .child(div().flex().items_center().gap(px(7.0)).px(px(8.0)).rounded(px(6.0))
                        .bg(hsla(theme::PANEL_BG)).border_1().border_color(hsla(theme::PANEL_BORDER))
                        .child(icon(IconName::Search, 14.0).text_color(hsla(theme::TEXT_DIM)))
                        .child(div().flex_1().min_w_0().child(field)))
                    .child(list);
                ("skills-scope-dialog", "SKILL SCOPE", body.into_any_element(), Some(choices))
            }
            WorkspacePicker::Editor { target } => {
                let current = self.preferred_editor();
                let target = target.clone();
                let mut choices = picker::Choices::new(&self.picker_nav, "");
                let mut list = choices.list("editor-picker-list", window);
                let mut shown = 0usize;
                for (i, &(ed, found)) in self.editors.iter().enumerate() {
                    if !found {
                        continue;
                    }
                    let selected = current == Some(ed);
                    let path = target.clone();
                    let lead = icon(IconName::ExternalLink, 12.0).text_color(if selected { accent } else { hsla(theme::TEXT_FAINT) }).into_any_element();
                    let row = picker::row(gpui::SharedString::from(format!("editor-picker-{i}")), Some(lead), ed.label(), None, None, selected, false);
                    list = list.child(choices.add(row, move |ws, window, cx| ws.act(Act::OpenEditor(ed, path.clone()), window, cx), cx));
                    shown += 1;
                }
                if shown == 0 {
                    list = list.child(picker::empty_note("No editors found."));
                }
                ("editor-picker-dialog", "OPEN IN EDITOR", list.into_any_element(), Some(choices))
            }
        };

        Some(picker::dropdown_overlay((id, title), at, priority, body, choices, |ws: &mut Self, window, cx| ws.dismiss_picker(window, cx), window, cx))
    }
}
