//! Overlays: toasts, the context menu, input modals, the editor choice and the project import.

use super::*;
use crate::git::worktree;

impl Workspace {
    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.show_toast(text.into(), error, None);
    }

    pub(super) fn show_toast(&mut self, text: String, error: bool, action: Option<(String, Act)>) {
        self.toast_serial += 1;
        let serial = self.toast_serial;
        let secs = if action.is_some() { 20 } else { 5 };
        self.toast = Some(Toast {
            text,
            error,
            serial,
            action,
        });
        self.rt.after(Duration::from_secs(secs), move |ws: &mut Workspace| {
            if ws.toast.as_ref().is_some_and(|t| t.serial == serial) {
                ws.toast = None;
            }
        });
    }

    /// Enter / OK in a modal.
    pub fn submit_modal(&mut self) {
        let act = match self.modal.take() {
            Some(Modal::Confirm { act, .. }) => act,
            Some(Modal::Rename { id, input }) => {
                let text = input.text.as_str().trim().to_string();
                if let Some(rec) = self.store.session_mut(&id) {
                    rec.custom_title = (!text.is_empty()).then_some(text);
                }
                Act::Dismiss
            }
            Some(Modal::Advisor { id, input }) => {
                let name = input.text.as_str().trim().to_lowercase();
                if name.is_empty() {
                    Act::Dismiss
                } else if !usage::valid_model_name(&name) {
                    self.modal = Some(Modal::Advisor { id, input });
                    self.toast("Use a model alias or id (letters, digits, . _ -), or “off”.", true);
                    return;
                } else {
                    self.act(Act::SetAdvisor(id, name));
                    Act::Dismiss
                }
            }
            Some(Modal::NewBranch { cwd, input }) => {
                // Spaces aren't allowed in branch names; git reports anything else that is invalid.
                let name = input.text.as_str().split_whitespace().collect::<Vec<_>>().join("-");
                if name.is_empty() { Act::Dismiss } else { Act::CreateBranch(cwd, name) }
            }
            Some(Modal::NewWorktree { main_checkout, input, base }) => {
                let typed = input.text.as_str().trim().to_string();
                let name = if typed.is_empty() {
                    Ok(worktree::generate_name(uuid::Uuid::new_v4().as_u128(), worktree::name_taken(&main_checkout)))
                } else {
                    worktree::check_name(&typed).and_then(|()| worktree::check_unused(&main_checkout, &typed)).map(|()| typed)
                };
                match name {
                    Ok(name) => Act::SwitchWorktree { folder: worktree::folder(&main_checkout, &name), base },
                    Err(why) => {
                        self.modal = Some(Modal::NewWorktree { main_checkout, input, base });
                        self.toast(why, true);
                        return;
                    }
                }
            }
            Some(Modal::Settings { shell, claude, model, effort, permission, updates, chrome, scale, trimmed, claudiu_off, skills_field, costly, hints }) => {
                let clean = |s: &str| s.trim().trim_matches('"').to_string();
                let (shell_t, claude_t) = (clean(&shell.text), clean(&claude.text));
                let model_name = claude::defaults::parse_model(&model.text);
                // In category order, then top to bottom: the first bad field decides which page the dialog shows.
                // Fields are numbered as `modal_field` numbers them.
                let paths = [("Shell", 0, &shell_t), ("Claude path", 1, &claude_t)]
                    .into_iter()
                    .find(|(_, _, t)| !t.is_empty() && platform::resolve_program(t).is_none())
                    .map(|(what, field, t)| (SettingsPage::General, field, format!("{what}: “{t}” is not a file or a program on PATH")));
                let bad_model = model_name.is_err().then(|| (SettingsPage::Sessions, 2, "Use a model alias or id (letters, digits, . _ -).".to_string()));
                let limits = claude::trim::Limits::parse(&skills_field.text);
                let bad_limits = limits.as_ref().err().map(|why| (SettingsPage::ClaudeTools, 3, why.clone()));
                if let Some((page, field, msg)) = paths.or(bad_model).or(bad_limits) {
                    self.settings_page = page;
                    self.modal = Some(Modal::Settings { shell, claude, model, effort, permission, updates, chrome, scale, trimmed, claudiu_off, skills_field, costly, hints });
                    if let Some(f) = modal_field(&mut self.modal, field) {
                        f.focused = true;
                    }
                    self.toast(msg, true);
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
                // The trims go into Claude Code's own settings.json (every account gets the file through config_sync).
                // When that fails the old trims stay, so what the page shows is what Claude Code has.
                let mut trim_error = None;
                let mut trimmed = trimmed;
                let mut limits = limits.unwrap_or_default();
                if s.trimmed_tools != trimmed || s.limits != limits {
                    let written = claude::claude_home()
                        .ok_or_else(|| std::io::Error::other("Claude Code's folder was not found"))
                        .and_then(|home| claude::trim::write_global(&home.join("settings.json"), &trimmed, &limits, &s.applied_trims));
                    match written {
                        Ok(owned) => s.applied_trims = owned,
                        Err(e) => {
                            trim_error = Some(e);
                            trimmed = s.trimmed_tools.clone();
                            limits = s.limits.clone();
                        }
                    }
                }
                let tools_changed = s.trimmed_tools != trimmed || s.limits != limits || s.claudiu_tools_off != claudiu_off;
                s.trimmed_tools = trimmed;
                s.limits = limits;
                s.claudiu_tools_off = claudiu_off;
                crate::panels::scale::set(scale);
                self.shell = platform::default_shell(s.shell.as_deref());
                self.claude_exe = claude::discover(s.claude_path.as_deref());
                self.store.mark_dirty();
                if let Some(e) = trim_error {
                    self.toast(format!("Settings saved, but Claude Code's settings.json could not be changed ({e}), so the tool trims stay as they were."), true);
                } else if tools_changed {
                    self.toast("Settings saved. Tool changes apply to new sessions; resumed sessions keep the old instructions until their next compaction.", false);
                } else {
                    self.toast("Settings saved. Open sessions keep their current shell.", false);
                }
                Act::Dismiss
            }
            None => return,
        };
        self.focus_active();
        self.act(act);
    }

    /// The editor "Open in" uses: the last one picked if it is still installed, else the first one found.
    pub fn preferred_editor(&self) -> Option<Editor> {
        let installed = |e: &Editor| self.editors.iter().any(|(x, ok)| x == e && *ok);
        self.store.data.settings.editor.filter(installed).or_else(|| Editor::ALL.into_iter().find(installed))
    }

    pub(super) fn import_project(&mut self) {
        self.rt.with_tx(|tx| {
            let options = rune::FileDialogOptions { title: "Add project folder".into(), ..Default::default() };
            tx.open_folder(options, |ws: &mut Workspace, path: rune::FileDialogResult, tx: &mut Tx| {
                if let Ok(Some(path)) = path {
                    ws.act(Act::AddProject(path));
                }
                ws.after_input(tx);
            });
        });
    }

    /// Open a menu at `anchor`, at least `min_w` wide.
    pub fn open_menu(&mut self, anchor: MenuAnchor, min_w: f32, items: Vec<MenuItem>) {
        self.menu_serial += 1;
        self.menu = Some(Menu { anchor, min_w, items, serial: self.menu_serial });
    }
}

/// A dialog's text field holding `value`, with the keyboard when `focused`.
pub(super) fn field(value: &str, focused: bool) -> TextEdit {
    let mut edit = TextEdit::new(value);
    edit.set_caret(value.len());
    edit.focused = focused;
    edit
}
