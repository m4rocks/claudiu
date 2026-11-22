//! The Settings pages that list things to act on: Projects, and Harnesses (Claude Code, Codex, OpenCode, Antigravity)
//! with shared model and subagent controls and provider-specific configuration.

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, deferred, div, prelude::FluentBuilder, px, rgba,
};
use gpui_kit::component::input::{Input, InputState};

use crate::app::{Act, HarnessDialog, Lookup, Modal, Workspace};
use crate::claude::subagents::{Agent, AgentModel};
use crate::harness::Harness;
use crate::panels::logos;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, button_disabled, section_label};

fn faint(text: impl Into<SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::TEXT_FAINT)).child(text.into())
}

fn warn(text: impl Into<SharedString>) -> Div {
    div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(theme::WARN)).child(text.into())
}

/// Compact pill toggle switch shared across all Settings pages and harness popups.
pub fn toggle_switch(on: bool) -> Div {
    div()
        .flex_none()
        .w(px(32.0))
        .h(px(18.0))
        .p(px(2.0))
        .rounded(px(999.0))
        .bg(hsla(if on { theme::ACCENT } else { 0x2c2c2c }))
        .flex()
        .items_center()
        .when(on, |d| d.justify_end())
        .child(div().w(px(14.0)).h(px(14.0)).rounded(px(999.0)).bg(hsla(if on { theme::PANEL_BG } else { 0x9e9e9e })))
}

/// A labelled Settings section: uppercase header (with an optional trailing control) over a single rounded card whose
/// child rows are separated by crisp hairlines.
pub fn settings_group(title: impl Into<SharedString>, trailing: Option<AnyElement>, rows: Vec<AnyElement>) -> Div {
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .child(section_label(title))
        .children(trailing);
    let mut card = div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(8.0))
        .bg(hsla(theme::SURFACE_ELEVATED))
        .border_1()
        .border_color(hsla(theme::PANEL_BORDER))
        .overflow_hidden();
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            card = card.child(div().h(px(1.0)).w_full().bg(hsla(theme::BORDER)));
        }
        card = card.child(row);
    }
    div().flex().flex_col().gap(px(7.0)).w_full().child(header).child(card)
}

