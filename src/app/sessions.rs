//! Sessions: starting, resuming, showing, closing and switching them, and the queries the rest of the app asks about them.

use super::*;

/// Item `n` (from 1) of `items`.
fn numbered<T>(items: &[T], n: usize) -> Option<&T> {
    items.get(n.checked_sub(1)?)
}

/// How long Ctrl (Cmd) is held alone before the sidebar shows the session shortcuts.
const SHORTCUT_HOLD: Duration = Duration::from_millis(400);

impl Workspace {
    /// Per-process additions to a Claude launch; nothing in the user's Claude Code config changes:
    /// the mod (`--plugin-dir`, which reports usage and status), the tab-title MCP server (`--mcp-config`, its
    /// tools pre-allowed), and IDE integration switched off through environment variables. `chrome` adds Claude in
    /// Chrome (`--chrome`).
    fn with_helpers(
        &mut self,
        mut spec: SpawnSpec,
        claude_session_id: &str,
        record_id: &str,
        chrome: bool,
        cx: &mut Context<Self>,
    ) -> SpawnSpec {
        let started = std::time::Instant::now();
        // Diagnostics for the start-up trace (`terminal/trace.rs`): start without Claudiu's MCP server or mod to see what they cost.
        let without = |name: &str| crate::terminal::trace::on() && std::env::var_os(name).is_some();
        let settings = &self.store.data.settings;
        let off = mcp::tools_off(&settings.claudiu_tools_off, &settings.trimmed_tools);
        if let Ok(config) = mcp::prepare(claude_session_id, record_id, &off).and_then(|c| if without("CLAUDIU_NO_MCP") { Err(std::io::Error::other("off")) } else { Ok(c) }) {
            spec.args.push("--allowedTools".into());
            spec.args.extend(mcp::allowed_tools(&off));
            spec.args.extend(["--mcp-config".into(), config.to_string_lossy().into_owned()]);
        }
        if let Ok(dir) = claude::plugin::install().and_then(|d| if without("CLAUDIU_NO_MOD") { Err(std::io::Error::other("off")) } else { Ok(d) }) {
            spec.args.extend(["--plugin-dir".into(), dir.to_string_lossy().into_owned()]);
            spec.env.push(("CLAUDIU_NAME_FILE".into(), mcp::name_file(claude_session_id).to_string_lossy().into_owned()));
            spec.env.push((usage::USAGE_DIR_ENV.into(), usage::snapshot_dir().to_string_lossy().into_owned()));
            spec.env.push((usage::CTL_ENV.into(), usage::ctl_file(record_id).to_string_lossy().into_owned()));
            spec.env.push((crate::visual::VISUAL_ENV.into(), crate::visual::file(record_id).to_string_lossy().into_owned()));
            spec.env.push((crate::claude::ask::ASK_ENV.into(), crate::claude::ask::request_file(record_id).to_string_lossy().into_owned()));
        }
        if chrome {
            spec.args.push("--chrome".into());
        }
        spec.env.push(("CLAUDE_CODE_NO_FLICKER".into(), "1".into())); // fullscreen renderer, per process
        spec.env.push(("CLAUDE_CODE_AUTO_CONNECT_IDE".into(), "false".into()));
        spec.env.push(("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL".into(), "1".into()));
        self.sync_configs_now(cx);
        if let Some(acct) = self.store.session_account(record_id) {
            spec.env.push((usage::ACCOUNT_ENV.into(), acct.id.clone()));
            spec.env.extend(Self::account_env(acct));
        }
        // Extra `claude` flags for the start-up trace, e.g. CLAUDIU_TRACE_ARGS="--safe-mode".
        if crate::terminal::trace::on() && let Some(extra) = std::env::var_os("CLAUDIU_TRACE_ARGS") {
            spec.args.extend(extra.to_string_lossy().split_whitespace().map(String::from));
        }
        (spec.program, spec.args) = platform::through_login_shell(spec.program, spec.args);
        if let Some(cwd) = spec.cwd.as_ref().and_then(|c| c.file_name()) {
            crate::terminal::trace::note(&cwd.to_string_lossy(), started.elapsed(), "launch files and arguments prepared (before the terminal exists)");
        }
        spec
    }

