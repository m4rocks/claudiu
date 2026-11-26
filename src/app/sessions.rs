//! Sessions: starting, resuming, showing, closing and switching them, and the queries the rest of the app asks about them.

use super::*;

/// Item `n` (from 1) of `items`.
fn numbered<T>(items: &[T], n: usize) -> Option<&T> {
    items.get(n.checked_sub(1)?)
}


impl Workspace {
    pub fn is_running(&self, id: &str, cx: &App) -> bool {
        self.live
            .iter()
            .any(|l| l.id == id && l.view.is_running(cx))
    }

    pub fn running_count(&self, cx: &App) -> usize {
        self.live
            .iter()
            .filter(|l| l.view.is_running(cx))
            .count()
    }

    pub fn live_view(&self, id: &str) -> Option<&SessionView> {
        self.live.iter().find(|l| l.id == id).map(|l| &l.view)
    }

    pub fn active_id(&self) -> Option<&Id> {
        match self.main {
            Main::Terminals => self.active.as_ref(),
            Main::Home => None,
        }
    }

    /// Where a new Claude session starts: the project the sidebar is filtered to, else where the shown session is.
    pub fn new_session_cwd(&self) -> PathBuf {
        self.project_filter().map_or_else(|| self.active_cwd(), |p| p.path.clone())
    }

    pub fn active_cwd(&self) -> PathBuf {
        if let Some(id) = self.active_id()
            && let Some(s) = self.store.session(id)
        {
            return s.cwd.clone();
        }
        self.home_target_cwd()
    }

    pub fn new_session(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session_default(cwd, None, vec![], Starter::User, window, cx);
    }

    /// A new session in the default harness, sent `prompt` once it is ready. `options` (by category, such as
    /// `model`) win over the new-session defaults.
    pub(super) fn new_session_default(&mut self, cwd: PathBuf, prompt: Option<String>, options: Vec<(&'static str, String)>, by: Starter, window: &mut Window, cx: &mut Context<Self>) {
        let harness = self.store.data.settings.default_model().harness;
        self.new_session_with(harness, cwd, prompt, options, by, None, window, cx);
    }

    /// A new session in `harness` directly inside `cwd` (preserving a chosen worktree) with optional model, effort and permission mode overrides.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_session_from_home(&mut self, harness: Harness, model: Option<String>, cwd: PathBuf, prompt: Option<String>, effort: Option<Effort>, permission_mode: Option<Permission>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let record_id = new_id();
        let meta = AgentMeta {
            chrome: self.store.data.settings.chrome_default,
            harness,
            chosen_model: model.clone().filter(|m| !m.is_empty()),
            chosen_effort: effort.map(|e| e.name().to_string()),
            ..Default::default()
        };
        let Some(mut start) = self.thread_start(&record_id, &cwd, &meta, None, prompt, Starter::User, cx) else { return false };
        if let Some(permission) = permission_mode {
            start.permission = Some(permission);
        }
        if let Some(model_id) = model.filter(|m| !m.is_empty()) {
            start.options.retain(|(category, _)| *category != "model");
            start.options.push(("model", model_id));
        }
        if let Some(effort) = effort {
            start.options.retain(|(category, _)| *category != "thought_level");
            start.options.push(("thought_level", effort.name().to_string()));
        }
        let record = SessionRecord {
            id: record_id,
            kind: SessionKind::Agent,
            title: "New session".into(),
            custom_title: None,
            auto_title: None,
            project_id: self.store.project_for_cwd(&cwd),
            cwd,
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            agent: Some(meta),
        };
        self.start_thread(record, start, true, window, cx);
        true
    }