/// An On/Off row that runs `flip` on the workspace when clicked, with an optional faint `tag` after the label.
pub fn switch_row(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    tag: Option<SharedString>,
    on: bool,
    flip: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    cx: &mut Context<Workspace>,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .min_h(px(36.0))
        .px(px(12.0))
        .py(px(6.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
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
        .child(toggle_switch(on))
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

/// Flips whether `harness` is enabled as a provider in m4code. Ensures at least one harness stays enabled.
fn flip_harness_enabled(ws: &mut Workspace, harness: Harness, cx: &mut Context<Workspace>) {
    let enabled = {
        let Some(Modal::Settings { subagents, default_model, .. }) = ws.modal.as_mut() else { return };
        if subagents.enabled.contains(&harness) {
            if subagents.enabled.len() <= 1 {
                return;
            }
            subagents.enabled.retain(|&h| h != harness);
            if default_model.harness == harness
                && let Some(&next) = subagents.enabled.first()
            {
                let all = ws.store.data.settings.aggregated_models_for(&subagents.enabled);
                *default_model = crate::store::Settings::resolve_model(&all, next, None);
            }
            false
        } else {
            subagents.enabled.push(harness);
            true
        }
    };
    if enabled {
        ws.load_models(cx);
    }
}

/// Flips an agent's "Allow use as subagents" switch inside its harness popup.
fn flip_subagent(ws: &mut Workspace, harness: Harness, cx: &mut Context<Workspace>) {
    let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() else { return };
    let mut access = subagents.access(harness);
    access.on = !access.on;
    subagents.set_access(harness, access);
    ws.load_models(cx);
}

fn harness_status(harness: Harness, enabled: bool, subagents: &crate::app::HarnessesForm, lookup: &Lookup, model_count: usize) -> (String, u32) {
    if !enabled {
        return ("Disabled".into(), theme::TEXT_FAINT);
    }
    if lookup.loading && model_count == 0 {
        return (format!("Enabled · loading {} models…", harness.name()), theme::TEXT_DIM);
    }
    if let Some(why) = &lookup.error
        && model_count == 0
    {
        return (format!("Enabled · couldn't load models ({why})"), theme::WARN);
    }
    let access = subagents.access(harness);
    let count = if subagents.picked(harness).is_empty() { model_count } else { subagents.picked(harness).len() };
    let models = match count {
        0 => String::new(),
        1 => " · 1 model".to_string(),
        n => format!(" · {n} models"),
    };
    let delegation = if access.on {
        if access.edits { "subagents on · edits allowed" } else { "subagents on · read-only" }
    } else {
        "subagents off"
    };
    (format!("Enabled{models} · {delegation}"), theme::TEXT_DIM)
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
    format!("{} of {} on", catalog.iter().filter(|m| is_ticked(on, &m.id)).count(), catalog.len())
}

/// Whether `id` is ticked. A harness nobody has set up yet has every model ticked, which `ticked` saves as no list.
fn is_ticked(ticked: &[String], id: &str) -> bool {
    ticked.is_empty() || ticked.iter().any(|t| t == id)
}

/// Tick or untick `id` among `catalog`'s models. The last ticked model stays ticked, so the model picker always has
/// one and an empty list only ever means all of them.
fn tick(ticked: &mut Vec<String>, catalog: &[AgentModel], id: &str) {
    if ticked.is_empty() {
        *ticked = catalog.iter().map(|m| m.id.clone()).collect();
    }
    match ticked.iter().position(|t| t == id) {
        Some(_) if ticked.len() == 1 => {}
        Some(at) => {
            ticked.remove(at);
        }
        None => ticked.push(id.to_string()),
    }
}

/// Whether `model` fits what was typed in the picker's search: every word, in its name or its id, ignoring case.
fn model_matches(query: &str, model: &AgentModel) -> bool {
    let haystack = format!("{} {}", model.name, model.id).to_lowercase();
    query.to_lowercase().split_whitespace().all(|word| haystack.contains(word))
}

impl Workspace {
    /// The Harnesses page: enable Claude Code, Codex, OpenCode and Antigravity, and open each provider's configuration
    /// popup with model, subagent and provider-specific controls.
    pub(crate) fn harnesses_page(&self, cx: &mut Context<Self>) -> Div {
        let Some(Modal::Settings { subagents, .. }) = &self.modal else { return div() };
        let mut rows: Vec<AnyElement> = Vec::new();
        for harness in Harness::ALL {
            let enabled = subagents.enabled.contains(&harness);
            let agent = harness.agent();
            let lookup = self.model_lookups.get(harness);
            let model_count = self.store.data.settings.harness_models(harness).len();
            let (status, tone) = harness_status(harness, enabled, subagents, lookup, model_count);

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
                        .child(logos::draw(logos::for_harness(harness), 15.0, hsla(if enabled { theme::ACCENT } else { theme::TEXT_FAINT })))
                        .child(div().text_size(crate::panels::scale::px(13.0)).font_weight(FontWeight::MEDIUM).text_color(hsla(if enabled { theme::TEXT } else { theme::TEXT_DIM })).child(harness.name())),
                )
                .child(div().text_size(crate::panels::scale::px(11.5)).text_color(hsla(tone)).child(status));
            let access = subagents.access(harness);
            let picked = subagents.picked(harness);
            let catalog = self.store.data.settings.harness_models(harness);
            if enabled && access.on && !picked.is_empty() {
                let names: Vec<&str> = picked.iter().map(|id| catalog.iter().find(|m| &m.id == id).map_or(id.as_str(), |m| m.name.as_str())).collect();
                about = about.child(faint(names.join(" · ")).overflow_hidden().text_ellipsis().whitespace_nowrap());
            }
            if let Some(warning) = agent.and_then(|agent| agent.provider().warning).filter(|_| access.on) {
                about = about.child(warn(warning));
            }
            let key = harness.name().to_lowercase().replace(' ', "-");
            let open = div()
                .id(SharedString::from(format!("settings-harness-{key}-open")))
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(12.0))
                .cursor_pointer()
                .child(about)
                .child(
                    div()
                        .flex_none()
                        .px(px(8.0))
                        .h(px(24.0))
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(hsla(theme::PANEL_BORDER))
                        .bg(hsla(theme::PANEL_BG))
                        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                        .flex()
                        .items_center()
                        .text_size(crate::panels::scale::px(11.5))
                        .text_color(hsla(theme::TEXT_DIM))
                        .child("Configure"),
                )
                .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                    ws.open_harness_dialog(harness, window, cx);
                    cx.notify();
                }));
            let toggle = div()
                .id(SharedString::from(format!("settings-harness-{key}")))
                .pl(px(12.0))
                .flex()
                .items_center()
                .cursor_pointer()
                .child(toggle_switch(enabled))
                .on_click(cx.listener(move |ws, _: &ClickEvent, _w, cx| {
                    flip_harness_enabled(ws, harness, cx);
                    cx.notify();
                }));
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .px(px(14.0))
                    .py(px(12.0))
                    .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)))
                    .child(open)
                    .child(toggle)
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(settings_group("CONNECTED HARNESSES", None, rows))
            .child(faint("Open any harness to configure its tools, accounts, or subagent delegation."))
    }

    fn subagent_controls(&self, harness: Harness, cx: &mut Context<Self>) -> Div {
        let Some(Modal::Settings { subagents, .. }) = &self.modal else { return div() };
        let access = subagents.access(harness);
        let key = harness.key();
        let mut controls = div().flex().flex_col().gap(px(8.0)).child(switch_row(
            SharedString::from(format!("agent-dialog-{key}-on")),
            "Allow use as subagents",
            Some("Let other m4code sessions delegate tasks to this harness".into()),
            access.on,
            move |ws, _, cx| flip_subagent(ws, harness, cx),
            cx,
        ));
        if let Some(warning) = harness.agent().and_then(|agent| agent.provider().warning) {
            controls = controls.child(warn(warning));
        }
        if access.on {
            controls = controls.child(switch_row(
                SharedString::from(format!("agent-dialog-{key}-edits")),
                "Allow edits in subagent tasks",
                None,
                access.edits,
                move |ws, _, _| {
                    if let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() {
                        let mut access = subagents.access(harness);
                        access.edits = !access.edits;
                        subagents.set_access(harness, access);
                    }
                },
                cx,
            )).child(faint(if access.edits {
                "m4code approves file edits and command requests. The harness's sandbox still applies. This can include network access or writes outside the project when permitted."
            } else {
                "Subagents cannot edit files. Commands follow the harness's read-only restrictions."
            }));
        }
        controls
    }

    fn model_list(&self, harness: Harness, search: &gpui::Entity<InputState>, cx: &mut Context<Self>) -> Div {
        let Some(Modal::Settings { subagents, .. }) = &self.modal else { return div() };
        let picked = subagents.picked(harness);
        let subagents_on = subagents.access(harness).on;
        let catalog = self.store.data.settings.harness_models(harness);
        let query = search.read(cx).value().to_string();
        let mut list = div().flex().flex_col().gap(px(6.0));
        let mut shown = 0;
        for (model, offered) in model_rows(catalog, picked).into_iter().filter(|(m, _)| model_matches(&query, m)) {
            let tag = if !offered { Some("no longer offered") } else if subagents_on { Some("Available as a subagent") } else { None };
            let id = model.id.clone();
            list = list.child(switch_row(
                gpui::SharedString::from(format!("harness-dialog-model-{}", model.id)),
                model.name.clone(),
                tag.map(Into::into),
                is_ticked(picked, &model.id),
                move |ws, _, _| {
                    let catalog = ws.store.data.settings.harness_models(harness).to_vec();
                    if let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() {
                        tick(subagents.picked_mut(harness), &catalog, &id);
                    }
                },
                cx,
            ));
            shown += 1;
        }
        let lookup = self.model_lookups.get(harness);
        if lookup.loading && catalog.is_empty() {
            list = list.child(faint(format!("Loading {}'s models…", harness.name())));
        } else if let Some(why) = lookup.error.as_ref().filter(|_| catalog.is_empty()) {
            list = list.child(warn(format!("Couldn't load {}'s models: {why}", harness.name())));
        } else if catalog.is_empty() {
            list = list.child(faint(format!("{} lists its models once it is enabled.", harness.name())));
        } else if shown == 0 {
            list = list.child(faint(format!("No model matches “{}”.", query.trim())));
        }
        let count = if catalog.is_empty() { String::new() } else { on_count(catalog, picked) };
        let about = if subagents_on {
            "Ticked models appear in the model picker and are available for delegated tasks from other m4code sessions."
        } else {
            "Ticked models appear in the model picker. Enable subagents to allow delegated tasks from other m4code sessions."
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(8.0))
            .child(div().flex().w_full().mt(px(4.0)).items_center().justify_between().child(section_label("MODELS")).child(faint(count)))
            .child(faint(about))
            .child(Input::new(search))
            .child(list)
    }

    /// Harness configuration, including shared model and subagent controls.
    pub(crate) fn render_harness_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Modal::Settings { claude, skills_field, costly, subagents, chrome, .. }) = &self.modal else { return None };
        let picking = subagents.picking.as_ref()?;
        let viewport = window.viewport_size();
        let scaled = crate::panels::scale::px;

        let body: Div = match picking {
            HarnessDialog::Claude(search) => {
                let enabled = subagents.enabled.contains(&Harness::Claude);
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
                    .child(faint("Uses your Claude Code configuration and sign-in."))
                    .child(switch_row(
                        "harness-dialog-claude-on",
                        "Enable Claude Code",
                        None,
                        enabled,
                        |ws, _, cx| flip_harness_enabled(ws, Harness::Claude, cx),
                        cx,
                    ))
                    .child(self.subagent_controls(Harness::Claude, cx))
                    .child(div().flex().flex_col().gap(px(4.0)).child(section_label("CLAUDE CODE PATH")).child(Input::new(claude)))
                    .child(switch_row(
                        "harness-dialog-claude-chrome",
                        "Claude in Chrome",
                        Some("New sessions start with browser integration (--chrome)".into()),
                        *chrome,
                        |ws, _, _| {
                            if let Some(Modal::Settings { chrome, .. }) = ws.modal.as_mut() {
                                *chrome = !*chrome;
                            }
                        },
                        cx,
                    ));

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
                col = col.child(self.model_list(Harness::Claude, search, cx));

                // m4code trims the extras itself (trim::FLUFF), per session; what's left here are the user's own settings.
                for found in costly {
                    col = col.child(warn(format!("{} is set in {}. {}", found.name, found.source, found.effect)));
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
                let top = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .w_full()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(logos::draw(logos::for_harness(harness), 16.0, hsla(theme::ACCENT)))
                            .child(div().text_size(scaled(15.0)).font_weight(FontWeight::SEMIBOLD).child(provider.name)),
                    )
                    .child(faint(format!("Uses your {} configuration and sign-in.", harness.name())))
                    .child(switch_row(
                        gpui::SharedString::from(format!("harness-dialog-{key}-enabled")),
                        format!("Enable {}", provider.name),
                        None,
                        enabled,
                        move |ws, _, cx| flip_harness_enabled(ws, harness, cx),
                        cx,
                    ))
                    .child(self.subagent_controls(harness, cx));
                let models = self.model_list(harness, search, cx);
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

    /// The Projects section on the Projects page: every project in the sidebar's order, to rename, move, remove, or
    /// add to. Changes apply at once; a removal can be undone from its toast.
    pub(crate) fn projects_section(&self, renaming: Option<&(crate::store::Id, gpui::Entity<InputState>)>, cx: &mut Context<Self>) -> Div {
        let projects = &self.store.data.projects;
        let add = button("project-add", "Add project…", false)
            .on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::ImportProject, window, cx)))
            .into_any_element();
        if projects.is_empty() {
            let empty = div()
                .px(px(14.0))
                .py(px(14.0))
                .text_size(crate::panels::scale::px(12.0))
                .text_color(hsla(theme::TEXT_FAINT))
                .child("No projects yet. Add a folder to group sessions and switch branches.")
                .into_any_element();
            return settings_group("PROJECTS", Some(add), vec![empty]);
        }
        let pressed = |b: gpui::Stateful<Div>, act: Act, cx: &mut Context<Self>| b.on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| ws.act(act.clone(), window, cx)));
        let last = projects.len() - 1;
        let mut rows: Vec<AnyElement> = Vec::with_capacity(projects.len());
        for (i, p) in projects.iter().enumerate() {
            let id = p.id.clone();
            let editing = renaming.filter(|(r, _)| *r == p.id);
            let name = match editing {
                Some((_, input)) => Input::new(input).into_any_element(),
                None => div().text_size(crate::panels::scale::px(13.0)).font_weight(FontWeight::MEDIUM).child(p.name.clone()).into_any_element(),
            };
            let mut buttons = div().flex().flex_none().gap(px(5.0));
            buttons = if editing.is_some() {
                buttons
                    .child(pressed(button(format!("project-cancel-{i}"), "Cancel", false), Act::CancelProjectRename, cx))
                    .child(pressed(button(format!("project-save-{i}"), "Save", true), Act::SaveProjectName, cx))
            } else {
                buttons.child(pressed(button(format!("project-rename-{i}"), "Rename", false), Act::RenameProject(id.clone()), cx))
            };
            buttons = buttons
                .child(if i == 0 { button_disabled(format!("project-up-{i}"), "Up") } else { pressed(button(format!("project-up-{i}"), "Up", false), Act::MoveProject(id.clone(), true), cx) })
                .child(if i == last { button_disabled(format!("project-down-{i}"), "Down") } else { pressed(button(format!("project-down-{i}"), "Down", false), Act::MoveProject(id.clone(), false), cx) })
                .child(pressed(button(format!("project-remove-{i}"), "Remove", false), Act::RemoveProject(id), cx));
            rows.push(
                div()
                    .flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .px(px(14.0))
                    .py(px(10.0))
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
                    .child(buttons)
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(settings_group("PROJECTS", Some(add), rows))
            .child(faint("Removing a project keeps its sessions; they show under no project."))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn harness_subtitles_use_the_same_subagent_status_for_every_provider() {
        let form = crate::app::HarnessesForm {
            enabled: Harness::ALL.to_vec(),
            agents: crate::claude::subagents::AgentMap::from_fn(|_| crate::app::AgentForm { on: true, edits: true, models: vec![] }),
            claude_models: vec![], claude_subagents: Default::default(), picking: None,
        };
        for harness in [Harness::Claude, Harness::Codex, Harness::OpenCode] {
            assert_eq!(harness_status(harness, true, &form, &Lookup::default(), 2).0, "Enabled · 2 models · subagents on · edits allowed");
        }
    }
    #[test]
    fn every_model_starts_ticked_and_the_last_one_stays() {
        use super::{is_ticked, tick};
        let catalog: Vec<_> = ["a", "b", "c"].iter().map(|id| crate::claude::subagents::AgentModel { id: id.to_string(), name: id.to_string() }).collect();
        let mut ticked = vec![];
        assert!(catalog.iter().all(|m| is_ticked(&ticked, &m.id)), "a harness nobody set up has every model ticked");
        tick(&mut ticked, &catalog, "b");
        assert_eq!(ticked, ["a", "c"]);
        tick(&mut ticked, &catalog, "a");
        tick(&mut ticked, &catalog, "c");
        assert_eq!(ticked, ["c"], "the last ticked model can't be unticked");
        tick(&mut ticked, &catalog, "b");
        assert_eq!(ticked, ["c", "b"]);
    }

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
