//! The Settings pages that list things to act on: Projects, and Custom subagents with the dialog each agent opens.

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, StatefulInteractiveElement,
    Styled, Window, deferred, div, px, rgba,
};
use gpui_kit::component::input::{Input, InputState};

use crate::app::{Act, AgentForm, Lookup, Modal, Workspace};
use crate::claude::subagents::{Agent, AgentModel, type_name};
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
        .min_h(px(32.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .rounded(px(6.0))
        .bg(hsla(0x111111))
        .border_1()
        .border_color(hsla(0x242424))
        .text_size(crate::panels::scale::px(13.0))
        .cursor_pointer()
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
        .child(div().flex_none().text_color(hsla(if on { theme::ACCENT } else { theme::TEXT_FAINT })).child(if on { "On" } else { "Off" }))
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

/// Flips an agent's switch. Switched on with no model chosen yet, its dialog opens on the models, since an agent with
/// none gives Claude no subagent.
fn flip_agent(ws: &mut Workspace, agent: Agent, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(Modal::Settings { subagents, .. }) = ws.modal.as_mut() else { return };
    let form = subagents.get_mut(agent);
    form.on = !form.on;
    let choose = form.on && form.models.is_empty();
    ws.load_models(cx);
    if choose && !ws.agent_dialog_open() {
        ws.open_agent_dialog(agent, window, cx);
    }
}

/// What an agent's card says under its name, and in which colour: off, missing a model, or what Claude gets.
fn agent_status(agent: Agent, form: &AgentForm, lookup: &Lookup) -> (String, u32) {
    if !form.on {
        return ("Off".into(), theme::TEXT_FAINT);
    }
    if lookup.error.is_some() {
        return (format!("Couldn't load {}'s models", agent.name()), theme::WARN);
    }
    let access = if form.edits { "edits allowed" } else { "read-only" };
    match form.models.len() {
        0 => ("On, but no model is chosen, so Claude gets no subagent".into(), theme::WARN),
        1 => (format!("1 subagent · {access}"), theme::TEXT_DIM),
        n => (format!("{n} subagents · {access}"), theme::TEXT_DIM),
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
    /// The Custom subagents page: what it is for, then one card per agent.
    pub(crate) fn subagents_page(&self, cx: &mut Context<Self>) -> Div {
        let Some(Modal::Settings { subagents, .. }) = &self.modal else { return div() };
        let saved = &self.store.data.settings.custom_subagents;
        let mut page = div().flex().flex_col().gap(px(8.0)).child(faint(
            "Let Claude hand a task to another coding agent. Each model you switch on under a provider becomes one subagent, and Claude uses it only when you ask for it. Applies to new and resumed sessions; running sessions keep theirs.",
        ));
        for agent in Agent::ALL {
            page = page.child(self.agent_card(agent, subagents.get(agent), &saved.get(agent).catalog, self.model_lookups.get(agent), cx));
        }
        page
    }

    /// An agent's card on the Custom subagents page: its name, its status and the models switched on, which open its
    /// dialog, and its switch beside them.
    fn agent_card(&self, agent: Agent, form: &AgentForm, catalog: &[AgentModel], lookup: &Lookup, cx: &mut Context<Self>) -> impl IntoElement {
        let key = agent.key();
        let (status, tone) = agent_status(agent, form, lookup);
        let mut about = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(3.0))
            .child(div().text_size(crate::panels::scale::px(13.0)).font_weight(FontWeight::SEMIBOLD).child(agent.name()))
            .child(div().text_size(crate::panels::scale::px(11.0)).text_color(hsla(tone)).child(status));
        if form.on && !form.models.is_empty() {
            let names: Vec<&str> = form.models.iter().map(|id| catalog.iter().find(|m| &m.id == id).map_or(id.as_str(), |m| m.name.as_str())).collect();
            about = about.child(faint(names.join(" · ")).overflow_hidden().text_ellipsis().whitespace_nowrap());
        }
        // The card's switch turns an agent on without opening its dialog once models are saved, so the card warns too.
        if let Some(warning) = agent.provider().warning {
            about = about.child(warn(warning));
        }
        let open = div()
            .id(gpui::SharedString::from(format!("settings-{key}-open")))
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(10.0))
            .cursor_pointer()
            .child(about)
            .child(div().text_size(crate::panels::scale::px(12.0)).text_color(hsla(theme::TEXT_DIM)).child("Set up ›"))
            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                ws.open_agent_dialog(agent, window, cx);
                cx.notify();
            }));
        let toggle = div()
            .id(gpui::SharedString::from(format!("settings-{key}")))
            .pl(px(14.0))
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(crate::panels::scale::px(13.0))
            .text_color(hsla(if form.on { theme::ACCENT } else { theme::TEXT_FAINT }))
            .child(if form.on { "On" } else { "Off" })
            .on_click(cx.listener(move |ws, _: &ClickEvent, window, cx| {
                flip_agent(ws, agent, window, cx);
                cx.notify();
            }));
        div()
            .flex()
            .items_center()
            .w_full()
            .px(px(12.0))
            .py(px(10.0))
            .rounded(px(8.0))
            .bg(hsla(0x111113))
            .border_1()
            .border_color(hsla(if form.on { 0x3a302b } else { 0x242428 }))
            .child(open)
            .child(toggle)
    }

    /// An agent's dialog over the Custom subagents page: what it runs on, its switch, Allow edits, and a search over
    /// the models it offers with a switch for each. It edits the page's form, so Settings' Save and Cancel decide what
    /// is kept.
    pub(crate) fn render_agent_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Modal::Settings { subagents, .. }) = &self.modal else { return None };
        let (agent, search) = subagents.picking.as_ref()?;
        let agent = *agent;
        let provider = agent.provider();
        let key = agent.key();
        let form = subagents.get(agent);
        let catalog = &self.store.data.settings.custom_subagents.get(agent).catalog;
        let lookup = self.model_lookups.get(agent);
        let mut top = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w_full()
            .child(div().text_size(crate::panels::scale::px(15.0)).font_weight(FontWeight::SEMIBOLD).child(format!("{} subagents", provider.name)))
            .child(faint(provider.about));
        if let Some(warning) = provider.warning {
            top = top.child(warn(warning));
        }
        top = top.child(switch_row(
            gpui::SharedString::from(format!("agent-dialog-{key}-on")),
            format!("Give Claude {} subagents", provider.name),
            None,
            form.on,
            move |ws, window, cx| flip_agent(ws, agent, window, cx),
            cx,
        ));
        let mut models = div().flex().flex_col().w_full().gap(px(8.0));
        if form.on {
            top = top
                .child(switch_row(
                    gpui::SharedString::from(format!("agent-dialog-{key}-edits")),
                    "Allow edits",
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
                // The mod registers the type, so Claude Code puts its plugin's name before it.
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
                .child(div().flex().w_full().mt(px(4.0)).items_center().justify_between().child(section_label("MODELS")).child(faint(count)))
                .child(faint(format!("Each model switched on is one subagent Claude can start, named as shown under it. Claude uses {} only when you ask for it.", provider.name)))
                .child(Input::new(search))
                .child(list);
        } else {
            models = models.child(faint(format!("Switch {} on to choose its models.", provider.name)));
        }
        let viewport = window.viewport_size();
        let card = div()
            .id("agent-dialog")
            .occlude()
            // A small window or a large scale shrinks the dialog to fit, and its body scrolls as one above Done.
            .w(crate::panels::scale::px(560.0).min(viewport.width - px(32.0)))
            .h(crate::panels::scale::px(560.0).min(viewport.height - px(32.0)))
            .p(px(20.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .rounded(px(12.0))
            .bg(hsla(0x151517))
            .border_1()
            .border_color(hsla(0x2c2c30))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|ws, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    ws.close_agent_dialog(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(div().id("agent-dialog-body").flex_1().min_h_0().flex().flex_col().gap(px(12.0)).overflow_y_scroll().child(top).child(models))
            .child(div().flex().justify_end().child(button("agent-dialog-done", "Done", true).on_click(cx.listener(|ws, _: &ClickEvent, window, cx| {
                ws.close_agent_dialog(window, cx);
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
                    .bg(rgba(0x00000066))
                    .on_mouse_down(MouseButton::Left, cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                        ws.close_agent_dialog(window, cx);
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
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .bg(hsla(0x111113))
                    .border_1()
                    .border_color(hsla(0x242428))
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
