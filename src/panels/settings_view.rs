//! The Settings dialog's body: the category list on the left and the chosen page on the right.

use rune::tok::motion;
use rune::{Color, Cursor, El, ScrollSpec, Tx, col, el, row};

use crate::app::{Act, AgentForm, Lookup, MenuAnchor, MenuItem, Modal, SettingsPage, Workspace};
use crate::claude::subagents::{Agent, AgentModel, type_name};
use crate::claude::defaults::{Effort, PermissionMode};
use crate::claude::mcp::Feature;
use crate::claude::trim::Trim;
use crate::panels::overlays::{modal_input, modal_input_submitting};
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{button, button_disabled, danger_button, font, label, section_label, tick};

fn faint(t: impl Into<String>) -> El {
    label(t, 11.0, theme::TEXT_FAINT).wrap()
}

/// A labelled text field, field `n` of the modal.
fn field(n: usize, title: &str, edit: &rune::TextEdit, hint: &str) -> El {
    col().gap(4.0).w_full().child(section_label(title.to_string())).child(modal_input(n, edit, hint))
}

/// A row of the settings pages: dark, rounded, its label at the left.
fn setting_row(key: String) -> El {
    row().key(key).h(32.0).w_full().px(10.0).center_y().justify_between().radius(6.0).fill(rgb(0x111113)).border(1.0, rgb(0x242428))
}

/// An on/off switch whose knob slides across.
fn switch(on: bool) -> El {
    row()
        .w(28.0)
        .h(16.0)
        .padding(2.0)
        .radius(8.0)
        .fill(rgb(if on { theme::ACCENT } else { 0x2c2c30 }))
        .justify_start()
        .motion(motion::SNAP)
        .child(el().size(12.0, 12.0).radius(6.0).fill(rgb(if on { 0x17100d } else { 0x8a8a8a })).ml(if on { 12.0 } else { 0.0 }).motion(motion::SNAP))
}

/// An On/Off row that runs `flip` on the workspace when clicked, with an optional faint `tag` after the label.
fn switch_row(key: String, title: &str, tag: Option<&str>, on: bool, flip: impl Fn(&mut Workspace) + 'static) -> El {
    // A long name (a model's, say) gives way and is cut short, so the switch keeps its place at the right.
    let mut left = row().grow(1.0).shrink().pr(10.0).center_y().gap(8.0).child(label(title.to_string(), 13.0, theme::TEXT).shrink().truncate());
    if let Some(t) = tag {
        left = left.child(label(t.to_string(), 11.0, theme::TEXT_FAINT).shrink().truncate());
    }
    let knob = format!("{key}-switch");
    setting_row(key)
        .cursor(Cursor::Pointer)
        .role("switch")
        .hover(Color::WHITE.alpha(0.02))
        .child(left)
        .child(switch(on).key(knob))
        .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            flip(ws);
            ws.after_input(tx);
        })
}

/// An On/Off row in the Settings modal; `field` picks the flag it flips.
fn settings_toggle(key: &str, title: &str, on: bool, field: fn(&mut Modal) -> Option<&mut bool>) -> El {
    switch_row(key.to_string(), title, None, on, move |ws| {
        if let Some(flag) = ws.modal.as_mut().and_then(field) {
            *flag = !*flag;
        }
    })
}

/// One agent's part of the Custom subagents page, edited as `form`.
struct AgentSection<'a> {
    agent: Agent,
    form: &'a AgentForm,
    /// The models the agent offered when the page last asked.
    catalog: &'a [AgentModel],
    lookup: &'a Lookup,
}