    /// A new session in `harness`, as `new_session_default`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_session_with(&mut self, harness: Harness, cwd: PathBuf, prompt: Option<String>, options: Vec<(&'static str, String)>, by: Starter, permission: Option<Permission>, window: &mut Window, cx: &mut Context<Self>) {
        // A new session starts in the main checkout, not in a linked worktree. Fix-with-Claude works where the failure was.
        let cwd = if matches!(by, Starter::FixGit | Starter::OtherSession) { cwd } else { self.outside_linked_worktree(cwd) };
        let record_id = new_id();
        let meta = AgentMeta {
            chrome: self.store.data.settings.chrome_default,
            harness,
            chosen_model: options.iter().find(|(k, _)| *k == "model").map(|(_, v)| v.clone()),
            chosen_effort: options.iter().find(|(k, _)| *k == "thought_level").map(|(_, v)| v.clone()),
            ..Default::default()
        };
        let Some(mut start) = self.thread_start(&record_id, &cwd, &meta, None, prompt, by, cx) else { return };
        start.permission = permission.or(start.permission);
        start.options.retain(|(category, _)| options.iter().all(|(c, _)| c != category));
        start.options.extend(options);
        let record = SessionRecord {
            id: record_id,
            kind: SessionKind::Agent,
            title: "New session".into(),
            custom_title: None,
            auto_title: None,
            project_id: self.store.project_for_cwd(&cwd),
            cwd,
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            agent: Some(meta),
        };
        self.start_thread(record, start, true, window, cx);
    }

    /// What a harness session starts with: m4code's MCP server, which every harness gets, and for Claude Code its
    /// mod and flags. A new session (no id yet) also gets the new-session defaults. None, with a toast, when
    /// Claude Code isn't installed.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn thread_start(&mut self, record_id: &str, cwd: &std::path::Path, meta: &AgentMeta, name: Option<String>, prompt: Option<String>, by: Starter, cx: &mut Context<Self>) -> Option<crate::thread::Start> {
        let session_id = Some(meta.session_id.clone()).filter(|id| !id.is_empty());
        let settings = &self.store.data.settings;
        let adapter = meta.harness.adapter();
        let mut plan = harness::SessionPlan {
            off: mcp::tools_off(&settings.m4code_tools_off, &[]),
            ..Default::default()
        };
        if !adapter.fallback_question {
            plan.off.push("ask_user");
        }
        let (mut permission, options) = match session_id {
            None => {
                // Only inherit the default model id when starting on the harness that serves it.
                let mut defaults = settings.new_session.clone();
                if defaults.effort.is_none() {
                    defaults.effort = Some(harness::defaults::Effort::default());
                }
                if meta.harness != settings.harness {
                    defaults.model = None;
                }
                let permission = meta.permission.or(defaults.permission_mode)
                    .filter(|p| *p != Permission::ReadOnly || by != Starter::FixGit);
                (permission, harness::defaults::config_options(&defaults))
            }
            Some(_) => (
                meta.permission.or_else(|| (adapter.full_access_relaunch && meta.antigravity_full_access).then_some(Permission::FullAccess)),
                resume_options(meta),
            ),
        };
        if meta.read_only_pending {
            permission = Some(Permission::ReadOnly);
        }
        let child_edits = meta.parent_session.as_ref().map(|_| meta.subagent_edits);
        if child_edits.is_some() {
            permission = None;
        }
        let input = harness::SessionInput {
            record: record_id,
            cwd,
            session_id: session_id.as_deref(),
            name,
            child_edits,
            settings,
            executable_override: self.claude_exe.as_deref(),
            chrome: meta.chrome,
        };
        if let Err(error) = (adapter.prepare)(&input, &mut plan) {
            self.show_toast(error, Some(("Check setup".into(), Act::OpenSetup)), cx);
            return None;
        }
        let targets: Vec<_> = settings.enabled_harnesses_list().into_iter()
            .filter(|h| settings.subagent_access(*h).on).map(Harness::key).collect();
        let project = self.store.project_for_cwd(cwd).and_then(|id| self.store.project(&id)).map(|p| p.path.as_path());
        let tools = match mcp::session::SessionTools::new(record_id, cwd, project, &plan.off, &targets) {
            Ok(tools) => tools,
            Err(error) => {
                self.toast(error, cx);
                return None;
            }
        };
        self.sync_skills(Some(meta.harness), cx);
        let all_models = self.store.data.settings.aggregated_models();
        let picked = self.store.data.settings.picked_models(meta.harness).to_vec();
        let prior_entries = if session_id.is_some() { crate::harness::thread::load_entries(record_id) } else { vec![] };
        let inline_visuals = !self.store.data.settings.m4code_tools_off.contains(&mcp::Feature::Visual);
        Some(crate::thread::Start {
            record_id: record_id.to_string(),
            harness: meta.harness,
            all_models,
            picked,
            server: self.acp_server.clone(),
            cwd: cwd.to_path_buf(),
            env: plan.env,
            launch_args: plan.args,
            session_id,
            meta: plan.meta,
            mcp_servers: plan.mcp_servers,
            mcp_tools: Some(tools),
            permission,
            starter: by,
            options,
            prompt,
            prior_entries,
            inline_visuals,
        })
    }

    /// Start the session over in its folder. For a session with no conversation yet, nothing is lost. A running view
    /// of it is replaced, which ends its process.
    pub(super) fn relaunch_empty(&mut self, id: &str, permission: Option<Permission>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rec) = self.store.session(id).cloned() else { return };
        let Some(mut meta) = rec.agent.clone() else { return };
        meta.session_id.clear();
        let cwd = self.resume_cwd(&rec, cx);
        let Some(mut start) = self.thread_start(&rec.id, &cwd, &meta, rec.chosen_title().map(String::from), None, Starter::User, cx) else { return };
        if permission.is_some() {
            start.permission = permission;
        }
        if let Some(m) = self.store.session_mut(id).and_then(|r| r.agent.as_mut()) {
            m.session_id.clear();
        }
        self.start_thread(rec, start, false, window, cx);
    }

    /// `cwd`, or the project's main checkout when `cwd` is inside one of its linked worktrees.
    fn outside_linked_worktree(&self, cwd: PathBuf) -> PathBuf {
        let git = self.store.project_for_cwd(&cwd).and_then(|id| self.store.project(&id)).and_then(|p| p.git.as_ref());
        match git.and_then(|g| g.folder_of(&cwd).filter(|f| !g.is_main(f)).map(|_| g.main_checkout())) {
            Some(main) if main.is_dir() => main.to_path_buf(),
            _ => cwd,
        }
    }

    /// Resume a Claude session through `claude --resume`. Reuses the history record.
    pub fn resume(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running(id, cx) {
            self.show_session(id, window, cx);
            return;
        }
        // The session leaves the RECENT list, so the list folds away rather than staying open under it.
        self.store.data.layout.recent_open = false;
        self.relaunch(id, window, cx);
    }

    /// Open the session again in its harness, in the record's folder. A running view of it is replaced, which ends
    /// its process.
    pub(super) fn relaunch(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let prior = self.live_view(id).and_then(|v| v.thread()).map(|t| t.read(cx).thread.entries.clone()).unwrap_or_default();
        self.relaunch_with_entries(id, prior, None, window, cx);
    }

    /// Like `relaunch`, carrying over `prior_entries` directly so the visible conversation never flashes blank or
    /// disappears while the replacement process opens.
    pub(super) fn relaunch_with_entries(&mut self, id: &str, prior_entries: Vec<crate::harness::thread::Entry>, permission: Option<Permission>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rec) = self.store.session(id).cloned() else { return };
        let Some(meta) = rec.agent.clone() else { return };
        let cwd = self.resume_cwd(&rec, cx);
        let Some(mut start) = self.thread_start(&rec.id, &cwd, &meta, rec.chosen_title().map(String::from), None, Starter::User, cx) else { return };
        if !prior_entries.is_empty() {
            start.prior_entries = prior_entries;
        }
        if permission.is_some() {
            start.permission = permission;
        }
        self.start_thread(rec, start, false, window, cx);
    }

    /// Where a resumed session starts: its folder, or its project's main checkout when the folder is gone (Claude Code's
    /// exit prompt removes a worktree). Claude Code then tells the user in the session that the worktree is gone.
    pub(super) fn resume_cwd(&mut self, rec: &SessionRecord, cx: &mut Context<Self>) -> PathBuf {
        if rec.cwd.is_dir() {
            return rec.cwd.clone();
        }
        let project = rec.project_id.as_ref().and_then(|id| self.store.project(id));
        let fallback = project.map(|p| p.git.as_ref().map_or(p.path.as_path(), |g| g.main_checkout()).to_path_buf()).filter(|p| p.is_dir());
        match fallback {
            Some(main) => {
                let short = crate::panels::widgets::short_path;
                self.toast(format!("{} is gone, so the session starts in {}.", short(&rec.cwd), short(&main)), cx);
                // The record follows, so the shell, the files and the next resume start there too.
                if let Some(r) = self.store.session_mut(&rec.id) {
                    r.cwd = main.clone();
                }
                main
            }
            None => rec.cwd.clone(),
        }
    }

    /// End the session's process and resume the same conversation. The shell and cards belong to the session, so they stay.
    #[cfg(test)]
    pub(super) fn restart_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let was_shown = self.active_id().is_some_and(|a| a == id);
        let shell = self.shells.remove(id);
        self.relaunch(id, window, cx);
        if let Some(shell) = shell {
            self.shells.insert(id.into(), shell);
        }
        if was_shown {
            self.show_session(id, window, cx);
        }
    }

    /// Run a shell tab's program (a sign-in) in a terminal.
    pub(super) fn start(&mut self, record: SessionRecord, spec: SpawnSpec, is_new: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (terminal, rx) = TerminalView::spawn(&spec);
        let id = record.id.clone();
        self.prepare_live(record, is_new);
        let view = cx.new(|cx| TerminalView::new(terminal, rx, cx));
        let sub_id = id.clone();
        let sub = cx.subscribe_in(&view, window, move |ws: &mut Workspace, _view, ev: &TerminalEvent, window, cx| match ev {
            TerminalEvent::Exited(code) => ws.on_exit(&sub_id, *code, window, cx),
            TerminalEvent::Bell => {}
            TerminalEvent::OpenPath(path, line) => ws.open_from_terminal(&sub_id, path, *line, window, cx),
        });
        self.adopt_live(id, SessionView::Terminal(view), sub, None, true, window, cx);
    }

    /// Run a harness session in a thread view.
    pub(super) fn start_thread(&mut self, record: SessionRecord, start: crate::thread::Start, is_new: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.start_thread_with_focus(record, start, is_new, true, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_thread_with_focus(&mut self, mut record: SessionRecord, start: crate::thread::Start, is_new: bool, foreground: bool, window: &mut Window, cx: &mut Context<Self>) {
        let id = record.id.clone();
        let name = record.chosen_title().map(String::from);
        let subagent_edits = record.agent.as_ref().filter(|m| m.parent_session.is_some()).map(|m| m.subagent_edits);
        let canonical_history = record.agent.as_ref().is_some_and(|m| !m.provider_sessions.is_empty());
        let auto_compact = record.agent.as_ref().and_then(|m| m.auto_compact_override).unwrap_or(self.store.data.settings.auto_compact);
        let auto_compact_threshold = crate::harness::compaction::clamp_threshold(self.store.data.settings.auto_compact_threshold);
        let native_controlled = start.harness.adapter().native_compaction_managed;
        if let Some(meta) = record.agent.as_mut() {
            meta.managed_compaction = true;
        }
        self.prepare_live(record, is_new);
        let switching_harness = self.pending_handoffs.contains_key(&id);
        if let Some(meta) = self.store.session_mut(&id).and_then(|r| r.agent.as_mut()) {
            meta.managed_compaction = true;
            if !switching_harness {
                meta.read_only_pending = start.permission == Some(Permission::ReadOnly);
            }
            self.store.mark_dirty();
        }
        let view = cx.new(|cx| ThreadView::new(start, window, cx));
        if switching_harness {
            view.update(cx, |v, cx| v.hold_workspace_switch(true, cx));
        }
        if subagent_edits.is_some() {
            view.update(cx, |v, cx| v.composer.update(cx, |c, cx| c.set_placeholder("Message this subagent…", window, cx)));
        }
        let follow_up = self.store.data.settings.follow_up;
        view.update(cx, |v, _| {
            v.follow_up = follow_up;
            v.subagent_edits = subagent_edits;
            v.canonical_history |= canonical_history;
            v.compaction.enabled = auto_compact;
            v.compaction.threshold_percent = auto_compact_threshold;
            v.compaction.native_controlled = native_controlled;
        });
        let sub_id = id.clone();
        let sub = cx.subscribe_in(&view, window, move |ws: &mut Workspace, view, ev: &ThreadEvent, window, cx| {
            if ws.live_view(&sub_id).and_then(|v| v.thread()) != Some(view) {
                return;
            }
            match ev {
                ThreadEvent::Ready(session) => {
                    ws.commit_handoff(&sub_id, session, window, cx);
                    let full_access = view.read(cx).antigravity_full_access();
                    if let Some(meta) = ws.store.session_mut(&sub_id).and_then(|r| r.agent.as_mut())
                        && (meta.read_only_pending || meta.antigravity_full_access != full_access)
                    {
                        meta.read_only_pending = false;
                        meta.antigravity_full_access = full_access;
                        ws.store.mark_dirty();
                    }
                }
                ThreadEvent::Exited(code) => {
                    if ws.switching_worktree.remove(&sub_id).is_some() {
                        cx.notify();
                    }
                    if !ws.rollback_handoff(&sub_id, window, cx) {
                        ws.on_exit(&sub_id, *code, window, cx);
                    }
                }
                ThreadEvent::PermissionPreference(permission) => {
                    if ws.pending_handoffs.contains_key(&sub_id) { return; }
                    if let Some(meta) = ws.store.session_mut(&sub_id).and_then(|r| r.agent.as_mut()) {
                        meta.permission = Some(*permission);
                        ws.store.mark_dirty();
                    }
                }
                ThreadEvent::Chosen(model, effort) => {
                    if ws.pending_handoffs.contains_key(&sub_id) { return; }
                    if let Some(meta) = ws.store.session_mut(&sub_id).and_then(|r| r.agent.as_mut())
                        && (&meta.chosen_model, &meta.chosen_effort) != (model, effort)
                    {
                        meta.chosen_model = model.clone();
                        meta.chosen_effort = effort.clone();
                        ws.store.mark_dirty();
                    }
                }
                ThreadEvent::SessionId(session) => {
                    if ws.pending_handoffs.contains_key(&sub_id) { return; }
                    if let Some(meta) = ws.store.session_mut(&sub_id).and_then(|r| r.agent.as_mut()) {
                        meta.session_id = session.clone();
                        ws.store.mark_dirty();
                    }
                }
                ThreadEvent::PromptSent(text) => {
                    if !ws.pending_handoffs.contains_key(&sub_id) {
                        ws.auto_title(&sub_id, text, cx);
                    }
                }
                ThreadEvent::Title(title) => {
                    if ws.pending_handoffs.contains_key(&sub_id) { return; }
                    if let Some(rec) = ws.store.session_mut(&sub_id) {
                        rec.title = title.clone();
                        rec.auto_title = None;
                        ws.store.mark_dirty();
                        cx.notify();
                    }
                }
                ThreadEvent::Models(harness, choices) => {
                    let catalog: Vec<crate::harness::subagents::AgentModel> = choices
                        .iter()
                        .map(|(id, name)| crate::harness::subagents::AgentModel { id: id.clone(), name: name.clone() })
                        .collect();
                    if !catalog.is_empty() {
                        let lookup = ws.model_lookups.get_mut(*harness);
                        lookup.fetched = true;
                        lookup.error = None;
                        if ws.store.data.settings.set_harness_models(*harness, catalog) {
                            ws.store.mark_dirty();
                            ws.push_models_to_threads(cx);
                        }
                    }
                }
                ThreadEvent::Modes(harness, modes) => {
                    if ws.store.data.settings.set_harness_modes(*harness, modes.clone()) {
                        ws.store.mark_dirty();
                        cx.notify();
                    }
                }
                ThreadEvent::Context(used, size) => {
                    if ws.pending_handoffs.contains_key(&sub_id) { return; }
                    if let Some(meta) = ws.store.session_mut(&sub_id).and_then(|r| r.agent.as_mut()) {
                        meta.context_tokens = Some(*used);
                        if *size > 0 {
                            meta.context_window = Some(*size);
                            meta.context_fraction = Some((*used as f64 / *size as f64).min(1.0) as f32);
                        }
                        cx.notify();
                    }
                }
                ThreadEvent::Usage(turn) => {
                    if ws.pending_handoffs.contains_key(&sub_id) { return; }
                    if let Some(meta) = ws.store.session_mut(&sub_id).and_then(|r| r.agent.as_mut()) {
                        meta.cache_usage.record(*turn);
                        ws.store.mark_dirty();
                        cx.notify();
                    }
                }
                ThreadEvent::UsageLimits(harness, limits) => {
                    ws.apply_live_limits(*harness, limits.clone(), cx);
                }
                ThreadEvent::OpenGitPicker(at) => {
                    let (project, cwd) = (ws.store.session(&sub_id).and_then(|r| r.project_id.clone()), ws.store.session(&sub_id).map(|r| r.cwd.clone()));
                    if let (Some(project), Some(cwd)) = (project, cwd) {
                        ws.open_git_picker(project, cwd, crate::app::GitPickerTarget::Session, *at, window, cx);
                    }
                }
                ThreadEvent::OpenPath(path, line, end_line) => ws.open_from_terminal_range(&sub_id, path, *line, *end_line, window, cx),
                ThreadEvent::AbortChildren => ws.end_child_sessions(&sub_id, window, cx),
                ThreadEvent::Idle(outcome) => {
                    if let Some(harness) = ws.store.session(&sub_id).and_then(|r| r.agent.as_ref()).map(|m| m.harness) {
                        ws.request_limits_refresh(harness, cx);
                    }
                    ws.report_to_parent(&sub_id, outcome, cx);
                    // The row's check mark and the notification follow the turn's end, not the next poll.
                    ws.update_activity(window, cx);
                }
                ThreadEvent::Select(child) => ws.act(Act::Select(child.clone()), window, cx),
                ThreadEvent::Handoff { to, model, message, effort, by_agent } => ws.dynamic_handoff(&sub_id, *to, model.clone(), message.clone(), *by_agent, effort.clone(), window, cx),
                ThreadEvent::ReconnectPermission(permission) => ws.reconnect_antigravity_permission(&sub_id, *permission, window, cx),
                ThreadEvent::NewSession(prompt) => {
                    let (harness, cwd) = ws
                        .store
                        .session(&sub_id)
                        .map(|r| (r.agent.as_ref().map_or(ws.store.data.settings.harness, |m| m.harness), r.cwd.clone()))
                        .unwrap_or_else(|| (ws.store.data.settings.harness, ws.new_session_cwd()));
                    ws.new_session_with(harness, cwd, prompt.clone(), vec![], Starter::User, None, window, cx);
                }
                ThreadEvent::OpenToolShell(command) => {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(command.clone()));
                    if ws.shells.get(&sub_id).is_some_and(|panel| panel.open) {
                        ws.focus_shell(window, cx);
                    } else {
                        ws.toggle_shell(&sub_id, window, cx);
                    }
                    ws.toast("Command copied. Paste it in the shell to run it.", cx);
                }
                ThreadEvent::Fork(end) => ws.fork_thread(&sub_id, *end, window, cx),
                ThreadEvent::ClearSession => ws.clear_session(&sub_id, window, cx),
                ThreadEvent::Compact { transcript, instructions } => ws.compact_session(&sub_id, transcript.clone(), instructions.clone(), window, cx),
            }
        });
        self.adopt_live(id, SessionView::Thread(view), sub, name, foreground, window, cx);
    }

    fn fork_thread(&mut self, source: &str, end: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut record) = self.store.session(source).cloned() else { return };
        let Some(thread) = self.live_view(source).and_then(|view| view.thread()).cloned() else { return };
        let view = thread.read(cx);
        let Some(entries) = view.thread.entries.get(..end) else {
            self.toast("The original turn is unavailable in this transcript.", cx);
            return;
        };
        let mut history = entries.to_vec();
        history.push(crate::harness::thread::Entry::Notice { text: crate::thread::FORK_SEED.into(), error: false });
        let harness = view.harness;
        let permission = view.handoff_permission();
        let options = ["model", "thought_level"].into_iter().filter_map(|category| {
            view.thread.option(category).map(|option| (category, option.current.clone()))
        }).collect::<Vec<_>>();
        let meta = AgentMeta { harness, chrome: self.store.data.settings.chrome_default, ..Default::default() };
        record.id = new_id();
        record.title = format!("Fork of {}", record.display_title());
        record.auto_title = None;
        record.custom_title = None;
        record.created_at = now();
        record.last_active = now();
        record.exit_code = None;
        record.hidden = false;
        record.agent = Some(meta.clone());
        let Some(mut start) = self.thread_start(&record.id, &record.cwd, &meta, Some(record.title.clone()), None, Starter::User, cx) else { return };
        start.permission = permission;
        start.options.retain(|(category, _)| options.iter().all(|(kept, _)| kept != category));
        start.options.extend(options);
        crate::harness::thread::save_entries(&record.id, &history);
        start.prior_entries = history;
        self.start_thread(record, start, true, window, cx);
    }

    /// Before a session's new view exists: drop the view it replaces and mark the record as running.
    fn prepare_live(&mut self, mut record: SessionRecord, is_new: bool) {
        let id = record.id.clone();
        record.exit_code = None;
        record.last_active = now();
        // Dropping a running view ends its process.
        self.live.retain(|l| l.id != id);
        self.purge_dead_shells();
        if is_new {
            self.store.add_session(record);
        } else if let Some(r) = self.store.session_mut(&id) {
            r.exit_code = None;
            r.last_active = now();
        }
    }

    /// Keep the new view among the live sessions and show it.
    #[allow(clippy::too_many_arguments)]
    fn adopt_live(&mut self, id: Id, view: SessionView, sub: Subscription, claude_name: Option<String>, foreground: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The mod reads the launch name first and leaves it alone; a stale file would otherwise rename the session.
        if let Some(cid) = self.store.session(&id).and_then(|r| r.provider_session_id()) {
            claude::write_name(cid, claude_name.as_deref());
        }
        self.live.push(Live { id: id.clone(), view, _sub: sub, claude_name });
        self.remember(&id);
        if !foreground {
            cx.notify();
            return;
        }
        self.activate(&id, cx);
        self.selected = Some(id.clone());
        self.main = Main::Terminals;
        self.sync_github(window, cx);
        self.persist_layout();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Make `id` the session shown.
    pub(super) fn activate(&mut self, id: &Id, cx: &App) {
        self.active = Some(id.clone());
        self.prune_live(cx);
    }

    /// Exited terminals are only kept while shown; a shell panel goes with its session, except the welcome screen's.
    fn prune_live(&mut self, cx: &App) {
        let active = &self.active;
        self.live.retain(|l| active.as_ref() == Some(&l.id) || l.view.is_running(cx));
        let live = &self.live;
        self.shells.retain(|id, _| id == HOME_SHELL || live.iter().any(|l| &l.id == id));
    }

    /// A Claude session that never produced a transcript (opened and closed without a prompt) has nothing
    /// to resume. Remove m4code's own record of it; Claude Code's data is never touched.
    fn drop_if_empty(&mut self, id: &str) {
        let ephemeral = self.ephemeral.remove(id);
        if crate::thread::has_saved_queue(id) {
            return;
        }
        if ephemeral {
            self.store.data.sessions.retain(|s| s.id != id);
            if self.selected.as_deref() == Some(id) {
                self.selected = None;
            }
            self.store.mark_dirty();
            return;
        }
        let Some(rec) = self.store.session(id) else {
            return;
        };
        let Some(meta) = rec.agent.as_ref() else {
            return;
        };
        if !has_conversation(id, meta) {
            self.store.data.sessions.retain(|s| s.id != id);
            if self.selected.as_deref() == Some(id) {
                self.selected = None;
            }
            self.store.mark_dirty();
        }
    }

    pub(super) fn remember(&mut self, id: &Id) {
        self.recent.retain(|r| r != id);
        self.recent.push(id.clone());
    }

    /// The process ended by itself. Close the session the way the X does, so the terminal does not stay up behind a
    /// "Session ended" bar. A non-zero exit is a toast, because that output goes away with the view; a clean exit
    /// says nothing. This does not offer to remove a worktree: `/exit` already asked inside Claude Code, and the X
    /// (`close_live`) is the path that kills the process before that prompt.
    fn on_exit(&mut self, id: &str, code: Option<i32>, window: &mut Window, cx: &mut Context<Self>) {
        let start_error = self.live_view(id).and_then(|v| v.start_error(cx));
        let title = self.store.session(id).map(|r| r.display_title().to_string()).unwrap_or_else(|| "Session".into());
        if let Some(rec) = self.store.session_mut(id) {
            rec.exit_code = Some(code.unwrap_or(0));
        }
        self.end_live(id, window, cx);
        if let Some(err) = start_error {
            self.toast(format!("Could not start {title}: {err}"), cx);
        } else if let Some(code) = code.filter(|c| *c != 0) {
            self.toast(format!("{title} ended (exit code {code})"), cx);
        }
    }

    /// Clicking a session: running -> show its terminal; past -> open it right away (no intermediate page).
    pub fn show_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.selected = Some(id.clone());
        if self.live_view(&id).is_some() {
            self.activate(&id, cx);
            self.remember(&id);
            self.main = Main::Terminals;
            self.sync_github(window, cx);
            self.focus_active(window, cx);
            self.persist_layout();
            cx.notify();
        } else {
            self.resume(&id, window, cx);
        }
    }

    /// Give the keyboard to the shown session's terminal, or on the home page to its shell when one is open.
    pub fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let (Main::Terminals, Some(id)) = (&self.main, self.active.as_ref())
            && let Some(view) = self.live_view(id)
        {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        if matches!(self.main, Main::Home) {
            match self.shells.get(HOME_SHELL).filter(|p| p.open) {
                Some(shell) => window.focus(&shell.view.focus_handle(cx), cx),
                None => self.home.prompt.update(cx, |p, cx| p.focus(window, cx)),
            }
            return;
        }
        window.focus(&self.root_focus, cx);
    }

    /// Bring a Claude session back after a restart: resume its conversation, or start it over when it has none yet.
    pub(super) fn bring_back(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.store.session(id).and_then(|s| s.agent.as_ref()) {
            Some(meta) if !has_conversation(id, meta) => self.relaunch_empty(id, None, window, cx),
            _ => self.resume(id, window, cx),
        }
    }

    /// Resume the last closed session that can come back: one still in the history, with a Claude conversation, and
    /// not running again. Whether there was one.
    pub fn reopen_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        while let Some(id) = self.closed.pop() {
            if !self.is_running(&id, cx) && self.store.session(&id).is_some_and(|r| r.agent.is_some()) {
                self.resume(&id, window, cx);
                return true;
            }
        }
        false
    }

    /// End a session. One that ran in a Claude Code worktree no other session uses then gets the offer to remove it,
    /// since Claude Code's own prompt only appears when a session exits with `/exit`.
    pub(super) fn close_live(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let was_in = self.store.session(id).map(|s| (s.cwd.clone(), s.project_id.clone()));
        self.closed.retain(|c| c != id);
        self.closed.push(id.to_string());
        self.end_live(id, window, cx);
        if let Some((cwd, project)) = was_in {
            self.offer_worktree_removal(&cwd, project.as_deref(), cx);
        }
    }

    fn end_child_sessions(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let children: Vec<_> = self.live.iter().filter(|live| self.store.session(&live.id).and_then(|r| r.agent.as_ref()).is_some_and(|m| m.parent_session.as_deref() == Some(id))).map(|live| live.id.clone()).collect();
        for child in children {
            self.end_live(&child, window, cx);
        }
    }

    pub(super) fn end_live(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.switching_worktree.remove(id);
        self.cancel_pending_handoff(id, cx);
        self.end_child_sessions(id, window, cx);
        let was_active = self.active_id().is_some_and(|a| a == id);
        if let Some(thread) = self.live_view(id).and_then(|view| view.thread()) {
            thread.update(cx, |view, _| view.stop());
        }
        self.live.retain(|l| l.id != id); // dropping the view drops the Terminal, which closes the PTY
        self.shells.remove(id);
        self.panes.remove(id);
        self.recent.retain(|r| r != id);
        // Closing the shown session goes back to the one shown before it; otherwise stay where we are.
        if was_active {
            self.active = self.recent.iter().rev().find(|r| self.live.iter().any(|l| &l.id == *r)).cloned();
            self.selected = self.active.clone();
            self.persist_layout();
        }
        if let Some(rec) = self.store.session_mut(id) {
            if rec.exit_code.is_none() {
                rec.exit_code = Some(-1);
            }
            rec.last_active = now();
        }
        self.drop_if_empty(id);
        self.purge_dead_shells();
        if self.active.is_none() {
            self.main = Main::Home;
        }
        self.store.save_if_dirty();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The running sessions, in the sidebar's order.
    pub fn running_ids(&self, cx: &App) -> Vec<Id> {
        let listed = |id: &str| !self.store.session(id).is_some_and(|rec| rec.is_subagent());
        self.live.iter().filter(|l| l.view.is_running(cx) && listed(&l.id)).map(|l| l.id.clone()).collect()
    }

    /// Show running session number `n` (from 1), as the sidebar numbers them while Ctrl is held.
    pub fn show_numbered(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = numbered(&self.running_ids(cx), n).cloned() {
            self.show_session(&id, window, cx);
        }
    }

    pub fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let running = self.running_ids(cx);
        if running.is_empty() {
            return;
        }
        let cur = self
            .active_id()
            .and_then(|a| running.iter().position(|r| r == a))
            .unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(running.len() as isize) as usize;
        self.show_session(&running[next].clone(), window, cx);
    }

    /// Prompt-cache timer for the selected session: `(remaining fraction, remaining secs, ttl secs)`.
    /// Context-meter source for the selected session across all harnesses: `(fraction, tokens, exact, full at)`.
    /// Reads live ACP `usage_update` context first (works for Claude Code, Codex, OpenCode, and Antigravity), then
    /// falls back to saved session metadata / transcript estimates.
    pub fn selected_context(&self, cx: &gpui::App) -> Option<(f32, u64, bool, Option<u64>)> {
        let id = self.selected.as_ref().or(self.active_id())?;
        if let Some(live) = self.live.iter().find(|l| &l.id == id)
            && let SessionView::Thread(t) = &live.view
            && let Some((used, size)) = t.read(cx).thread.context
            && size > 0
        {
            let frac = (used as f64 / size as f64).min(1.0) as f32;
            return Some((frac, used, true, Some(size)));
        }
        let scaled = |tokens: u64, limit: Option<u64>| limit.map(|l| (tokens as f64 / l as f64).min(1.0) as f32);
        let meta = self.store.session(id)?.agent.as_ref()?;
        let limit = claude::context_limit(meta.context_window, meta.compact_at, meta.auto_compact);
        if let Some(f) = meta.context_fraction {
            let tokens = meta.context_tokens.unwrap_or_else(|| meta.context_window.map_or(0, |w| (w as f64 * f as f64) as u64));
            return Some((scaled(tokens, limit).unwrap_or(f), tokens, true, limit));
        }
        let tokens = meta.context_tokens?;
        Some((claude::context_fraction(tokens), tokens, false, None))
    }
}

