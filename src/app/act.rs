//! `Act`: every operation a button, menu, shortcut or click can trigger, and `Workspace::act`, which runs one.
//! The work itself is in the topic files next to this one; `act` only routes to it.

use super::*;

/// Every user-triggerable operation, so buttons, menus and shortcuts share one code path.
#[derive(Clone, Debug)]
pub enum Act {
    NewClaude {
        cwd: PathBuf,
    },
    /// Make a worktree named `name` (random when blank) straight from the Branch & Worktree dialog: chosen for the
    /// Welcome screen's next session (`for_home`), or the shown session moves into it.
    QuickWorktree { main_checkout: PathBuf, name: String, base: Base, for_home: bool },
    CreateHomeWorktree {
        main_checkout: PathBuf,
        folder: PathBuf,
        base: Base,
    },
    /// Move the shown session into this working tree of its project (`base` is for a worktree that does not exist yet).
    SwitchWorktree {
        folder: PathBuf,
        base: Base,
    },
    /// `SwitchWorktree`, after the user agreed to leave uncommitted files behind.
    ConfirmSwitch {
        id: Id,
        folder: PathBuf,
        base: Base,
    },
    /// Ask before removing a worktree. A running session in it refuses.
    RemoveWorktree {
        main_checkout: PathBuf,
        folder: PathBuf,
    },
    ConfirmRemoveWorktree {
        main_checkout: PathBuf,
        folder: PathBuf,
        discard: bool,
    },
    /// Ask before deleting a folder in m4code's worktrees folder that git no longer lists.
    AskDeleteUnlisted(PathBuf),
    ConfirmDeleteUnlisted(PathBuf),
    /// Show or hide the shell under a Claude session's terminal (started on first use, in the session's folder).
    ToggleShell(Id),
    /// Return to the Welcome / New session launcher screen.
    ShowHome,
    HomeSelectProject(Id),
    HomeSelectWorktree(PathBuf),
    HomeSelectModel(crate::harness::CatalogModel),
    HomeSetEffort(Effort),
    HomeLaunch,
    /// List the default agent's sessions in a folder, to pick one up here.
    ListSessions { cwd: PathBuf },
    /// Open a session an agent listed.
    PickUp { harness: Harness, cwd: PathBuf, session_id: String, title: Option<String> },
    /// Turn editor mode on or off for the shown session (see `editor_mode.rs`).
    ToggleEditor,
    /// Click on a session row: live -> show it, history -> resume it.
    Select(Id),
    Resume(Id),
    CloseLive(Id),
    /// End the session although its editor has unsaved files (the user confirmed).
    ConfirmCloseLive(Id),
    /// Close an editor tab and drop its unsaved edits (the user confirmed).
    DiscardTab(Id, PathBuf),
    Rename(Id),
    /// Hide (true) or restore (false) a session in the sidebar. Archived sessions still turn up in search.
    Archive(Id, bool),
    CopyText(String),
    Reveal(PathBuf),
    OpenEditor(Editor, PathBuf),
    ImportProject,
    OpenSettings,
    /// The setup window again, from Settings or the missing-`claude` toast (see `setup.rs`).
    OpenSetup,
    /// A choice in the Settings dialog's default-model, effort, or permission-mode picker; kept in the dialog until Save.
    PickDefaultModel(crate::harness::CatalogModel),
    PickEffort(Effort),
    PickPermission(String),
    /// The project's GitHub issues and pull requests.
    OpenGithub {
        cwd: PathBuf,
    },
    /// Have the running session's own mod (Haiku) write a new title from what was said in it.
    RegenTitle(Id),
    /// Change a setting of a running session through its mod (see `usage::send_ctl`).
    SetAutoCompact(Id, bool),
    SetPrecomputeCompact(Id, bool),
    /// Filter the sidebar list to a project (`None`: all).
    FilterProject(Option<Id>),
    /// Stage everything in this folder's repository, commit it with a message the running session's mod (Haiku) writes, push.
    CommitPush(Id, PathBuf),
    Pull(PathBuf),
    SwitchBranch(PathBuf, String),
    /// Read what deleting the local branch would lose, then ask.
    DeleteBranch(PathBuf, String),
    ConfirmDeleteBranch(PathBuf, String),
    /// Ask for a name, then `CreateBranch`.
    NewBranch(PathBuf),
    CreateBranch(PathBuf, String),
    /// New Claude session (Sonnet 5.5, medium effort) that resolves a failed pull / push in this repository.
    FixGitWithClaude(PathBuf),
    AddProject(PathBuf),
    RemoveProject(Id),
    /// Undo a removal: the project, back where it was in the list.
    RestoreProject(usize, Project),
    /// Start renaming a project on the Projects page of Settings.
    RenameProject(Id),
    SaveProjectName,
    CancelProjectRename,
    /// Move a project one place up (true) or down the list.
    MoveProject(Id, bool),
    ToggleRecent,
    /// The Check now button in Settings.
    CheckForUpdates,
    ConfirmQuit,
    ConfirmInstallUpdate,
    Dismiss,
    InstallUpdate,
    /// New sessions start with this account; running ones keep theirs.
    SwitchAccount(Id),
    /// The shown session runs under another account: restart it under the active one (asks first, see `handoff.rs`).
    Handoff,
    ConfirmHandoff(Id),
    /// A session's request to open another one with a first message, by request id (see `cross.rs`).
    ConfirmStartSession(String),
    /// Shows the sharing warning, then `ConfirmAddAccount`.
    AddAccount,
    ConfirmAddAccount,
    /// Run `claude auth login` for an alt account in a terminal tab.
    SignIn(Id),
    RemoveAccount(Id),
    ConfirmRemoveAccount(Id),
    /// Editor mode's file tree: ask for a file name (new or renamed), ask before deleting, delete.
    AskFileName(Id, NameFor),
    AskDelete(Id, PathBuf),
    ConfirmDelete(Id, PathBuf),
    /// An entry of the editor's right-click menu.
    Editor(EditorCommand),
    /// Settings → Skills (`skills.sh` browser, installer and global/per-project toggles).
    SetSkillsTab(SkillsTab),
    SetSkillScope(SkillScope),
    SearchSkills,
    InstallSkill(crate::skills::registry::CatalogEntry),
    InstallSkillRepo(String),
    ToggleSkill(String),
    UninstallSkill(String),
    /// Download the latest version of these installed skills (one, one repository's, or all).
    UpdateSkills(Vec<String>),
}

