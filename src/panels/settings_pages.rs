//! The Settings pages that list things to act on: Projects, and Harnesses (Claude Code, Codex, OpenCode, Antigravity)
//! with each harness's configuration popup (Claude Code tools & accounts; subagent opt-in and models for the others).

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, StatefulInteractiveElement,
    Styled, Window, deferred, div, px, rgba,
};
use gpui_kit::component::input::{Input, InputState};

use crate::app::{Act, HarnessDialog, Lookup, Modal, Workspace};
use crate::claude::subagents::{Agent, AgentModel, type_name};
use crate::claude::trim::Trim;
use crate::harness::Harness;
use crate::panels::logos;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, button_disabled, section_label};

fn faint(text: impl Into<gpui::SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(text.into())
}

fn warn(text: impl Into<gpui::SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::WARN)).child(text.into())
}

/// An On/Off row that runs `flip` on the workspace when clicked, with an optional faint `tag` after the label.
pub fn switch_row(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    tag: Option<gpui::SharedString>,
    on: bool,
    flip: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    cx: &mut Context<Workspace>,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .min_h(px(34.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .rounded(px(7.0))
        .bg(hsla(theme::SURFACE_ELEVATED))
        .border_1()
        .border_color(hsla(theme::PANEL_BORDER))
        .text_size(crate::panels::scale::px(13.0))
        .cursor_pointer()
        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(8.0))
                .child(label.into())
                .children(tag.map(|tag| div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(tag))),
        )
        .child(div().flex_none().font_weight(FontWeight::MEDIUM).text_color(hsla(if on { theme::ACCENT } else { theme::TEXT_FAINT })).child(if on { "On" } else { "Off" }))
        .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
            flip(ws, window, cx);
            cx.notify();
        }))
}

/// Adds `item` to `items`, or removes it when it is there.
pub fn flip<T: PartialEq>(items: &mut Vec<T>, item: T) {
    match items.iter().position(|i| *i == item) {
        Some(at) => {
            items.remove(at);
        }
        None => items.push(item),
    }
}

/// Flips whether `harness` is enabled as a provider in Claudiu. Ensures at least one harness stays enabled.
fn flip_harness_enabled(ws: &mut Workspace, harness: Harness) {
    let Some(Modal::Settings { subagents, harness: default_harness, .. }) = ws.modal.as_mut() else { return };
    if subagents.enabled.contains(&harness) {
        if subagents.enabled.len() <= 1 {
            return;
        }
        subagents.enabled.retain(|&h| h != harness);
        if *default_harness == harness
            && let Some(&next) = subagents.enabled.first()
        {
            *default_harness = next;
        }
    } else {
        subagents.enabled.push(harness);
    }
}

/// Flips an agent's "Allow use as subagents" switch inside its harness popup.
fn flip_subagent(ws: &mut Workspace, agent: Agent, cx: &mut Context<Workspace>) {
    let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() else { return };
    let form = subagents.get_mut(agent);
    form.on = !form.on;
    ws.load_models(cx);
}

fn agent_for_harness(harness: Harness) -> Option<Agent> {
    match harness {
        Harness::Claude => None,
        Harness::Codex => Some(Agent::Codex),
        Harness::OpenCode => Some(Agent::OpenCode),
        Harness::Antigravity => Some(Agent::Antigravity),
    }
}