/// The options a resumed session asks for: the model and effort last picked in it.
fn resume_options(meta: &AgentMeta) -> Vec<(&'static str, String)> {
    [("model", &meta.chosen_model), ("thought_level", &meta.chosen_effort)]
        .into_iter()
        .filter_map(|(category, value)| Some((category, value.clone()?)))
        .collect()
}

/// Whether Claude Code has a conversation for the session: one it listed on a scan, or a transcript on disk. A
/// session nobody has written to has none, and `claude --resume` ends at once on it.
fn has_conversation(record_id: &str, meta: &AgentMeta) -> bool {
    crate::harness::thread::load_entries(record_id).iter().any(|entry| matches!(entry,
        crate::harness::thread::Entry::User(_) | crate::harness::thread::Entry::Agent(_) | crate::harness::thread::Entry::Tool(_)))
        || meta.external != ExternalState::Unknown
        || meta.transcript.is_some()
        || claude::claude_home().and_then(|h| claude::find_transcript(&h, &meta.session_id)).is_some()
}

#[cfg(test)]
mod tests {
    use super::{numbered, resume_options};
    use crate::store::AgentMeta;

    #[test]
    fn a_resumed_session_asks_for_the_model_and_effort_picked_in_it() {
        let picked = AgentMeta { chosen_model: Some("opus".into()), chosen_effort: Some("high".into()), ..Default::default() };
        assert_eq!(resume_options(&picked), [("model", "opus".to_string()), ("thought_level", "high".to_string())]);
        let model_only = AgentMeta { chosen_model: Some("gpt-5.3-codex".into()), ..Default::default() };
        assert_eq!(resume_options(&model_only), [("model", "gpt-5.3-codex".to_string())]);
        assert!(resume_options(&AgentMeta::default()).is_empty(), "nothing picked asks for nothing");
    }

    #[test]
    fn numbers_count_from_one() {
        let running = ["a", "b", "c"];
        assert_eq!(numbered(&running, 1), Some(&"a"));
        assert_eq!(numbered(&running, 3), Some(&"c"));
        assert_eq!(numbered(&running, 4), None, "no fourth session");
        assert_eq!(numbered(&running, 0), None);
    }
}