/// What the page says about an agent: what it runs on and needs, and what Allow edits on and off let it do.
fn agent_texts(agent: Agent) -> (&'static str, &'static str, &'static str) {
    match agent {
        Agent::Codex => (
            "Runs the task in OpenAI Codex through codex-acp. Needs Node (npx) and a `codex login`; the first run downloads codex-acp from npm.",
            "Claudiu accepts everything Codex asks for: editing files, running commands, writing outside the folder and using the network. None of it goes through Claude Code's permission rules or hooks.",
            "Codex runs read-only: it can read and search, but not change files.",
        ),
        Agent::OpenCode => (
            "Runs the task in OpenCode (`opencode acp`), on the providers you set up in OpenCode. Needs `opencode` on PATH.",
            "Claudiu accepts everything OpenCode asks for, and OpenCode's own permission settings decide the rest: it can edit files, run commands and use the network. None of it goes through Claude Code's permission rules or hooks.",
            "OpenCode runs read-only: it can read, search and list files, but not change them or run any command, not even a read-only one.",
        ),
    }
}

/// The models to list: those the agent offers, then any switched on that it no longer offers, so they can be
/// switched off. The flag says whether the agent still offers the model.
fn model_rows(catalog: &[AgentModel], on: &[String]) -> Vec<(AgentModel, bool)> {
    let gone = on.iter().filter(|id| !catalog.iter().any(|m| &m.id == *id)).map(|id| AgentModel { id: id.clone(), name: id.clone() });
    catalog.iter().cloned().map(|m| (m, true)).chain(gone.map(|m| (m, false))).collect()
}

/// How many of the models the agent offers are switched on, as "2 of 107 on". A model switched on that the agent no
/// longer offers isn't one of them.
fn on_count(catalog: &[AgentModel], on: &[String]) -> String {
    format!("{} of {} on", catalog.iter().filter(|m| on.contains(&m.id)).count(), catalog.len())
}

/// Whether `model` fits what was typed in the picker's search: every word, in its name or its id, ignoring case.
fn model_matches(query: &str, model: &AgentModel) -> bool {
    let haystack = format!("{} {}", model.name, model.id).to_lowercase();
    query.to_lowercase().split_whitespace().all(|word| haystack.contains(word))
}

/// A model's switch, keyed `key`, on the page or in the picker; it flips the model in the agent's form.
fn model_switch(key: String, agent: Agent, model: &AgentModel, offered: bool, on: bool) -> Option<El> {
    let name = type_name(agent, &model.id)?;
    let tag = if offered { name } else { format!("{name} · no longer offered") };
    let id = model.id.clone();
    Some(switch_row(key, &model.name, Some(&tag), on, move |ws| {
        if let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() {
            flip(&mut subagents.get_mut(agent).models, id.clone());
        }
    }))
}

/// Where the agent's model lookup stands, when there is something to say.
fn lookup_status(agent: Agent, catalog: &[AgentModel], lookup: &Lookup) -> Option<El> {
    let name = agent.name();
    if lookup.loading && catalog.is_empty() {
        Some(faint(format!("Loading {name}'s models…")))
    } else {
        lookup.error.as_ref().map(|why| label(format!("Couldn't load {name}'s models: {why}"), 11.0, theme::WARN).wrap())
    }
}

/// An agent's switch and, once it is on, the models switched on, the button to the model picker, and whether it may
/// edit.
fn agent_section(s: AgentSection) -> El {
    let AgentSection { agent, form, catalog, lookup } = s;
    let (about, edits_on, edits_off) = agent_texts(agent);
    let key = agent.key();
    let mut section = col()
        .gap(8.0)
        .child(section_label(agent.name().to_uppercase()).mt(4.0))
        .child(faint(about))
        .child(switch_row(format!("settings-{key}"), &format!("{} subagents", agent.name()), None, form.on, move |ws| {
            if let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() {
                let form = subagents.get_mut(agent);
                form.on = !form.on;
            }
            ws.load_models();
        }));
    if !form.on {
        return section;
    }
    for (model, offered) in model_rows(catalog, &form.models).into_iter().filter(|(m, _)| form.models.contains(&m.id)) {
        section = section.children(model_switch(format!("settings-{key}-model-{}", model.id), agent, &model, offered, true));
    }
    if let Some(status) = lookup_status(agent, catalog, lookup) {
        section = section.child(status);
    } else if form.models.is_empty() {
        section = section.child(faint(format!("Choose a model to give Claude a {} subagent for it.", agent.name())));
    }
    let count = if catalog.is_empty() { String::new() } else { on_count(catalog, &form.models) };
    section
        .child(
            row()
                .w_full()
                .center_y()
                .justify_between()
                .child(button("Choose models…", false).key(format!("settings-{key}-choose")).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                    ws.open_model_picker(agent);
                    ws.after_input(tx);
                }))
                .child(faint(count)),
        )
        .child(switch_row(format!("settings-{key}-edits"), "Allow edits", None, form.edits, move |ws| {
            if let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() {
                let form = subagents.get_mut(agent);
                form.edits = !form.edits;
            }
        }))
        .child(faint(if form.edits { edits_on } else { edits_off }))
}