fn harness_status(harness: Harness, enabled: bool, subagents: &crate::app::HarnessesForm, trimmed: &[Trim], lookup: Option<&Lookup>) -> (String, u32) {
    if !enabled {
        return ("Disabled".into(), theme::TEXT_FAINT);
    }
    match agent_for_harness(harness) {
        None => {
            if trimmed.is_empty() {
                ("Enabled · Claude Agent SDK · all tools on".into(), theme::TEXT_DIM)
            } else {
                let n = trimmed.len();
                let noun = if n == 1 { "trim" } else { "trims" };
                (format!("Enabled · {n} tool {noun} off"), theme::TEXT_DIM)
            }
        }
        Some(agent) => {
            let form = subagents.get(agent);
            if !form.on {
                return ("Enabled · primary sessions only".into(), theme::TEXT_DIM);
            }
            if lookup.is_some_and(|l| l.error.is_some()) {
                return (format!("Enabled · couldn't load {}'s subagent models", agent.name()), theme::WARN);
            }
            let access = if form.edits { "edits allowed" } else { "read-only" };
            match form.models.len() {
                0 => ("Enabled · subagents on, choose a model".into(), theme::WARN),
                1 => (format!("Enabled · 1 subagent · {access}"), theme::TEXT_DIM),
                n => (format!("Enabled · {n} subagents · {access}"), theme::TEXT_DIM),
            }
        }
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

/// Where the agent's model lookup stands, when there is something to say.
fn lookup_status(agent: Agent, catalog: &[AgentModel], lookup: &Lookup) -> Option<Div> {
    let name = agent.name();
    if lookup.loading && catalog.is_empty() {
        Some(faint(format!("Loading {name}'s models…")))
    } else {
        lookup.error.as_ref().map(|why| warn(format!("Couldn't load {name}'s models: {why}")))
    }
}

impl Workspace {
    /// The Harnesses page: enable Claude Code, Codex, OpenCode and Antigravity, and open each provider's configuration
    /// popup (Claude Code tools & accounts; subagent opt-in and models for the others).
    pub(crate) fn harnesses_page(&self, cx: &mut Context<Self>) -> Div {
        let Some(Modal::Settings { subagents, trimmed, .. }) = &self.modal else { return div() };
        let saved = &self.store.data.settings.custom_subagents;
        let mut page = div().flex().flex_col().gap(px(10.0)).child(faint(
            "Choose which coding agent harnesses are enabled across Claudiu. Enabled harnesses appear in the welcome screen, in-chat switcher, and sidebar usage meters. Open any harness to configure its tools or allow using it as a subagent.",
        ));
        for harness in Harness::ALL {
            let enabled = subagents.enabled.contains(&harness);
            let agent = agent_for_harness(harness);
            let lookup = agent.map(|a| self.model_lookups.get(a));
            let (status, tone) = harness_status(harness, enabled, subagents, trimmed, lookup);
            let color = theme::harness_color(harness);

            let mut about = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(3.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(logos::draw(logos::for_harness(harness), 15.0, hsla(if enabled { color } else { theme::TEXT_FAINT })))
                        .child(div().text_size(crate::panels::scale::px(13.5)).font_weight(FontWeight::SEMIBOLD).child(harness.name())),
                )
                .child(div().text_size(crate::panels::scale::px(11.5)).text_color(hsla(tone)).child(status));
            if let Some(agent) = agent {
                let form = subagents.get(agent);
                let catalog = &saved.get(agent).catalog;
                if enabled && form.on && !form.models.is_empty() {
                    let names: Vec<&str> = form.models.iter().map(|id| catalog.iter().find(|m| &m.id == id).map_or(id.as_str(), |m| m.name.as_str())).collect();
                    about = about.child(faint(names.join(" · ")).overflow_hidden().text_ellipsis().whitespace_nowrap());
                }
                if let Some(warning) = agent.provider().warning
                    && form.on
                {
                    about = about.child(warn(warning));
                }
            }
            let key = harness.name().to_lowercase().replace(' ', "-");
            let open = div()
                .id(gpui::SharedString::from(format!("settings-harness-{key}-open")))
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(10.0))
                .cursor_pointer()
                .child(about)
                .child(div().text_size(crate::panels::scale::px(12.0)).text_color(hsla(theme::TEXT_DIM)).child("Configure ›"))
                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                    ws.open_harness_dialog(harness, window, cx);
                    cx.notify();
                }));
            let toggle = div()
                .id(gpui::SharedString::from(format!("settings-harness-{key}")))
                .pl(px(14.0))
                .flex()
                .items_center()
                .cursor_pointer()
                .text_size(crate::panels::scale::px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(if enabled { color } else { theme::TEXT_FAINT }))
                .child(if enabled { "On" } else { "Off" })
                .on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
                    flip_harness_enabled(ws, harness);
                    cx.notify();
                }));
            page = page.child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .px(px(14.0))
                    .py(px(11.0))
                    .rounded(px(8.0))
                    .bg(hsla(theme::SURFACE_ELEVATED))
                    .border_1()
                    .border_color(hsla(if enabled { theme::PANEL_BORDER } else { theme::BORDER }))
                    .child(open)
                    .child(toggle),
            );
        }
        page
    }

    /// Popup dialog over the Harnesses page: Claude Code tools & accounts, or an external provider's subagent controls.
    pub(crate) fn render_harness_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Modal::Settings { claude, trimmed, skills_field, costly, subagents, .. }) = &self.modal else { return None };
        let picking = subagents.picking.as_ref()?;
        let viewport = window.viewport_size();
        let scaled = crate::panels::scale::px;

        let body: Div = match picking {
            HarnessDialog::Claude => {
                let enabled = subagents.enabled.contains(&Harness::Claude);
                let plan_trimmed = trimmed.contains(&Trim::PlanMode);
                let mut col = div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .w_full()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(logos::draw(&logos::CLAUDE, 16.0, hsla(theme::ACCENT)))
                            .child(div().text_size(scaled(15.0)).font_weight(FontWeight::SEMIBOLD).child("Claude Code")),
                    )
                    .child(faint("Runs through Anthropic's Claude Agent SDK with your local `claude` CLI."))
                    .child(switch_row(
                        "harness-dialog-claude-on",
                        "Enable Claude Code",
                        None,
                        enabled,
                        |ws, _, _| flip_harness_enabled(ws, Harness::Claude),
                        cx,
                    ))
                    .child(div().flex().flex_col().gap(px(4.0)).child(section_label("CLAUDE CODE PATH")).child(Input::new(claude)));

                // Claude Code accounts (moved out of the sidebar footer).
                let active_id = self.store.active_account().map(|a| a.id.clone());
                let mut accounts_box = div().flex().flex_col().gap(px(6.0)).child(div().mt(px(4.0)).child(section_label("ACCOUNTS")));
                for (i, acct) in self.store.data.accounts.iter().enumerate() {
                    let id = acct.id.clone();
                    let active = active_id.as_deref() == Some(acct.id.as_str());
                    let sign_id = id.clone();
                    let remove_id = id.clone();
                    let mut row = div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .px(px(10.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .bg(hsla(theme::SURFACE_ELEVATED))
                        .border_1()
                        .border_color(hsla(if active { theme::ACCENT } else { theme::PANEL_BORDER }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .min_w_0()
                                .flex_1()
                                .child(div().text_size(scaled(12.5)).text_color(hsla(theme::TEXT)).child(acct.name()))
                                .child(faint(if acct.is_main() { "Main" } else { "Alt" })),
                        );
                    let mut btns = div().flex().gap(px(6.0)).flex_none();
                    if active {
                        btns = btns.child(button_disabled(format!("acct-use-{i}"), "Active"));
                    } else {
                        btns = btns.child(button(format!("acct-use-{i}"), "Use", false).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                            ws.act(Act::SwitchAccount(id.clone()), window, cx)
                        })));
                    }
                    if !acct.is_main() {
                        btns = btns
                            .child(button(format!("acct-signin-{i}"), "Sign in", false).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                                ws.act(Act::SignIn(sign_id.clone()), window, cx)
                            })))
                            .child(button(format!("acct-rm-{i}"), "Remove", false).on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                                ws.act(Act::RemoveAccount(remove_id.clone()), window, cx)
                            })));
                    }
                    row = row.child(btns);
                    accounts_box = accounts_box.child(row);
                }
                accounts_box = accounts_box.child(div().flex().child(button("acct-add", "Add account…", false).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                    ws.act(Act::AddAccount, window, cx)
                }))));
                col = col.child(accounts_box);

                // Claude Code tools (moved from the old Claude Code tools page).
                col = col.child(div().mt(px(6.0)).child(section_label("CLAUDE CODE TOOLS")));
                for found in costly {
                    col = col.child(warn(format!("{} is set in {}. {}", found.name, found.source, found.effect)));
                }
                col = col.child(faint(
                    "Applies to new sessions. These switches edit Claude Code's settings.json for every account. Claudiu only turns things off; anything you turned off yourself stays off.",
                ));
                for (heading, items) in Trim::GROUPS {
                    col = col.child(div().mt(px(4.0)).child(section_label(heading.to_string())));
                    for &item in items {
                        let tag = Some(item.saving().label().into());
                        col = col.child(switch_row(
                            ("settings-trim", item as usize),
                            item.label(),
                            tag,
                            !trimmed.contains(&item),
                            move |ws, _, _| {
                                if let Some(Modal::Settings { trimmed, .. }) = ws.modal.as_mut() {
                                    flip(trimmed, item);
                                }
                            },
                            cx,
                        ));
                    }
                }
                if plan_trimmed {
                    col = col.child(faint("With Plan mode off, Claude can't leave plan mode itself; press Shift+Tab."));
                }
                col.child(div().mt(px(4.0)).child(section_label("SKILLS TO TURN OFF IN CLAUDE CODE")))
                    .child(faint("Comma-separated skill names to hide from Claude Code's context. Existing entries in settings.json win."))
                    .child(Input::new(skills_field))
            }
            HarnessDialog::Agent(agent, search) => {
                let agent = *agent;
                let harness = match agent {
                    Agent::Codex => Harness::Codex,
                    Agent::OpenCode => Harness::OpenCode,
                    Agent::Antigravity => Harness::Antigravity,
                };
                let provider = agent.provider();
                let key = agent.key();
                let enabled = subagents.enabled.contains(&harness);
                let form = subagents.get(agent);
                let catalog = &self.store.data.settings.custom_subagents.get(agent).catalog;
                let lookup = self.model_lookups.get(agent);

                let mut top = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .w_full()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(logos::draw(logos::for_harness(harness), 16.0, hsla(theme::harness_color(harness))))
                            .child(div().text_size(scaled(15.0)).font_weight(FontWeight::SEMIBOLD).child(provider.name)),
                    )
                    .child(faint(provider.about))
                    .child(switch_row(
                        gpui::SharedString::from(format!("harness-dialog-{key}-enabled")),
                        format!("Enable {}", provider.name),
                        None,
                        enabled,
                        move |ws, _, _| flip_harness_enabled(ws, harness),
                        cx,
                    ))
                    .child(switch_row(
                        gpui::SharedString::from(format!("agent-dialog-{key}-on")),
                        "Allow use as subagents",
                        Some("Let Claude Code delegate tasks to this provider".into()),
                        form.on,
                        move |ws, _, cx| flip_subagent(ws, agent, cx),
                        cx,
                    ));
                // Keep Antigravity's terms/suspension warning directly under the subagent switch.
                if let Some(warning) = provider.warning {
                    top = top.child(warn(warning));
                }
                let mut models = div().flex().flex_col().w_full().gap(px(8.0));
                if form.on {
                    top = top
                        .child(switch_row(
                            gpui::SharedString::from(format!("agent-dialog-{key}-edits")),
                            "Allow edits in subagent tasks",
                            None,
                            form.edits,
                            move |ws, _, _| {
                                if let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() {
                                    let form = subagents.get_mut(agent);
                                    form.edits = !form.edits;
                                }
                            },
                            cx,
                        ))
                        .child(faint(if form.edits { provider.edits_on } else { provider.edits_off }));
                    let query = search.read(cx).value().to_string();
                    let mut list = div().flex().flex_col().gap(px(6.0));
                    let mut shown = 0;
                    for (model, offered) in model_rows(catalog, &form.models).into_iter().filter(|(m, _)| model_matches(&query, m)) {
                        let Some(name) = type_name(agent, &model.id) else { continue };
                        let name = format!("Claudiu:{name}");
                        let tag = if offered { name } else { format!("{name} · no longer offered") };
                        let id = model.id.clone();
                        list = list.child(switch_row(
                            gpui::SharedString::from(format!("agent-dialog-model-{}", model.id)),
                            model.name.clone(),
                            Some(tag.into()),
                            form.models.contains(&model.id),
                            move |ws, _, _| {
                                if let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() {
                                    flip(&mut subagents.get_mut(agent).models, id.clone());
                                }
                            },
                            cx,
                        ));
                        shown += 1;
                    }
                    if let Some(status) = lookup_status(agent, catalog, lookup) {
                        list = list.child(status);
                    } else if shown == 0 {
                        list = list.child(faint(format!("No model matches “{}”.", query.trim())));
                    }
                    let count = if catalog.is_empty() { String::new() } else { on_count(catalog, &form.models) };
                    models = models
                        .child(div().flex().w_full().mt(px(4.0)).items_center().justify_between().child(section_label("SUBAGENT MODELS")).child(faint(count)))
                        .child(faint(format!("Each model switched on becomes a subagent available to Claude Code. {} runs only when asked.", provider.name)))
                        .child(Input::new(search))
                        .child(list);
                } else {
                    models = models.child(faint(format!("Turn on “Allow use as subagents” to choose models for {}.", provider.name)));
                }
                div().flex().flex_col().gap(px(12.0)).w_full().child(top).child(models)
            }
        };

        let card = div()
            .id("agent-dialog")
            .occlude()
            .w(scaled(680.0).min(viewport.width - px(48.0)))
            .h(scaled(620.0).min(viewport.height - px(48.0)))
            .p(px(22.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .rounded(px(12.0))
            .bg(hsla(theme::PANEL_BG))
            .border_1()
            .border_color(hsla(theme::PANEL_BORDER))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    ws.close_harness_dialog(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(div().id("agent-dialog-body").flex_1().min_h_0().flex().flex_col().gap(px(12.0)).overflow_y_scroll().child(body))
            .child(div().flex().justify_end().child(button("agent-dialog-done", "Done", true).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                ws.close_harness_dialog(window, cx);
                cx.notify();
            }))));
        Some(
            deferred(
                div()
                    .id("agent-dialog-backdrop")
                    .occlude()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x00000088))
                    .on_mouse_down(MouseButton::Left, cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                        ws.close_harness_dialog(window, cx);
                        cx.notify();
                    }))
                    .child(card),
            )
            .with_priority(30)
            .into_any_element(),
        )
    }

    /// The Projects page: every project in the sidebar's order, to rename, move, remove, or add to. Changes apply at
    /// once, as on the Worktrees page; a removal can be undone from its toast.
    pub(crate) fn projects_page(&self, renaming: Option<&(crate::store::Id, gpui::Entity<InputState>)>, cx: &mut Context<Self>) -> Div {
        let projects = &self.store.data.projects;
        let add = button("project-add", "Add project…", false).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ImportProject, window, cx)));
        let list = div().flex().flex_col().gap(px(8.0)).child(
            div()
                .flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap(px(10.0))
                .child(faint("Removing a project keeps its sessions; they show under no project.").flex_1().min_w_0())
                .child(add),
        );
        if projects.is_empty() {
            return list.child(faint("No projects yet."));
        }
        let pressed = |b: gpui::Stateful<Div>, act: Act, cx: &mut Context<Self>| b.on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(act.clone(), window, cx)));
        let last = projects.len() - 1;
        let mut list = list;
        for (i, p) in projects.iter().enumerate() {
            let id = p.id.clone();
            let editing = renaming.filter(|(r, _)| *r == p.id);
            let name = match editing {
                Some((_, input)) => Input::new(input).into_any_element(),
                None => div().text_size(crate::panels::scale::px(13.0)).child(p.name.clone()).into_any_element(),
            };
            let mut buttons = div().flex().flex_none().gap(px(6.0));
            buttons = if editing.is_some() {
                buttons
                    .child(pressed(button(format!("project-cancel-{i}"), "Cancel", false), Act::CancelProjectRename, cx))
                    .child(pressed(button(format!("project-save-{i}"), "Save", true), Act::SaveProjectName, cx))
            } else {
                buttons.child(pressed(button(format!("project-rename-{i}"), "Rename", false), Act::RenameProject(id.clone()), cx))
            };
            buttons = buttons
                .child(if i == 0 { button_disabled(format!("project-up-{i}"), "Move up") } else { pressed(button(format!("project-up-{i}"), "Move up", false), Act::MoveProject(id.clone(), true), cx) })
                .child(if i == last { button_disabled(format!("project-down-{i}"), "Move down") } else { pressed(button(format!("project-down-{i}"), "Move down", false), Act::MoveProject(id.clone(), false), cx) })
                .child(pressed(button(format!("project-remove-{i}"), "Remove", false), Act::RemoveProject(id), cx));
            list = list.child(
                div()
                    .flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(px(10.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(7.0))
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
                            .child(name)
                            .child(faint(p.path.to_string_lossy().into_owned()).overflow_hidden().text_ellipsis().whitespace_nowrap()),
                    )
                    .child(buttons),
            );
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
