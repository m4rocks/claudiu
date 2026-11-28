//! Overlays: toasts, the context menu, input modals, the editor choice and the project import.

use super::*;
use crate::panels::widgets::Dropdown;

/// Boundary stops wrap inside a dialog without visiting workspace controls behind it.
pub struct DialogTabCycle {
    pub start: FocusHandle,
    pub end: FocusHandle,
}

impl DialogTabCycle {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let start = cx.focus_handle().tab_stop(true);
        let end = cx.focus_handle().tab_stop(true);
        let last = end.clone();
        cx.on_focus(&start, window, move |_, window, cx| {
            if window.modifiers().shift {
                window.focus(&last, cx);
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
        }).detach();
        let first = start.clone();
        cx.on_focus(&end, window, move |_, window, cx| {
            if window.modifiers().shift {
                window.focus_prev(cx);
            } else {
                window.focus(&first, cx);
                window.focus_next(cx);
            }
        }).detach();
        Self { start, end }
    }
}

impl Workspace {
    pub(crate) fn visible_toasts(&self) -> impl Iterator<Item = &Toast> { self.toast.iter().chain(self.toast_queue.iter()) }

    pub(super) fn executable_input(&mut self, value: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let input = self.modal_input(value, placeholder, window, cx);
        self.validate_executable_input(&input, cx);
        cx.subscribe(&input, |ws, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                ws.validate_executable_input(&input, cx);
                cx.notify();
            }
        }).detach();
        input
    }

    fn validate_executable_input(&mut self, input: &Entity<InputState>, cx: &Context<Self>) {
        match super::parse_executable_path(input.read(cx).value().as_ref()) {
            Ok(_) => { self.executable_errors.remove(&input.entity_id()); }
            Err(message) => { self.executable_errors.insert(input.entity_id(), message); }
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.show_toast(text.into(), None, cx);
    }

    pub(super) fn show_toast(&mut self, text: String, action: Option<(String, Act)>, cx: &mut Context<Self>) {
        self.enqueue_toast(text, action, crate::panels::ui::alert::Tone::Default, cx);
    }

    pub(crate) fn error_toast(&mut self, text: String, cx: &mut Context<Self>) {
        self.enqueue_toast(text, None, crate::panels::ui::alert::Tone::Destructive, cx);
    }

    fn enqueue_toast(&mut self, text: String, action: Option<(String, Act)>, tone: crate::panels::ui::alert::Tone, cx: &mut Context<Self>) {
        let previous = self.toast_queue.back().or(self.toast.as_ref());
        if action.is_none() && previous.is_some_and(|toast| toast.action.is_none() && toast.text == text && toast.tone == tone) { return; }
        self.toast_serial += 1;
        let serial = self.toast_serial;
        let secs = if action.is_some() { 20 } else { 5 };
        self.toast_queue.push_back(Toast { text, action, tone, serial });
        if self.toast.is_none() { self.next_toast(cx); }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(secs)).await;
            let _ = this.update(cx, |ws, cx| ws.dismiss_toast_serial(serial, cx));
        }).detach();
        cx.notify();
    }

    fn next_toast(&mut self, cx: &mut Context<Self>) {
        self.toast = self.toast_queue.pop_front();
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.next_toast(cx);
    }

    pub(crate) fn dismiss_toast_serial(&mut self, serial: u64, cx: &mut Context<Self>) {
        if self.toast.as_ref().is_some_and(|toast| toast.serial == serial) {
            self.next_toast(cx);
        } else {
            self.toast_queue.retain(|toast| toast.serial != serial);
            cx.notify();
        }
    }

    /// A single-line field for a dialog, focused, with Enter submitting the dialog.
    pub(super) fn modal_input(&mut self, value: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        self.modal_input_then(value, placeholder, Self::submit_modal, window, cx)
    }

    /// `modal_input`, where Enter runs `on_enter` instead of submitting the whole dialog.
    pub(super) fn modal_input_then(
        &mut self,
        value: &str,
        placeholder: &str,
        on_enter: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value.to_string()).placeholder(placeholder.to_string()));
        cx.subscribe_in(&input, window, move |ws: &mut Self, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                on_enter(ws, window, cx);
            }
            cx.notify();
        })
        .detach();
        input.update(cx, |s, cx| s.focus(window, cx));
        input
    }

    /// Enter / OK in a modal.
    pub fn submit_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.modal, Some(Modal::Settings { .. })) {
            self.close_settings(window, cx);
            return;
        }
        let act = match self.modal.take() {
            Some(Modal::Confirm { act, .. }) => act,
            Some(Modal::Rename { id, input }) => {
                let text = input.read(cx).value().trim().to_string();
                if let Some(rec) = self.store.session_mut(&id) {
                    rec.custom_title = (!text.is_empty()).then_some(text);
                }
                Act::Dismiss
            }
            Some(Modal::FileName { id, purpose, input }) => {
                let name = input.read(cx).value().trim().to_string();
                if let Err(why) = self.apply_file_name(&id, &purpose, &name, window, cx) {
                    self.modal = Some(Modal::FileName { id, purpose, input });
                    self.toast(why, cx);
                    return;
                }
                Act::Dismiss
            }
            Some(Modal::GoToLine { id, input }) => {
                let text = input.read(cx).value().to_string();
                if let Err(why) = self.go_to_line(&id, &text, window, cx) {
                    self.modal = Some(Modal::GoToLine { id, input });
                    self.toast(why, cx);
                    return;
                }
                Act::Dismiss
            }
            Some(Modal::NewBranch { cwd, input }) => {
                // Spaces aren't allowed in branch names; git reports anything else that is invalid.
                let name = input.read(cx).value().split_whitespace().collect::<Vec<_>>().join("-");
                if name.is_empty() { Act::Dismiss } else { Act::CreateBranch(cwd, name) }
            }
            Some(Modal::Settings { .. }) | None => return,
        };
        self.focus_active(window, cx);
        self.act(act, window, cx);
    }

    /// Close Settings, auto-saving every valid change and applying it to open sessions immediately.
    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Settings {
            shell,
            claude,
            default_model,
            text_generation_model,
            effort,
            permission,
            updates,
            chrome,
            scale,
            follow_up,
            auto_compact,
            auto_compact_threshold,
            usage_display,
            notify_model_on_limit,
            auto_handoff_on_limit,
            auto_handoff_model,
            m4code_off,
            skills_field,
            renaming,
            subagents,
            ..
        }) = self.modal.take()
        else {
            return;
        };
        let clean = |s: &str| s.trim().trim_matches('"').to_string();
        let (shell_t, claude_t) = (clean(shell.read(cx).value().as_ref()), clean(claude.read(cx).value().as_ref()));
        let mut warnings = Vec::new();

        // Keep a name typed on the Projects page.
        if let Some((id, name)) = renaming {
            let name = name.read(cx).value().to_string();
            self.store.rename_project(&id, &name);
        }

        let s = &mut self.store.data.settings;
        if shell_t.is_empty() || platform::resolve_program(&shell_t).is_some() {
            s.shell = (!shell_t.is_empty()).then_some(shell_t);
        } else {
            warnings.push(format!("Shell: “{shell_t}” is not a file or a program on PATH"));
        }
        if claude_t.is_empty() || platform::resolve_program(&claude_t).is_some() {
            s.claude_path = platform::resolve_program(&claude_t);
        } else {
            warnings.push(format!("Claude path: “{claude_t}” is not a file or a program on PATH"));
        }

        // Only the on/off state is edited here; a custom feed URL survives while checks stay on.
        if updates == (s.update_url.as_deref() == Some("")) {
            s.update_url = (!updates).then(String::new);
        }
        let scale_changed = s.scale != (scale != 100).then_some(scale);
        s.scale = (scale != 100).then_some(scale);
        s.chrome_default = chrome;
        s.follow_up = follow_up;
        let auto_compact_threshold = crate::harness::compaction::clamp_threshold(auto_compact_threshold);
        let auto_compact_changed = s.auto_compact != auto_compact;
        let auto_compact_threshold_changed = s.auto_compact_threshold != auto_compact_threshold;
        s.auto_compact = auto_compact;
        s.auto_compact_threshold = auto_compact_threshold;
        s.usage_display = usage_display;
        s.notify_model_on_limit = notify_model_on_limit;
        s.auto_handoff_on_limit = auto_handoff_on_limit;
        s.auto_handoff_model = auto_handoff_model;
        s.text_generation_model = text_generation_model;

        let enabled = if subagents.enabled.is_empty() { vec![default_model.harness] } else { subagents.enabled.clone() };
        let chosen = if enabled.contains(&default_model.harness) {
            default_model
        } else {
            let all = s.aggregated_models_for(&enabled);
            crate::store::Settings::resolve_model(&all, enabled[0], None)
        };
        let next_model = (!chosen.id.is_empty()).then_some(chosen.id.clone());
        let default_model_changed = s.harness != chosen.harness || s.new_session.model != next_model;
        let effort_changed = s.new_session.effort != Some(effort);
        let permission_changed = s.new_session.permission_mode != permission;
        let harness_changed = s.harness != chosen.harness || s.enabled_harnesses.as_ref() != Some(&enabled);
        s.enabled_harnesses = Some(enabled);
        s.harness = chosen.harness;
        s.new_session = harness::defaults::NewSessionDefaults {
            model: next_model,
            effort: Some(effort),
            permission_mode: permission,
        };
        self.home.effort = effort;

        let m4code_off = mcp::normalized_features(&m4code_off);
        let mut limits = match claude::trim::Limits::parse(skills_field.read(cx).value().as_ref()) {
            Ok(l) => l,
            Err(why) => {
                warnings.push(why);
                s.limits.clone()
            }
        };
        if s.limits != limits {
            match claude::trim::write_global_limits(&limits, &s.applied_trims) {
                Ok(owned) => s.applied_trims = owned,
                Err(e) => {
                    warnings.push(format!("Claude Code's settings.json could not be changed ({e})"));
                    limits = s.limits.clone();
                }
            }
        }

        // The catalogs are what the agents last offered, kept as they are; the page edits the rest.
        let custom_subagents = harness::subagents::CustomSubagents(harness::subagents::AgentMap::from_fn(|agent| {
            let form = subagents.get(agent);
            let catalog = s.custom_subagents.get(agent).catalog.clone();
            let models = if form.on && form.models.is_empty() { catalog.iter().map(|m| m.id.clone()).collect() } else { form.models.clone() };
            harness::subagents::AgentChoice { on: form.on, models, edits: form.edits, catalog }
        }));
        let subagents_changed = s.custom_subagents != custom_subagents || s.claude_picked != subagents.claude_models || s.claude_subagents != subagents.claude_subagents;
        s.limits = limits;
        s.m4code_tools_off = m4code_off;
        s.custom_subagents = custom_subagents;
        s.claude_picked = subagents.claude_models.clone();
        s.claude_subagents = subagents.claude_subagents;

        crate::panels::scale::set(scale);
        self.shell = platform::default_shell(s.shell.as_deref());
        self.store.mark_dirty();
        self.discover_executables(cx);
        if default_model_changed {
            self.home.model = None;
        }

        // Apply updated settings to every open session immediately.
        self.push_models_to_threads(cx);
        let inline_visuals = !self.store.data.settings.m4code_tools_off.contains(&mcp::Feature::Visual);
        let effort_id = effort.name().to_string();
        for live in &self.live {
            let inherits_auto_compact = self.store.session(&live.id).and_then(|r| r.agent.as_ref()).is_none_or(|m| m.auto_compact_override.is_none());
            if let Some(thread) = live.view.thread() {
                thread.update(cx, |t, cx| {
                    t.follow_up = follow_up;
                    if t.inline_visuals != inline_visuals {
                        t.inline_visuals = inline_visuals;
                        t.remeasure(0);
                    }
                    if (auto_compact_changed && inherits_auto_compact) || auto_compact_threshold_changed {
                        let enabled = if inherits_auto_compact { auto_compact } else { t.compaction.enabled };
                        t.set_compaction_policy(enabled, auto_compact_threshold, cx);
                    }
                    if default_model_changed && t.harness == chosen.harness && !chosen.id.is_empty() {
                        t.pick_model(chosen.clone(), cx);
                    }
                    if effort_changed {
                        t.pending_effort = Some(effort_id.clone());
                        if let Some(opt) = t.thread.option("thought_level")
                            && opt.choices.iter().any(|(v, _)| v == &effort_id)
                        {
                            let id = opt.id.clone();
                            t.set_option(id, effort_id.clone(), cx);
                        }
                    }
                    if permission_changed
                        && let Some(mode) = permission
                    {
                        t.set_permission(mode, cx);
                    }
                    if scale_changed {
                        t.remeasure(0);
                    }
                    cx.notify();
                });
            }
        }
        if harness_changed || subagents_changed {
            self.load_enabled_models(cx);
            self.sync_skills(None, cx);
        }
        if !warnings.is_empty() {
            self.toast(warnings.join("\n"), cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The editor "Open in" uses: the last one picked if it is still installed, else the first one found.
    pub fn preferred_editor(&self) -> Option<Editor> {
        let installed = |e: &Editor| self.editors.iter().any(|(x, ok)| x == e && *ok);
        self.store.data.settings.editor.filter(installed).or_else(|| Editor::ALL.into_iter().find(installed))
    }

    pub(super) fn import_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next() {
                    let _ = this.update_in(cx, |ws, window, cx| {
                        ws.act(Act::AddProject(path), window, cx)
                    });
                }
        })
        .detach();
    }

    pub fn open_menu(&mut self, pos: Point<Pixels>, items: Vec<MenuItem>, window: &mut Window, cx: &mut Context<Self>) {
        self.show_menu(pos, gpui::px(0.0), items, window, cx);
    }

    /// A menu hanging under its trigger button.
    pub fn open_dropdown(&mut self, at: Dropdown, items: Vec<MenuItem>, window: &mut Window, cx: &mut Context<Self>) {
        self.show_menu(at.pos, at.min_w, items, window, cx);
    }

    /// The menu takes the keyboard for Up, Down, Enter and Esc, and remembers who had it.
    fn show_menu(&mut self, pos: Point<Pixels>, min_w: Pixels, items: Vec<MenuItem>, window: &mut Window, cx: &mut Context<Self>) {
        // A menu opened over another one keeps the first one's place to go back to.
        let return_focus = self.menu.take().and_then(|m| m.return_focus).or_else(|| window.focused(cx));
        self.menu = Some(Menu { pos, min_w, items, return_focus });
        self.menu_nav.reset();
        self.menu_nav.focus(window, cx);
        cx.notify();
    }

    /// Close the menu and hand the keyboard back to what had it before.
    pub fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = self.menu.take().and_then(|m| m.return_focus) {
            window.focus(&focus, cx);
        }
        cx.notify();
    }
}
