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
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value.to_string()).placeholder(placeholder.to_string()));
        cx.subscribe_in(&input, window, |ws: &mut Self, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                ws.submit_modal(window, cx);
            }
        })
        .detach();
        input.update(cx, |s, cx| s.focus(window, cx));
        input
    }

    /// Enter / OK in a modal.
    pub fn submit_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            Some(Modal::Advisor { id, input }) => {
                let name = input.read(cx).value().trim().to_lowercase();
                if name.is_empty() {
                    Act::Dismiss
                } else if !usage::valid_model_name(&name) {
                    self.modal = Some(Modal::Advisor { id, input });
                    self.toast("Use a model alias or id (letters, digits, . _ -), or “off”.", true, cx);
                    return;
                } else {
                    self.act(Act::SetAdvisor(id, name), window, cx);
                    Act::Dismiss
                }
            }
            Some(Modal::NewBranch { cwd, input }) => {
                // Spaces aren't allowed in branch names; git reports anything else that is invalid.
                let name = input.read(cx).value().split_whitespace().collect::<Vec<_>>().join("-");
                if name.is_empty() { Act::Dismiss } else { Act::CreateBranch(cwd, name) }
            }
            Some(Modal::Settings { shell, claude, model, effort, permission, updates, chrome, scale, trimmed, claudiu_off, list_focus }) => {
                let clean = |s: &str| s.trim().trim_matches('"').to_string();
                let (shell_t, claude_t) = (clean(shell.read(cx).value().as_ref()), clean(claude.read(cx).value().as_ref()));
                let model_name = claude::defaults::parse_model(model.read(cx).value().as_ref());
                // In category order, then top to bottom: the first bad field decides which page the dialog shows.
                let paths = [("Shell", &shell, &shell_t), ("Claude path", &claude, &claude_t)]
                    .into_iter()
                    .find(|(_, _, t)| !t.is_empty() && platform::resolve_program(t).is_none())
                    .map(|(what, field, t)| (SettingsPage::General, field.clone(), format!("{what}: “{t}” is not a file or a program on PATH")));
                let bad_model = model_name.is_err().then(|| (SettingsPage::Sessions, model.clone(), "Use a model alias or id (letters, digits, . _ -).".to_string()));
                if let Some((page, field, msg)) = paths.or(bad_model) {
                    self.settings_page = page;
                    field.update(cx, |s, cx| s.focus(window, cx));
                    self.modal = Some(Modal::Settings { shell, claude, model, effort, permission, updates, chrome, scale, trimmed, claudiu_off, list_focus });
                    self.toast(msg, true, cx);
                    return;
                }
                let s = &mut self.store.data.settings;
                s.shell = (!shell_t.is_empty()).then_some(shell_t);
                s.claude_path = platform::resolve_program(&claude_t);
                // Only the on/off state is edited here; a custom feed URL survives while checks stay on.
                if updates == (s.update_url.as_deref() == Some("")) {
                    s.update_url = (!updates).then(String::new);
                }
                s.scale = (scale != 100).then_some(scale);
                s.chrome_default = chrome;
                s.new_session = claude::defaults::NewSessionDefaults { model: model_name.unwrap_or_default(), effort, permission_mode: permission };
                let trimmed = claude::trim::normalized(&trimmed);
                let claudiu_off = mcp::normalized_features(&claudiu_off);
                let tools_changed = s.trimmed_tools != trimmed || s.claudiu_tools_off != claudiu_off;
                s.trimmed_tools = trimmed;
                s.claudiu_tools_off = claudiu_off;
                crate::panels::scale::set(scale);
                self.shell = platform::default_shell(s.shell.as_deref());
                self.claude_exe = claude::discover(s.claude_path.as_deref());
                self.store.mark_dirty();
                if tools_changed {
                    self.toast("Settings saved. Tool changes apply to new sessions; resumed sessions keep the old instructions until their next compaction.", false, cx);
                } else {
                    self.toast("Settings saved. Open sessions keep their current shell.", false, cx);
                }
                Act::Dismiss
            }
            None => return,
        };
        self.focus_active(window, cx);
        self.act(act, window, cx);
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