impl Workspace {
    pub fn act(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        // Dragging the effort slider inside the Home model picker keeps that dialog open.
        // A pick made in a dropdown hands the keyboard back to the message box under it; an act that opens
        // something else moves focus on from there.
        if !matches!(act, Act::HomeSetEffort(_)) && self.picker.take().is_some() && self.modal.is_none() {
            self.focus_active(window, cx);
        }
        match act {
            Act::NewClaude { cwd } => self.new_claude(cwd, window, cx),
            Act::QuickWorktree { main_checkout, name, base, for_home } => match worktrees::new_worktree_act(&main_checkout, &name, base, for_home) {
                Ok(act) => {
                    self.picker = None;
                    self.act(act, window, cx);
                }
                Err(why) => self.toast(why, true, cx),
            },
            Act::CreateHomeWorktree { main_checkout, folder, base } => self.create_home_worktree(main_checkout, folder, base, window, cx),
            Act::SwitchWorktree { folder, base } => self.switch_shown(folder, base, false, window, cx),
            Act::ConfirmSwitch { id, folder, base } => {
                self.modal = None;
                self.switch_session(id, folder, base, true, window, cx)
            }
            Act::RemoveWorktree { main_checkout, folder } => self.ask_remove_worktree(main_checkout, folder, cx),
            Act::AskDeleteUnlisted(folder) => self.ask_delete_unlisted(folder, cx),
            Act::ConfirmDeleteUnlisted(folder) => {
                self.worktrees_page.asking = None;
                self.run_git(GitOp::Worktree, move || Ok(crate::git::worktree::delete_unlisted(&folder)?), cx);
            }
            Act::ConfirmRemoveWorktree { main_checkout, folder, discard } => {
                self.worktrees_page.asking = None;
                self.run_git(
                    GitOp::Worktree,
                    move || Ok(crate::git::worktree::remove(&main_checkout, &folder, discard, Duration::from_secs(10))?),
                    cx,
                );
            }
            Act::ToggleShell(id) => self.toggle_shell(&id, window, cx),
            Act::ShowHome => {
                self.main = Main::Home;
                self.editor_mode = false;
                self.right_gh = None;
                self.home.prompt.update(cx, |p, cx| p.focus(window, cx));
            }
            Act::HomeSelectProject(id) => {
                self.home.project = Some(id);
                self.home.worktree = None;
            }
            Act::HomeSelectWorktree(path) => {
                self.home.worktree = Some(path);
            }
            Act::HomeSelectModel(model) => {
                self.home.model = Some(model);
            }
            Act::HomeSetEffort(effort) => {
                self.home.effort = effort;
            }
            Act::HomeLaunch if self.home.creating.is_some() => {
                // The worktree is on its way; the session starts in it as soon as git is done.
                self.home.launch_when_created = true;
            }
            Act::HomeLaunch => {
                let cwd = self.home_target_cwd();
                let chosen = self.home_selected_model();
                let effort = self.home.effort;
                let text = self.home.prompt.read(cx).value().trim().to_string();
                let prompt = (!text.is_empty()).then_some(text);
                if prompt.is_some() {
                    self.home.prompt.update(cx, |p, cx| p.set_value("", window, cx));
                }
                let model = (!chosen.id.is_empty()).then_some(chosen.id);
                self.new_session_from_home(chosen.harness, model, cwd, prompt, Some(effort), window, cx);
            }
            Act::ListSessions { cwd } => self.list_sessions(cwd, window.mouse_position(), cx),
            Act::PickUp { harness, cwd, session_id, title } => self.pick_up(harness, cwd, session_id, title, window, cx),
            Act::ToggleEditor => self.toggle_editor(window, cx),
            Act::Editor(command) => self.run_editor_command(command, window, cx),
            Act::Select(id) => self.show_session(&id, window, cx),
            Act::Resume(id) => self.resume(&id, window, cx),
            Act::CloseLive(id) => self.request_close_live(&id, window, cx),
            Act::ConfirmCloseLive(id) => {
                self.modal = None;
                self.close_live(&id, window, cx)
            }
            Act::DiscardTab(id, path) => {
                self.modal = None;
                if let Some(ix) = self.panes.get(&id).and_then(|p| p.tabs.iter().position(|t| t.path == path)) {
                    self.close_tab(&id, ix, cx);
                }
            }
            Act::Rename(id) => {
                let text = self
                    .store
                    .session(&id)
                    .map(|s| s.display_title().to_string())
                    .unwrap_or_default();
                let input = self.modal_input(&text, "", window, cx);
                self.modal = Some(Modal::Rename { id, input });
            }
            Act::Archive(id, archive) => {
                if let Some(rec) = self.store.session_mut(&id) {
                    rec.hidden = archive;
                }
                if archive && self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = None;
                }
                if archive {
                    self.toast("Archived. Search still finds it; the Claude transcript was not touched.", false, cx);
                }
            }
            Act::CopyText(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.toast("Copied", false, cx);
            }
            Act::Reveal(path) => cx.reveal_path(&path),
            Act::OpenEditor(editor, path) => {
                // The last editor used becomes the "Open in" default.
                if self.store.data.settings.editor != Some(editor) {
                    self.store.data.settings.editor = Some(editor);
                    self.store.mark_dirty();
                }
                if let Err(e) = editor.open(&path) {
                    self.toast(e, true, cx);
                }
            }
            Act::ImportProject => self.import_project(window, cx),
            Act::OpenGithub { cwd } => {
                // GitHub opens in the right sidebar, which editor mode hides.
                self.editor_mode = false;
                self.open_github(cwd, window, cx)
            }
            Act::RegenTitle(id) => self.regen_title(&id, cx),
            Act::CommitPush(_id, cwd) => {
                self.run_git(
                    GitOp::CommitPush,
                    move || {
                        crate::git::commit::commit_and_push(&|_system, prompt| Ok(summarize_commit_diff(prompt)), &cwd)
                            .map_err(|e| crate::git::commit::classify(&cwd, e))
                    },
                    cx,
                )
            }
            Act::Pull(cwd) => self.run_git(
                GitOp::Pull,
                move || crate::git::commit::pull(&cwd).map_err(|e| crate::git::commit::classify(&cwd, e)),
                cx,
            ),
            Act::SwitchBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::git::commit::switch_branch(&cwd, &name)?), cx)
            }
            Act::DeleteBranch(cwd, name) => self.ask_delete_branch(cwd, name, cx),
            Act::ConfirmDeleteBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::git::commit::delete_branch(&cwd, &name)?), cx)
            }
            Act::FixGitWithClaude(cwd) => {
                let prompt = "m4code's Pull or Commit & Push just failed in this repository because of a merge conflict, a diverged branch or a rejected push. Run git status, fix it (resolve conflicts keeping the intent of both sides, finish the merge or rebase, then push if the push was rejected). Ask me whenever a conflict is ambiguous or you are unsure which side to keep.";
                match self.running_claude_in(&cwd, cx) {
                    Some(id) => self.send_to_claude(&id, prompt, window, cx),
                    None => {
                        // No session to ask: a short-lived one on medium effort (mid-sized Claude model when on Claude).
                        let mut options = vec![("thought_level", "medium".to_string())];
                        if self.store.data.settings.harness == Harness::Claude {
                            options.push(("model", "claude-sonnet-5-5".to_string()));
                        }
                        let before = self.selected.clone();
                        self.new_claude_with(cwd, Some(prompt.to_string()), options, Starter::FixGit, window, cx);
                        if self.selected != before
                            && let Some(id) = self.selected.clone() {
                                self.ephemeral.insert(id);
                            }
                    }
                }
            }
            Act::OpenSetup => self.open_setup(window, cx),
            Act::OpenSettings => {
                let s = &self.store.data.settings;
                let (shell_t, claude_t) = (
                    s.shell.clone().unwrap_or_default(),
                    s.claude_path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                );
                let updates = s.update_url.as_deref() != Some("");
                let chrome = s.chrome_default;
                let default_model = s.default_model();
                let claude_hint = self.claude_exe.as_ref().map_or("not found".to_string(), |p| p.to_string_lossy().into_owned());
                let defaults = s.new_session.clone();
                let claude = self.modal_input(&claude_t, &claude_hint, window, cx);
                let shell = self.modal_input(&shell_t, &platform::default_shell(None).0, window, cx);
                let skills_field = self.modal_input(&self.store.data.settings.limits.text(), "none, e.g. dataviz, claude-api", window, cx);
                let skill_search = self.modal_input_then("", "Search skills.sh or enter owner/repo", |ws, _, cx| ws.search_skills(cx), window, cx);
                let list_focus = cx.focus_handle();
                window.focus(&list_focus, cx);
                let custom = &self.store.data.settings.custom_subagents;
                let form = |c: &claude::subagents::AgentChoice| AgentForm { on: c.on, models: c.models.clone(), edits: c.edits };
                let subagents = Box::new(HarnessesForm {
                    enabled: self.store.data.settings.enabled_harnesses_list(),
                    agents: claude::subagents::AgentMap::from_fn(|agent| form(custom.get(agent))),
                    claude_models: self.store.data.settings.claude_picked.clone(),
                    picking: None,
                });
                let modes = self.store.data.settings.modes_for(default_model.harness);
                let initial_perm = defaults
                    .mode_id
                    .clone()
                    .or_else(|| defaults.permission_mode.map(|m| m.acp_id().to_string()))
                    .filter(|id| modes.iter().any(|(mid, _)| mid == id))
                    .or_else(|| modes.first().map(|(id, _)| id.clone()))
                    .unwrap_or_else(|| "default".into());
                self.modal = Some(Modal::Settings { shell, claude, default_model, effort: defaults.effort.unwrap_or_default(), permission: initial_perm, updates, chrome, scale: crate::panels::scale::percent(), m4code_off: self.store.data.settings.m4code_tools_off.clone(), skills_field, skill_search, costly: claude::costly::scan(), list_focus, renaming: None, subagents });
                // After the modal exists: a page that loads something reads the dialog's fields.
                self.set_settings_page(self.settings_page, cx);
            }
            Act::SetSkillsTab(tab) => {
                self.skills_page.tab = tab;
                if tab == SkillsTab::Browse {
                    self.load_skills_catalog(cx);
                }
            }
            Act::SetSkillScope(scope) => {
                self.skills_page.scope = scope;
                self.load_repo_skills(cx);
            }
            Act::SearchSkills => self.search_skills(cx),
            Act::InstallSkill(entry) => self.install_skill(entry, cx),
            Act::InstallSkillRepo(source) => self.install_skill_repo(source, cx),
            Act::ToggleSkill(name) => self.toggle_skill(name, cx),
            Act::UninstallSkill(name) => self.uninstall_skill(name, cx),
            Act::UpdateSkills(names) => self.update_skills(names, cx),
            Act::PickDefaultModel(choice) => {
                let modes = self.store.data.settings.modes_for(choice.harness);
                if let Some(Modal::Settings { default_model, permission, list_focus, .. }) = self.modal.as_mut() {
                    if !modes.iter().any(|(id, _)| id == permission)
                        && let Some((first, _)) = modes.first()
                    {
                        *permission = first.clone();
                    }
                    *default_model = choice;
                    window.focus(list_focus, cx);
                }
            }
            Act::PickEffort(choice) => {
                if let Some(Modal::Settings { effort, .. }) = self.modal.as_mut() {
                    *effort = choice;
                }
            }
            Act::PickPermission(choice) => {
                if let Some(Modal::Settings { permission, .. }) = self.modal.as_mut() {
                    *permission = choice;
                }
            }
            Act::NewBranch(cwd) => {
                let input = self.modal_input("", "", window, cx);
                self.modal = Some(Modal::NewBranch { cwd, input });
            }
            Act::CreateBranch(cwd, name) => {
                self.run_git(GitOp::Branch, move || Ok(crate::git::commit::create_branch(&cwd, &name)?), cx)
            }
            Act::AddProject(path) => {
                let id = self.store.add_project(&path);
                let _ = id;
                self.toast("Project added", false, cx);
            }
            Act::RemoveProject(id) => {
                let Some((at, project)) = self.store.remove_project(&id) else { return };
                if self.store.data.layout.project_filter.as_ref() == Some(&id) {
                    self.store.data.layout.project_filter = None;
                }
                if let Some(Modal::Settings { renaming, .. }) = self.modal.as_mut()
                    && renaming.as_ref().is_some_and(|(r, _)| *r == id)
                {
                    *renaming = None;
                }
                self.show_toast("Project removed. Its sessions are kept.".into(), false, Some(("Undo".into(), Act::RestoreProject(at, project))), cx);
            }
            Act::RestoreProject(at, project) => self.store.restore_project(at, project),
            Act::RenameProject(id) => {
                let Some(name) = self.store.project(&id).map(|p| p.name.clone()) else { return };
                let input = self.modal_input_then(&name, "Name", |ws, window, cx| ws.act(Act::SaveProjectName, window, cx), window, cx);
                if let Some(Modal::Settings { renaming, .. }) = self.modal.as_mut() {
                    *renaming = Some((id, input));
                }
            }
            Act::SaveProjectName => {
                if let Some(Modal::Settings { renaming, .. }) = self.modal.as_mut()
                    && let Some((id, name)) = renaming.take()
                {
                    let name = name.read(cx).value().to_string();
                    self.store.rename_project(&id, &name);
                }
            }
            Act::CancelProjectRename => {
                if let Some(Modal::Settings { renaming, .. }) = self.modal.as_mut() {
                    *renaming = None;
                }
            }
            Act::MoveProject(id, up) => self.store.move_project(&id, up),
            Act::SetAutoCompact(id, on) => {
                // Show the choice at once; the mod's next snapshot confirms or corrects it.
                if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.claude.as_mut()) {
                    meta.auto_compact = Some(on);
                }
                self.store.mark_dirty();
                cx.notify();
                usage::send_ctl(&usage::ctl_file(&id), "auto_compact", on.into());
                self.toast(format!("Turning auto-compact {}…", if on { "on" } else { "off" }), false, cx);
            }
            Act::SetPrecomputeCompact(id, on) => {
                if let Some(meta) = self.store.session_mut(&id).and_then(|s| s.claude.as_mut()) {
                    meta.precompute_compact = Some(on);
                }
                self.store.mark_dirty();
                cx.notify();
                usage::send_ctl(&usage::ctl_file(&id), "precompute_compact", on.into());
                self.toast(format!("Turning background compaction {}…", if on { "on" } else { "off" }), false, cx);
            }
            Act::FilterProject(id) => {
                self.store.data.layout.project_filter = id;
                self.store.mark_dirty();
            }
            Act::ToggleRecent => {
                let l = &mut self.store.data.layout;
                l.recent_open = !l.recent_open;
                self.store.mark_dirty();
            }
            Act::ConfirmQuit => {
                self.modal = None;
                self.finalize(cx);
                cx.quit();
            }
            Act::Dismiss => {
                if matches!(self.modal, Some(Modal::Settings { .. })) {
                    self.close_settings(window, cx);
                    return;
                }
                self.modal = None;
                self.focus_active(window, cx);
            }
            Act::InstallUpdate => {
                let ready = matches!(self.update.status, crate::updater::Status::Ready { .. });
                let running = self.running_count(cx);
                if ready && running > 0 {
                    self.modal = Some(Modal::Confirm {
                        title: "Restart to update?".into(),
                        body: format!(
                            "{running} session{} running. Restarting ends {}. Claude sessions can be resumed afterwards.",
                            if running == 1 { " is" } else { "s are" },
                            if running == 1 { "it" } else { "them" }
                        ),
                        confirm: "Restart and update".into(),
                        danger: true,
                        act: Act::ConfirmInstallUpdate,
                    });
                    window.focus(&self.modal_focus, cx);
                } else {
                    crate::updater::install(self, cx);
                }
            }
            Act::CheckForUpdates => crate::updater::check_now(self, cx),
            Act::ConfirmInstallUpdate => {
                self.modal = None;
                crate::updater::install(self, cx);
            }
            Act::SwitchAccount(id) => {
                self.store.data.active_account = Some(id);
                self.store.mark_dirty();
                self.refresh_accounts(cx);
                self.model_lookups.get_mut(Harness::Claude).fetched = false;
                if !cfg!(test) {
                    self.load_enabled_models(cx);
                }
                self.toast("Switched. New and resumed sessions use this account; running sessions keep theirs.", false, cx);
            }
            Act::Handoff => self.ask_handoff(window, cx),
            Act::ConfirmStartSession(req_id) => self.confirm_start(&req_id, window, cx),
            Act::ConfirmHandoff(id) => {
                self.modal = None;
                self.start_handoff(&id, window, cx);
            }
            Act::AddAccount => {
                self.modal = Some(Modal::Confirm {
                    title: "Add an account?".into(),
                    body: "Only sign in to accounts you own. Sharing or pooling logins, or using several accounts to get around \
                           usage limits, can break Anthropic's terms.\n\n\
                           The new account uses your main Claude Code setup. Settings, CLAUDE.md, keybindings, MCP servers and project trust \\
                           are kept the same in every account, both ways: a change made here (like /model) also changes ~/.claude and \\
                           your other accounts, and the other way round. Agents, skills, plugins and session history are shared with \\
                           ~/.claude. Only the login is separate. m4code creates this account's folder."
                        .into(),
                    confirm: "Add and sign in".into(),
                    danger: false,
                    act: Act::ConfirmAddAccount,
                });
                window.focus(&self.modal_focus, cx);
            }
            Act::ConfirmAddAccount => {
                self.modal = None;
                match claude::claude_home().ok_or_else(|| std::io::Error::other("Claude Code's folder was not found")).and_then(|h| accounts::create(&h)) {
                    Ok(dir) => {
                        let id = new_id();
                        self.store.data.accounts.push(Account { id: id.clone(), dir: Some(dir), ..Default::default() });
                        self.store.data.active_account = Some(id.clone());
                        self.store.mark_dirty();
                        self.sign_in(&id, window, cx);
                    }
                    Err(e) => self.toast(format!("Could not create the account folder: {e}"), true, cx),
                }
            }
            Act::SignIn(id) => self.sign_in(&id, window, cx),
            Act::RemoveAccount(id) => {
                let name = self.store.account(&id).and_then(|a| a.email.clone()).unwrap_or_else(|| "this account".into());
                self.modal = Some(Modal::Confirm {
                    title: format!("Remove {name}?"),
                    body: "Deletes this account's login from m4code. Shared settings and session history stay in your main \
                           ~/.claude. End its running sessions first."
                        .into(),
                    confirm: "Remove".into(),
                    danger: true,
                    act: Act::ConfirmRemoveAccount(id),
                });
                window.focus(&self.modal_focus, cx);
            }
            Act::AskFileName(id, purpose) => self.ask_file_name(id, purpose, window, cx),
            Act::AskDelete(id, path) => self.ask_delete(id, path, window, cx),
            Act::ConfirmDelete(id, path) => self.delete_path(id, path, cx),
            Act::ConfirmRemoveAccount(id) => {
                self.modal = None;
                if let Some(dir) = self.store.account(&id).and_then(|a| a.dir.clone()) {
                    match accounts::remove(&dir) {
                        Ok(()) => {
                            self.store.data.accounts.retain(|a| a.id != id);
                            if self.store.data.active_account.as_deref() == Some(id.as_str()) {
                                self.store.data.active_account = None;
                            }
                            self.store.mark_dirty();
                        }
                        Err(e) => self.toast(format!("Could not remove the account: {e}"), true, cx),
                    }
                }
            }
        }
        self.store.save_if_dirty();
        cx.notify();
    }
}

/// Deterministic, harness-agnostic commit subject derived from `git diff --cached --stat --patch`.
fn summarize_commit_diff(prompt: &str) -> String {
    let files: Vec<&str> = prompt
        .lines()
        .filter_map(|line| line.strip_prefix("diff --git a/").and_then(|rest| rest.split(" b/").nth(1)))
        .collect();
    match files.as_slice() {
        [] => "Update files".into(),
        [only] => format!("Update {only}"),
        [a, b] => format!("Update {a} and {b}"),
        many => format!("Update {} files", many.len()),
    }
}