    pub fn is_running(&self, id: &str, cx: &App) -> bool {
        self.live
            .iter()
            .any(|l| l.id == id && l.view.read(cx).is_running())
    }

    pub fn running_count(&self, cx: &App) -> usize {
        self.live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
            .count()
    }

    pub fn live_view(&self, id: &str) -> Option<&Entity<TerminalView>> {
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
        self.active_id()
            .and_then(|id| self.store.session(id).map(|s| s.cwd.clone()))
            .or_else(|| self.store.data.projects.first().map(|p| p.path.clone()))
            .or_else(platform::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn new_claude(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.new_claude_with(cwd, vec![], Starter::User, window, cx);
    }

    /// `leading` goes before every other argument: the initial prompt must come ahead of the variadic
    /// `--allowedTools` / `--mcp-config` flags, or they would swallow it.
    pub(super) fn new_claude_with(&mut self, cwd: PathBuf, mut leading: Vec<String>, by: Starter, window: &mut Window, cx: &mut Context<Self>) {
        let Some(exe) = self.claude_exe.clone() else {
            self.toast("Could not find the `claude` CLI. Install Claude Code, or set its path in Settings.", true, cx);
            return;
        };
        let session_id = new_id();
        let record_id = new_id();
        let mut spec = claude::new_session_spec(&exe, &cwd, &session_id);
        let settings = &self.store.data.settings;
        let defaults = claude::defaults::flags(&settings.new_session, settings.trimmed_tools.contains(&claude::trim::Trim::PlanMode), &leading, by);
        leading.extend(defaults);
        spec.args.splice(0..0, leading);
        let chrome = self.store.data.settings.chrome_default;
        let spec = self.with_helpers(spec, &session_id, &record_id, chrome, cx);
        let record = SessionRecord {
            id: record_id,
            kind: SessionKind::Claude,
            title: "New Claude session".into(),
            custom_title: None,
            auto_title: None,
            project_id: self.store.project_for_cwd(&cwd),
            cwd,
            created_at: now(),
            last_active: now(),
            exit_code: None,
            hidden: false,
            claude: Some(ClaudeMeta {
                session_id,
                chrome,
                ..Default::default()
            }),
        };
        self.start(record, spec, true, window, cx);
    }

    /// Resume a Claude session through `claude --resume`. Reuses the history record.
    pub fn resume(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running(id, cx) {
            self.show_session(id, window, cx);
            return;
        }
        let Some(rec) = self.store.session(id).cloned() else {
            return;
        };
        let Some(meta) = rec.claude.clone() else {
            return;
        };
        let Some(exe) = self.claude_exe.clone() else {
            self.toast("Could not find the `claude` CLI.", true, cx);
            return;
        };
        let mut spec = claude::resume_spec(&exe, &rec.cwd, &meta.session_id);
        // Hand Claudiu's tab title to Claude Code so its own name (prompt box, /resume, Remote Control) matches.
        if let Some(name) = rec.chosen_title() {
            spec.args.extend(["--name".into(), name.into()]);
        }
        let spec = self.with_helpers(spec, &meta.session_id, &rec.id, meta.chrome, cx);
        self.start(rec, spec, false, window, cx);
    }

    /// End the session's process and resume the same conversation. The shell and cards belong to the session, so they stay.
    pub(super) fn restart_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let was_shown = self.active_id().is_some_and(|a| a == id);
        let shell = self.shells.remove(id);
        self.live.retain(|l| l.id != id); // dropping the view closes the old process
        self.resume(id, window, cx);
        if let Some(shell) = shell {
            self.shells.insert(id.into(), shell);
        }
        if was_shown {
            self.show_session(id, window, cx);
        }
    }

    pub(super) fn start(
        &mut self,
        mut record: SessionRecord,
        spec: SpawnSpec,
        is_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (terminal, rx) = TerminalView::spawn(&spec);
        let id = record.id.clone();
        let loading = record.kind == SessionKind::Claude;
        record.exit_code = None;
        record.last_active = now();
        // Re-resuming: drop the previous (exited) terminal view for this record.
        self.live.retain(|l| l.id != id);
        self.purge_dead_shells();
        self.transcript_mtimes.remove(&id);
        if is_new {
            self.store.add_session(record);
        } else if let Some(r) = self.store.session_mut(&id) {
            r.exit_code = None;
            r.last_active = now();
        }
        let view = cx.new(|cx| {
            let mut v = TerminalView::new(terminal, rx, cx);
            v.loading = loading;
            v
        });
        let sub_id = id.clone();
        let sub = cx.subscribe_in(
            &view,
            window,
            move |ws: &mut Workspace, _view, ev: &TerminalEvent, window, cx| match ev {
                TerminalEvent::Exited(code) => ws.on_exit(&sub_id, *code, window, cx),
                TerminalEvent::Bell => {}
                TerminalEvent::OpenPath(path, line) => ws.open_from_terminal(&sub_id, path, *line, window, cx),
            },
        );
        let claude_name = spec.args.iter().position(|a| a == "--name").and_then(|i| spec.args.get(i + 1)).cloned();
        // The mod reads the launch name first and leaves it alone; a stale file would otherwise rename the session.
        if let Some(cid) = self.store.session(&id).and_then(|r| r.claude_id()) {
            mcp::write_name(cid, claude_name.as_deref());
        }
        // The account `with_helpers` chose: kept, so the session starts there again after closing or a restart.
        let account = self.store.session_account(&id).map_or_else(|| MAIN_ACCOUNT.into(), |a| a.id.clone());
        if let Some(meta) = self.store.session_mut(&id).and_then(|r| r.claude.as_mut()) {
            meta.account = Some(account.clone());
        }
        let _focus = self.watch_terminal_focus(&id, &view, window, cx);
        self.live.push(Live {
            id: id.clone(),
            view,
            _sub: sub,
            _focus,
            claude_name,
            title_stamp: None,
            account,
        });
        self.activate(&id, cx);
        self.remember(&id);
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

    /// Exited terminals are only kept while shown; a shell panel goes with its session.
    fn prune_live(&mut self, cx: &App) {
        let active = &self.active;
        self.live.retain(|l| active.as_ref() == Some(&l.id) || l.view.read(cx).is_running());
        let live = &self.live;
        self.shells.retain(|id, _| live.iter().any(|l| &l.id == id));
    }

    /// A Claude session that never produced a transcript (opened and closed without a prompt) has nothing
    /// to resume. Remove Claudiu's own record of it; Claude Code's data is never touched.
    fn drop_if_empty(&mut self, id: &str) {
        if self.ephemeral.remove(id) {
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
        let Some(meta) = rec.claude.as_ref() else {
            return;
        };
        if meta.external != ExternalState::Unknown || meta.transcript.is_some() {
            return;
        }
        let has_transcript = claude::claude_home()
            .and_then(|h| claude::find_transcript(&h, &meta.session_id))
            .is_some();
        if !has_transcript {
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

    fn on_exit(&mut self, id: &str, code: Option<i32>, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(err) = self.live_view(id).and_then(|v| v.read(cx).start_error()) {
            let program = self.store.session(id).map(|r| r.display_title().to_string()).unwrap_or_default();
            self.toast(format!("Could not start {program}: {err}"), true, cx);
        }
        if self.logins.remove(id) {
            self.refresh_accounts(cx);
        }
        if let Some(rec) = self.store.session_mut(id) {
            rec.exit_code = Some(code.unwrap_or(0));
            rec.last_active = now();
        }
        self.drop_if_empty(id);
        self.store.save_if_dirty();
        cx.notify();
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

    pub fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let (Main::Terminals, Some(id)) = (&self.main, self.active.as_ref())
            && let Some(view) = self.live_view(id)
        {
            window.focus(&view.read(cx).focus_handle(cx), cx);
            return;
        }
        window.focus(&self.root_focus, cx);
    }

    pub(super) fn close_live(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.logins.remove(id) {
            self.refresh_accounts(cx);
        }
        let was_active = self.active_id().is_some_and(|a| a == id);
        self.live.retain(|l| l.id != id); // dropping the view drops the Terminal, which closes the PTY
        self.shells.remove(id);
        self.panes.remove(id);
        self.visuals.remove(id);
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
        self.live.iter().filter(|l| l.view.read(cx).is_running()).map(|l| l.id.clone()).collect()
    }

    /// Show running session number `n` (from 1), as the sidebar numbers them while Ctrl is held.
    pub fn show_numbered(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = numbered(&self.running_ids(cx), n).cloned() {
            self.show_session(&id, window, cx);
        }
    }

    /// Ctrl (Cmd) alone, held for a moment, shows the sessions' numbers; anything else hides them. Quick chords such as
    /// Ctrl+C never show them, since the key is let go before the wait ends.
    pub fn modifiers_changed(&mut self, modifiers: &gpui::Modifiers, cx: &mut Context<Self>) {
        self.shortcuts_wait += 1;
        if self.shortcuts_shown {
            self.shortcuts_shown = false;
            cx.notify();
        }
        let alone = modifiers.secondary() && !modifiers.shift && !modifiers.alt && !(modifiers.control && modifiers.platform);
        if !alone {
            return;
        }
        let wait = self.shortcuts_wait;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SHORTCUT_HOLD).await;
            let _ = this.update(cx, |ws, cx| {
                if ws.shortcuts_wait == wait {
                    ws.shortcuts_shown = true;
                    cx.notify();
                }
            });
        })
        .detach();
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
    /// Every request that reads or writes the cache restarts its TTL; that moment is the timestamp of the last
    /// assistant response that used it (not the transcript's mtime, which user messages and renames also bump).
    /// Unknown TTL assumes the 5-minute tier.
    pub fn selected_cache(&self) -> Option<(f32, i64, i64)> {
        let id = self.selected.as_ref().or(self.active_id())?;
        let meta = self.store.session(id)?.claude.as_ref()?;
        let last = meta.cache_at?;
        let ttl = meta.cache_ttl.unwrap_or(300) as i64;
        let left = (last + ttl - now()).clamp(0, ttl);
        Some((left as f32 / ttl as f32, left, ttl))
    }

    /// Context-meter source for the selected session: `(fraction, tokens, exact, full at)`. `exact` means Claude Code
    /// itself reported the figures; otherwise they are estimated from the transcript. `full at` is the token count
    /// the bar ends at (`claude::context_limit`), when known.
    pub fn selected_context(&self) -> Option<(f32, u64, bool, Option<u64>)> {
        let id = self.selected.as_ref().or(self.active_id())?;
        let scaled = |tokens: u64, limit: Option<u64>| limit.map(|l| (tokens as f64 / l as f64).min(1.0) as f32);
        let meta = self.store.session(id)?.claude.as_ref()?;
        let limit = claude::context_limit(meta.context_window, meta.compact_at, meta.auto_compact);
        if let Some(f) = meta.context_fraction {
            let tokens = meta.context_tokens.unwrap_or_else(|| meta.context_window.map_or(0, |w| (w as f64 * f as f64) as u64));
            return Some((scaled(tokens, limit).unwrap_or(f), tokens, true, limit));
        }
        let tokens = meta.context_tokens?;
        Some((claude::context_fraction(tokens), tokens, false, None))
    }
}

#[cfg(test)]
mod tests {
    use super::numbered;

    #[test]
    fn numbers_count_from_one() {
        let running = ["a", "b", "c"];
        assert_eq!(numbered(&running, 1), Some(&"a"));
        assert_eq!(numbered(&running, 3), Some(&"c"));
        assert_eq!(numbered(&running, 4), None, "no fourth session");
        assert_eq!(numbered(&running, 0), None);
    }
}