/// A row that opens a menu of `options` under itself; the first entry, "Claude Code default", clears the choice.
fn picker_row<T: Copy + PartialEq + 'static>(key: &'static str, title: &str, current: Option<T>, options: &'static [T], name: fn(T) -> &'static str, pick: fn(Option<T>) -> Act) -> El {
    const DEFAULT: &str = "Claude Code default";
    setting_row(key.to_string())
        .cursor(Cursor::Pointer)
        .role("button")
        .hover(Color::WHITE.alpha(0.02))
        .child(label(title.to_string(), 13.0, theme::TEXT))
        .child(label(current.map_or(DEFAULT, name), 13.0, if current.is_some() { theme::ACCENT } else { theme::TEXT_FAINT }))
        .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            let items = std::iter::once(MenuItem::new(format!("{}{DEFAULT}", tick(current.is_none())), pick(None)))
                .chain(options.iter().map(|&o| MenuItem::new(format!("{}{}", tick(current == Some(o)), name(o)), pick(Some(o)))))
                .collect();
            ws.open_menu(MenuAnchor::Key(key.into()), 200.0, items);
            ws.after_input(tx);
        })
}

/// Adds `item` to `items`, or removes it when it is there.
fn flip<T: PartialEq>(items: &mut Vec<T>, item: T) {
    match items.iter().position(|i| *i == item) {
        Some(at) => {
            items.remove(at);
        }
        None => items.push(item),
    }
}

/// A − / + button that moves the scale by `by` percent, within the allowed range.
fn scale_step(key: &'static str, sign: &'static str, by: i32) -> El {
    row()
        .key(key)
        .size(22.0, 22.0)
        .center()
        .radius(4.0)
        .cursor(Cursor::Pointer)
        .role("button")
        .hover(Color::WHITE.alpha(0.08))
        .press_scale(0.9)
        .child(label(sign, 13.0, theme::TEXT))
        .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            if let Some(Modal::Settings { scale, .. }) = ws.modal.as_mut() {
                *scale = (*scale as i32 + by).clamp(crate::panels::scale::MIN as i32, crate::panels::scale::MAX as i32) as u32;
            }
            ws.after_input(tx);
        })
}

