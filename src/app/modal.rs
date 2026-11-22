//! Overlays: toasts, the context menu, input modals, the editor choice and the project import.

use super::*;
use crate::panels::widgets::Dropdown;

impl Workspace {
    pub fn toast(&mut self, text: impl Into<String>, error: bool, cx: &mut Context<Self>) {
        self.show_toast(text.into(), error, None, cx);
    }

    pub(super) fn show_toast(&mut self, text: String, error: bool, action: Option<(String, Act)>, cx: &mut Context<Self>) {
        self.toast_serial += 1;
        let serial = self.toast_serial;
        let secs = if action.is_some() { 20 } else { 5 };
        self.toast = Some(Toast {
            text,
            error,
            serial,
            action,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(secs)).await;
            let _ = this.update(cx, |ws, cx| {
                if ws.toast.as_ref().is_some_and(|t| t.serial == serial) {
                    ws.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
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
                cx.notify();
            }
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
                    self.toast(why, true, cx);
                    return;
                }
                Act::Dismiss
            }
            Some(Modal::GoToLine { id, input }) => {
                let text = input.read(cx).value().to_string();
                if let Err(why) = self.go_to_line(&id, &text, window, cx) {
                    self.modal = Some(Modal::GoToLine { id, input });
                    self.toast(why, true, cx);
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
            effort,
            permission,
            updates,
            chrome,
            scale,
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
        let permission_changed = s.new_session.mode_id.as_deref() != Some(permission.as_str());
        let harness_changed = s.harness != chosen.harness || s.enabled_harnesses.as_ref() != Some(&enabled);
        let legacy_perm = claude::defaults::PermissionMode::ALL
            .into_iter()
            .find(|m| m.acp_id() == permission || m.name() == permission);
        s.enabled_harnesses = Some(enabled);
        s.harness = chosen.harness;
        s.new_session = claude::defaults::NewSessionDefaults {
            model: next_model,
            effort: Some(effort),
            permission_mode: legacy_perm,
            mode_id: Some(permission.clone()),
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
        let custom_subagents = claude::subagents::CustomSubagents(claude::subagents::AgentMap::from_fn(|agent| {
            let form = subagents.get(agent);
            let catalog = s.custom_subagents.get(agent).catalog.clone();
            let models = if form.on && form.models.is_empty() { catalog.iter().map(|m| m.id.clone()).collect() } else { form.models.clone() };
            claude::subagents::AgentChoice { on: form.on, models, edits: form.edits, catalog }
        }));
        let subagents_changed = s.custom_subagents != custom_subagents || s.claude_picked != subagents.claude_models;
        s.limits = limits;
        s.m4code_tools_off = m4code_off;
        s.custom_subagents = custom_subagents;
        s.claude_picked = subagents.claude_models.clone();

        crate::panels::scale::set(scale);
        self.shell = platform::default_shell(s.shell.as_deref());
        self.claude_exe = claude::discover(s.claude_path.as_deref());
        self.store.mark_dirty();
        if default_model_changed {
            self.home.model = None;
        }

        // Apply updated settings to every open session immediately.
        self.push_models_to_threads(cx);
        let effort_id = effort.name().to_string();
        for live in &self.live {
            if let Some(thread) = live.view.thread() {
                thread.update(cx, |t, cx| {
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
                    if permission_changed && t.thread.modes.iter().any(|(id, _)| id == &permission) {
                        t.set_mode(permission.clone(), cx);
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
        if let Some(msg) = warnings.into_iter().next() {
            self.toast(msg, true, cx);
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

    pub fn open_menu(&mut self, pos: Point<Pixels>, items: Vec<MenuItem>, cx: &mut Context<Self>) {
        self.menu = Some(Menu { pos, min_w: gpui::px(0.0), items });
        cx.notify();
    }

    /// A menu hanging under its trigger button.
    pub fn open_dropdown(&mut self, at: Dropdown, items: Vec<MenuItem>, cx: &mut Context<Self>) {
        self.menu = Some(Menu { pos: at.pos, min_w: at.min_w, items });
        cx.notify();
    }
}