impl Workspace {
    /// The model picker over the Custom subagents page: a search field and a switch for every model the agent offers
    /// that fits it. Its switches edit the page's form, so Settings' Save and Cancel decide what is kept.
    pub(crate) fn model_picker(&self) -> Option<El> {
        let Some(Modal::Settings { subagents, .. }) = &self.modal else { return None };
        let (agent, search) = subagents.picking.as_ref()?;
        let agent = *agent;
        let form = subagents.get(agent);
        let catalog = &self.store.data.settings.custom_subagents.get(agent).catalog;
        let query = search.text.as_str();
        let mut list = col().gap(6.0);
        let mut shown = 0;
        for (model, offered) in model_rows(catalog, &form.models).into_iter().filter(|(m, _)| model_matches(query, m)) {
            if let Some(row) = model_switch(format!("model-picker-{}", model.id), agent, &model, offered, form.models.contains(&model.id)) {
                list = list.child(row);
                shown += 1;
            }
        }
        if let Some(status) = lookup_status(agent, catalog, self.model_lookups.get(agent)) {
            list = list.child(status);
        } else if shown == 0 {
            list = list.child(faint(format!("No model matches “{}”.", query.trim())));
        }
        let count = on_count(catalog, &form.models);
        let card = col()
            .key("model-picker")
            .w(crate::panels::scale::px(520.0))
            .h(crate::panels::scale::px(440.0))
            .padding(20.0)
            .gap(12.0)
            .radius(12.0)
            .fill(rgb(0x151517))
            .border(1.0, rgb(0x2c2c30))
            .shadow(16.0, 48.0, Color::BLACK.alpha(0.55))
            .role("dialog")
            .transition(rune::Transition::scale(0.96).combined(rune::Transition::opacity()))
            .child(rune::text(format!("{} models", agent.name())).font(font(15.0, 600.0)).color(rgb(theme::TEXT)))
            .child(modal_input_submitting(5, search, "Search models", Workspace::close_model_picker))
            .child(col().key("model-picker-list").grow(1.0).shrink().min_h(0.0).w_full().scroll(ScrollSpec { stick_bottom: false }).child(list.w_full().pr(6.0)))
            .child(
                row().w_full().center_y().justify_between().child(faint(count)).child(button("Done", true).key("model-picker-done").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                    ws.close_model_picker();
                    ws.after_input(tx);
                })),
            );
        Some(
            row()
                .key("model-picker-backdrop")
                .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .center()
                .fill(Color::BLACK.alpha(0.4))
                .transition(rune::Transition::opacity())
                .modal(true, |ws: &mut Workspace| ws.close_model_picker())
                .child(card),
        )
    }

    pub(crate) fn settings_body(&self) -> El {
        let Some(Modal::Settings { shell, claude, model, effort, permission, updates, chrome, scale, trimmed, claudiu_off, skills_field, costly, hints, renaming, subagents }) = &self.modal else {
            return el();
        };
        let page = self.settings_page;
        let content = match page {
            SettingsPage::General => col()
                .gap(10.0)
                .child(field(0, "SHELL", shell, &hints[0]))
                .child(field(1, "CLAUDE CODE PATH", claude, &hints[1]))
                .child(row().child(button("Check setup", false).key("settings-check-setup").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                    ws.act(Act::OpenSetup);
                    ws.after_input(tx);
                })))
                .child(
                    setting_row("settings-scale".into()).child(label("Scale (text and terminal)", 13.0, theme::TEXT)).child(
                        row()
                            .center_y()
                            .gap(10.0)
                            .child(scale_step("scale-down", "−", -(crate::panels::scale::STEP as i32)))
                            .child(row().w(40.0).justify_center().child(label(format!("{scale}%"), 13.0, theme::TEXT)))
                            .child(scale_step("scale-up", "+", crate::panels::scale::STEP as i32)),
                    ),
                )
                .child(settings_toggle("settings-updates", "Check for updates", *updates, |m| match m {
                    Modal::Settings { updates, .. } => Some(updates),
                    _ => None,
                })),
            SettingsPage::Sessions => {
                let mut c = col()
                    .gap(10.0)
                    .child(field(2, "MODEL FOR NEW SESSIONS", model, &hints[2]))
                    .child(picker_row("settings-effort", "Effort", *effort, &Effort::ALL, Effort::name, Act::PickEffort))
                    .child(picker_row("settings-permission", "Permission mode", *permission, &PermissionMode::ALL, PermissionMode::name, Act::PickPermission));
                if *permission == Some(PermissionMode::BypassPermissions) {
                    c = c.child(faint("Every new session will run commands and edit files without asking."));
                }
                if *permission == Some(PermissionMode::Plan) && trimmed.contains(&Trim::PlanMode) {
                    c = c.child(faint("Plan mode is off under Claude Code tools, so new sessions start in Claude Code's default mode."));
                }
                c.child(settings_toggle("settings-chrome", "Claude in Chrome for new sessions", *chrome, |m| match m {
                    Modal::Settings { chrome, .. } => Some(chrome),
                    _ => None,
                }))
            }
            SettingsPage::ClaudeTools => {
                let mut list = col().gap(8.0);
                for found in costly {
                    list = list.child(label(format!("{} is set in {}. {}", found.name, found.source, found.effect), 11.0, theme::WARN).wrap());
                }
                list = list.child(faint(
                    "Applies to new sessions. Running sessions keep their tools. Resumed sessions get the new tools, and the old instructions stay until their next compaction. These switches edit Claude Code's own settings.json, for every account and for Claude Code outside Claudiu. Claudiu only turns things off; anything you turned off yourself stays off.",
                ));
                for (heading, items) in Trim::GROUPS {
                    list = list.child(section_label(heading.to_string()).mt(4.0));
                    for &item in items {
                        list = list.child(switch_row(format!("settings-trim-{}", item as usize), item.label(), Some(item.saving().label()), !trimmed.contains(&item), move |ws| {
                            if let Some(Modal::Settings { trimmed, .. }) = ws.modal.as_mut() {
                                flip(trimmed, item);
                            }
                        }));
                    }
                }
                if trimmed.contains(&Trim::PlanMode) {
                    list = list.child(faint("With Plan mode off, Claude can't leave plan mode itself; press Shift+Tab."));
                }
                list.child(section_label("SKILLS").mt(4.0))
                    .child(faint("Claude no longer sees these skills, but you can still type them. A setting you already have for a skill in Claude Code's settings.json wins."))
                    .child(field(3, "SKILLS TO TURN OFF, SEPARATED BY COMMAS", skills_field, &hints[3]))
            }
            SettingsPage::ClaudiuTools => {
                let mut list = col().gap(8.0).child(faint(
                    "Applies to new sessions. Running sessions keep their tools. Resumed sessions get the new tools, and the old instructions stay until their next compaction.",
                ));
                for feature in Feature::ALL {
                    list = list.child(switch_row(format!("settings-claudiu-{}", feature as usize), feature.label(), None, !claudiu_off.contains(&feature), move |ws| {
                        if let Some(Modal::Settings { claudiu_off, .. }) = ws.modal.as_mut() {
                            flip(claudiu_off, feature);
                        }
                    }));
                }
                list
            }
            SettingsPage::CustomSubagents => {
                let saved = &self.store.data.settings.custom_subagents;
                let mut page = col().gap(8.0).child(faint(
                    "Agent types that run a task in another coding agent instead of Claude. Claude uses one only when you ask for it. Applies to new and resumed sessions. Running sessions keep theirs.",
                ));
                for agent in Agent::ALL {
                    page = page.child(agent_section(AgentSection { agent, form: subagents.get(agent), catalog: &saved.get(agent).catalog, lookup: self.model_lookups.get(agent) }));
                }
                page
            }
            SettingsPage::Projects => self.projects_page(renaming.as_ref()),
            SettingsPage::Worktrees => self.worktrees_page(),
            SettingsPage::About => col().child(faint(format!(
                "Claudiu {} · An independent project, not affiliated with, endorsed by or sponsored by Anthropic. Claude is a trademark of Anthropic.",
                env!("CARGO_PKG_VERSION")
            ))),
        };
        // The highlight under the chosen category slides to it.
        let item_h = crate::panels::scale::px(28.0);
        let at = SettingsPage::ALL.iter().position(|p| *p == page).unwrap_or(0) as f32;
        let mut categories = col()
            .key("settings-categories")
            .w(crate::panels::scale::px(160.0))
            .gap(2.0)
            .child(el().key("settings-highlight").absolute(0.0, at * (item_h + 2.0)).w_full().h(item_h).radius(6.0).fill(rgb(0x1f1f22)).motion(motion::SNAP));
        for p in SettingsPage::ALL {
            let selected = p == page;
            categories = categories.child(
                row()
                    .key(format!("settings-category-{}", p.label()))
                    .h(item_h)
                    .w_full()
                    .px(10.0)
                    .center_y()
                    .radius(6.0)
                    .cursor(Cursor::Pointer)
                    .role("tab")
                    .hover(Color::WHITE.alpha(0.03))
                    .child(label(p.label(), 13.0, if selected { theme::ACCENT } else { theme::TEXT_DIM }))
                    .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                        ws.set_settings_page(p);
                        // The category list has the keyboard again: Up and Down move through it.
                        ws.blur = true;
                        ws.after_input(tx);
                    }),
            );
        }
        row()
            .key("settings-body")
            .grow(1.0)
            .shrink()
            .min_h(0.0)
            .w_full()
            .gap(16.0)
            .child(categories)
            .child(col().key(format!("settings-page-{}", page.label())).grow(1.0).shrink().min_w(0.0).h_full().scroll(ScrollSpec { stick_bottom: false }).transition(rune::Transition::opacity()).child(content.w_full().pr(6.0)))
    }

    /// The Projects page: every project in the sidebar's order, to rename, move, remove, or add to. Changes apply at
    /// once, as on the Worktrees page; a removal can be undone from its toast.
    fn projects_page(&self, renaming: Option<&(crate::store::Id, rune::TextEdit)>) -> El {
        let projects = &self.store.data.projects;
        let add = button("Add project…", false).key("project-add").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
            ws.act(Act::ImportProject);
            ws.after_input(tx);
        });
        let mut list = col().gap(8.0).child(
            row()
                .w_full()
                .center_y()
                .justify_between()
                .gap(10.0)
                .child(faint("Removing a project keeps its sessions; they show under no project.").grow(1.0).shrink())
                .child(add),
        );
        if projects.is_empty() {
            return list.child(faint("No projects yet."));
        }
        let pressed = |key: String, b: El, a: Act| {
            b.key(key).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                ws.act(a.clone());
                ws.after_input(tx);
            })
        };
        let last = projects.len() - 1;
        for (i, p) in projects.iter().enumerate() {
            let id = p.id.clone();
            let name = match renaming.filter(|(r, _)| *r == p.id) {
                Some((_, edit)) => modal_input_submitting(4, edit, "Name", |ws| ws.act(Act::SaveProjectName)),
                None => label(p.name.clone(), 13.0, theme::TEXT),
            };
            let mut buttons = row().gap(6.0);
            buttons = if renaming.is_some_and(|(r, _)| *r == p.id) {
                buttons.child(pressed(format!("project-cancel-{i}"), button("Cancel", false), Act::CancelProjectRename)).child(pressed(format!("project-save-{i}"), button("Save", true), Act::SaveProjectName))
            } else {
                buttons.child(pressed(format!("project-rename-{i}"), button("Rename", false), Act::RenameProject(id.clone())))
            };
            buttons = buttons
                .child(if i == 0 { button_disabled("Move up").key(format!("project-up-{i}")) } else { pressed(format!("project-up-{i}"), button("Move up", false), Act::MoveProject(id.clone(), true)) })
                .child(if i == last { button_disabled("Move down").key(format!("project-down-{i}")) } else { pressed(format!("project-down-{i}"), button("Move down", false), Act::MoveProject(id.clone(), false)) })
                .child(pressed(format!("project-remove-{i}"), button("Remove", false), Act::RemoveProject(id)));
            list = list.child(
                row()
                    // By id, so a moved row keeps its element and slides to its new place.
                    .key(format!("project-{}", p.id))
                    .w_full()
                    .center_y()
                    .justify_between()
                    .gap(10.0)
                    .px(10.0)
                    .py(6.0)
                    .radius(6.0)
                    .fill(rgb(0x111113))
                    .border(1.0, rgb(0x242428))
                    .child(col().grow(1.0).shrink().min_w(0.0).gap(2.0).child(name).child(label(p.path.to_string_lossy(), 11.0, theme::TEXT_FAINT).truncate_middle()))
                    .child(buttons),
            );
        }
        list
    }

    /// The Worktrees page: the worktree folders Claudiu made, by repository, each to show in the file manager or remove.
    fn worktrees_page(&self) -> El {
        let page = &self.worktrees_page;
        let reveal = if cfg!(target_os = "macos") { "Show in Finder" } else { "Show in File Explorer" };
        let mut list = col().gap(8.0).child(faint(
            "Worktrees Claudiu made live in its own data folder, outside your repositories. Removing one here asks the same question as the x in the top-bar menu.",
        ));
        let Some(stored) = &page.stored else { return list.child(faint("Looking…")) };
        if stored.is_empty() {
            return list.child(faint("No worktrees yet. Make one from the worktree button in the top bar."));
        }
        let mut group: Option<&std::path::Path> = None;
        for (i, item) in stored.iter().enumerate() {
            let repo_folder = item.folder.parent();
            if group != repo_folder {
                group = repo_folder;
                let heading = match &item.main_checkout {
                    Some(main) => main.to_string_lossy().into_owned(),
                    None => format!("{} (repository not found)", repo_folder.and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy()),
                };
                list = list.child(section_label(heading).mt(4.0));
            }
            let name = item.folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let revealed = item.folder.clone();
            let ask = match item.main_checkout.clone() {
                Some(main_checkout) => Act::RemoveWorktree { main_checkout, folder: item.folder.clone() },
                None => Act::AskDeleteUnlisted(item.folder.clone()),
            };
            let mut r = col()
                .key(format!("worktree-{i}"))
                .gap(6.0)
                .px(10.0)
                .py(6.0)
                .radius(6.0)
                .fill(rgb(0x111113))
                .border(1.0, rgb(0x242428))
                .child(
                    row()
                        .w_full()
                        .center_y()
                        .justify_between()
                        .gap(10.0)
                        .child(col().grow(1.0).shrink().min_w(0.0).child(label(name, 13.0, theme::TEXT)).child(label(item.folder.to_string_lossy(), 11.0, theme::TEXT_FAINT).truncate_middle()))
                        .child(
                            row()
                                .gap(6.0)
                                .child(button(reveal, false).key(format!("worktree-reveal-{i}")).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                                    ws.act(Act::Reveal(revealed.clone()));
                                    ws.after_input(tx);
                                }))
                                .child(button("Remove", false).key(format!("worktree-remove-{i}")).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                                    ws.act(ask.clone());
                                    ws.after_input(tx);
                                })),
                        ),
                );
            if let Some(asked) = page.asking.as_ref().filter(|a| a.folder == item.folder) {
                let a = asked.act.clone();
                let confirm = if asked.danger { danger_button(asked.confirm.clone()) } else { button(asked.confirm.clone(), true) };
                r = r.child(faint(asked.body.clone())).child(
                    row()
                        .gap(6.0)
                        .child(button("Cancel", false).key(format!("worktree-cancel-{i}")).on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                            ws.worktrees_page.asking = None;
                            ws.after_input(tx);
                        }))
                        .child(confirm.key(format!("worktree-confirm-{i}")).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                            ws.act(a.clone());
                            ws.after_input(tx);
                        })),
                );
            }
            list = list.child(r);
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, name: &str) -> AgentModel {
        AgentModel { id: id.into(), name: name.into() }
    }

    #[test]
    fn every_word_of_the_search_must_match_the_name_or_the_id() {
        let sol = model("openai/gpt-6.1-sol", "openai/GPT-6.1 Sol");
        assert!(model_matches("", &sol));
        assert!(model_matches("  GPT sol ", &sol));
        assert!(model_matches("openai 6.1", &sol));
        assert!(!model_matches("gpt luna", &sol));
        assert!(!model_matches("anthropic", &sol));
    }

    #[test]
    fn the_count_leaves_out_models_no_longer_offered() {
        let catalog = [model("a", "A"), model("b", "B")];
        let on = ["a".to_string(), "gone-1".into(), "gone-2".into()];
        assert_eq!(on_count(&catalog, &on), "1 of 2 on");
    }

    #[test]
    fn models_no_longer_offered_come_last_and_marked() {
        let rows = model_rows(&[model("a", "A")], &["gone".to_string(), "a".into()]);
        assert_eq!(rows, vec![(model("a", "A"), true), (model("gone", "gone"), false)]);
    }
}
