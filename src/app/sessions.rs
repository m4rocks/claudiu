//! Sessions: starting, resuming, showing, closing and switching them, and the queries the rest of the app asks about them.

use super::*;

/// The transcript a resumed session with auto effort continues; the mod reads its last reply.
const RESUMED_TRANSCRIPT_ENV: &str = "CLAUDIU_RESUMED_TRANSCRIPT";

/// A level the user picked by hand in a session with auto effort; the restarted mod keeps it instead of judging again.
const HAND_EFFORT_ENV: &str = "CLAUDIU_HAND_EFFORT";

fn hand_effort_env(meta: &ClaudeMeta) -> Option<(String, String)> {
    let effort = meta.effort.clone().filter(|_| meta.auto_state == usage::AutoState::Manual)?;
    Some((HAND_EFFORT_ENV.into(), effort))
}

/// The value of `flag` in a launch's arguments, as `--flag value`.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// Item `n` (from 1) of `items`.
fn numbered<T>(items: &[T], n: usize) -> Option<&T> {
    items.get(n.checked_sub(1)?)
}

impl Workspace {
    /// Per-process additions to a Claude launch; nothing in the user's Claude Code config changes:
    /// the mod (`--plugin-dir`, which reports usage and status), the tab-title MCP server (`--mcp-config`, its
    /// tools pre-allowed), and IDE integration switched off through environment variables. `chrome` adds Claude in
    /// Chrome (`--chrome`). `auto` hands the mod the auto effort range; `resumed_from` is the transcript a resumed
    /// session continues, whose last reply the mod reads for its first judged prompt.
    fn with_helpers(
        &mut self,
        mut spec: SpawnSpec,
        claude_session_id: &str,
        record_id: &str,
        chrome: bool,
        auto: bool,
        resumed_from: Option<&std::path::Path>,
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
        let mut has_mod = false;
        if let Ok(dir) = claude::plugin::install().and_then(|d| if without("CLAUDIU_NO_MOD") { Err(std::io::Error::other("off")) } else { Ok(d) }) {
            has_mod = true;
            spec.args.extend(["--plugin-dir".into(), dir.to_string_lossy().into_owned()]);
            spec.env.push(("CLAUDIU_NAME_FILE".into(), mcp::name_file(claude_session_id).to_string_lossy().into_owned()));
            spec.env.push((usage::USAGE_DIR_ENV.into(), usage::snapshot_dir().to_string_lossy().into_owned()));
            spec.env.push((usage::CTL_ENV.into(), usage::ctl_file(record_id).to_string_lossy().into_owned()));
            spec.env.push((crate::visual::VISUAL_ENV.into(), crate::visual::prefix(record_id).to_string_lossy().into_owned()));
            spec.env.push((crate::claude::ask::ASK_ENV.into(), crate::claude::ask::request_file(record_id).to_string_lossy().into_owned()));
        }
        if chrome {
            spec.args.push("--chrome".into());
        }
        spec.env.push(("CLAUDE_CODE_NO_FLICKER".into(), "1".into())); // fullscreen renderer, per process
        spec.env.push(("FORCE_HYPERLINK".into(), "1".into())); // Claude Code would print links as text otherwise; cards are found by theirs
        spec.env.push(("CLAUDE_CODE_AUTO_CONNECT_IDE".into(), "false".into()));
        spec.env.push(("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL".into(), "1".into()));
        self.sync_configs_now();
        if let Some(acct) = self.store.session_account(record_id) {
            spec.env.push((usage::ACCOUNT_ENV.into(), acct.id.clone()));
            spec.env.extend(Self::account_env(acct));
        }
        // Auto effort lives in the mod, so a session started without it gets none of this.
        if auto && has_mod {
            let capabilities = claude::defaults::CAPABILITIES_ENV;
            let set_already = std::env::var_os(capabilities).is_some() || spec.env.iter().any(|(k, _)| k == capabilities);
            spec.env.extend(claude::defaults::auto_env(self.store.data.settings.new_session.auto_effort, set_already));
            if let Some(path) = resumed_from {
                spec.env.push((RESUMED_TRANSCRIPT_ENV.into(), path.to_string_lossy().into_owned()));
            }
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

    pub fn is_running(&self, id: &str) -> bool {
        self.live
            .iter()
            .any(|l| l.id == id && l.view.is_running())
    }

    pub fn running_count(&self) -> usize {
        self.live
            .iter()
            .filter(|l| l.view.is_running())
            .count()
    }

    pub fn live_view(&self, id: &str) -> Option<&TerminalView> {
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

    pub fn new_claude(&mut self, cwd: PathBuf) {
        self.new_claude_with(cwd, vec![], Starter::User);
    }

    /// `leading` goes before every other argument: the initial prompt must come ahead of the variadic
    /// `--allowedTools` / `--mcp-config` flags, or they would swallow it.
    pub(super) fn new_claude_with(&mut self, cwd: PathBuf, leading: Vec<String>, by: Starter) {
        // A new session starts in the main checkout, not in a linked worktree. Fix-with-Claude works where the failure was.
        let cwd = if by == Starter::FixGit { cwd } else { self.outside_linked_worktree(cwd) };
        let session_id = new_id();
        let record_id = new_id();
        let chrome = self.store.data.settings.chrome_default;
        let auto = claude::defaults::auto_effort_for(&self.store.data.settings.new_session, by);
        let Some(spec) = self.fresh_spec(&cwd, &session_id, &record_id, chrome, auto, leading, by) else { return };
        let effort = flag_value(&spec.args, "--effort").map(String::from);
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
                auto_effort: Some(auto),
                effort,
                auto_state: if auto { usage::AutoState::On } else { usage::AutoState::Off },
                ..Default::default()
            }),
        };
        self.start_session(record, spec, true);
    }

    /// Whether a session runs with auto effort: what it was launched with, or for one from before the setting, the
    /// setting as it is now.
    fn runs_auto_effort(&self, meta: &ClaudeMeta) -> bool {
        meta.auto_effort.unwrap_or(self.store.data.settings.new_session.auto_effort.on)
    }

    /// `claude --session-id` in `cwd` with the new-session defaults. None, with a toast, when `claude` isn't installed.
    #[allow(clippy::too_many_arguments)]
    fn fresh_spec(&mut self, cwd: &std::path::Path, session_id: &str, record_id: &str, chrome: bool, auto: bool, mut leading: Vec<String>, by: Starter) -> Option<SpawnSpec> {
        let Some(exe) = self.claude_exe.clone() else {
            self.show_toast("Could not find the `claude` CLI. Install Claude Code, or set its path in Settings.".into(), true, Some(("Check setup".into(), Act::OpenSetup)));
            return None;
        };
        let mut spec = claude::new_session_spec(&exe, cwd, session_id);
        let settings = &self.store.data.settings;
        let defaults = claude::defaults::NewSessionDefaults { effort: claude::defaults::launch_effort(&settings.new_session, auto), ..settings.new_session.clone() };
        let defaults = claude::defaults::flags(&defaults, settings.trimmed_tools.contains(&claude::trim::Trim::PlanMode), &leading, by);
        leading.extend(defaults);
        spec.args.splice(0..0, leading);
        Some(self.with_helpers(spec, session_id, record_id, chrome, auto, None))
    }

    /// Start the session over under its own id in its folder. For a session with no conversation yet: `--resume` has
    /// nothing to find, and nothing is lost. A running view of it is replaced, which ends its process.
    pub(super) fn relaunch_empty(&mut self, id: &str) {
        let Some(rec) = self.store.session(id).cloned() else { return };
        let Some(meta) = rec.claude.clone() else { return };
        let cwd = self.resume_cwd(&rec);
        let auto = self.runs_auto_effort(&meta);
        let Some(mut spec) = self.fresh_spec(&cwd, &meta.session_id, &rec.id, meta.chrome, auto, vec![], Starter::User) else { return };
        spec.env.extend(hand_effort_env(&meta));
        if let Some(name) = rec.chosen_title() {
            spec.args.extend(["--name".into(), name.into()]);
        }
        self.start_session(rec, spec, false);
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
    pub fn resume(&mut self, id: &str) {
        if self.is_running(id) {
            self.show_session(id);
            return;
        }
        self.relaunch(id);
    }

    /// `claude --resume` in the record's folder. A running view of the session is replaced, which ends its process.
    pub(super) fn relaunch(&mut self, id: &str) {
        let Some(rec) = self.store.session(id).cloned() else {
            return;
        };
        let Some(meta) = rec.claude.clone() else {
            return;
        };
        let Some(exe) = self.claude_exe.clone() else {
            self.toast("Could not find the `claude` CLI.", true);
            return;
        };
        let cwd = self.resume_cwd(&rec);
        let mut spec = claude::resume_spec(&exe, &cwd, &meta.session_id);
        // Hand Claudiu's tab title to Claude Code so its own name (prompt box, /resume, Remote Control) matches.
        if let Some(name) = rec.chosen_title() {
            spec.args.extend(["--name".into(), name.into()]);
        }
        let auto = self.runs_auto_effort(&meta);
        let mut spec = self.with_helpers(spec, &meta.session_id, &rec.id, meta.chrome, auto, meta.transcript.as_deref());
        spec.env.extend(hand_effort_env(&meta));
        self.start_session(rec, spec, false);
    }

    /// Where a resumed session starts: its folder, or its project's main checkout when the folder is gone (Claude Code's
    /// exit prompt removes a worktree). Claude Code then tells the user in the session that the worktree is gone.
    fn resume_cwd(&mut self, rec: &SessionRecord) -> PathBuf {
        if rec.cwd.is_dir() {
            return rec.cwd.clone();
        }
        let project = rec.project_id.as_ref().and_then(|id| self.store.project(id));
        let fallback = project.map(|p| p.git.as_ref().map_or(p.path.as_path(), |g| g.main_checkout()).to_path_buf()).filter(|p| p.is_dir());
        match fallback {
            Some(main) => {
                let short = crate::panels::widgets::short_path;
                self.toast(format!("{} is gone, so the session starts in {}.", short(&rec.cwd), short(&main)), false);
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
    pub(super) fn restart_session(&mut self, id: &str) {
        let was_shown = self.active_id().is_some_and(|a| a == id);
        let shell = self.shells.remove(id);
        self.relaunch(id);
        if let Some(shell) = shell {
            self.shells.insert(id.into(), shell);
        }
        if was_shown {
            self.show_session(id);
        }
    }

    pub(super) fn start_session(
        &mut self,
        mut record: SessionRecord,
        spec: SpawnSpec,
        is_new: bool,
    ) {
        let (terminal, rx) = TerminalView::spawn(&spec);
        let id = record.id.clone();
        let loading = record.kind == SessionKind::Claude;
        record.exit_code = None;
        record.last_active = now();
        // The pane's grid: from the view this one replaces, or the one shown there now.
        let layout = self.live_view(&id).or_else(|| self.active.as_ref().and_then(|a| self.live_view(a))).and_then(|v| v.layout());
        // Drop the previous terminal view for this record: an exited one, or a running one whose process this ends.
        self.live.retain(|l| l.id != id);
        self.purge_dead_shells();
        self.transcript_mtimes.remove(&id);
        if is_new {
            self.store.add_session(record);
        } else if let Some(r) = self.store.session_mut(&id) {
            r.exit_code = None;
            r.last_active = now();
        }
        let mut view = TerminalView::new(terminal, rx, id.clone());
        view.loading = loading;
        if let Some(layout) = layout {
            view.take_layout(layout);
        }
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
        self.live.push(Live {
            id: id.clone(),
            view,
            claude_name,
            title_stamp: None,
            account,
        });
        self.activate(&id);
        self.remember(&id);
        self.selected = Some(id.clone());
        self.main = Main::Terminals;
        self.sync_github();
        self.persist_layout();
        self.focus_active();
    }

    /// Make `id` the session shown.
    pub(super) fn activate(&mut self, id: &Id) {
        self.active = Some(id.clone());
        self.prune_live();
    }

    /// Exited terminals are only kept while shown; a shell panel goes with its session.
    fn prune_live(&mut self) {
        let active = &self.active;
        self.live.retain(|l| active.as_ref() == Some(&l.id) || l.view.is_running());
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

    /// The process ended by itself. Close the session the way the X does, so the terminal does not stay up behind a
    /// "Session ended" bar. A non-zero exit is a toast, because that output goes away with the view; a clean exit
    /// says nothing. This does not offer to remove a worktree: `/exit` already asked inside Claude Code, and the X
    /// (`close_live`) is the path that kills the process before that prompt.
    pub(super) fn on_exit(&mut self, id: &str, code: Option<i32>) {
        let start_error = self.live_view(id).and_then(|v| v.start_error());
        let title = self.store.session(id).map(|r| r.display_title().to_string()).unwrap_or_else(|| "Session".into());
        if let Some(rec) = self.store.session_mut(id) {
            rec.exit_code = Some(code.unwrap_or(0));
        }
        self.end_live(id);
        if let Some(err) = start_error {
            self.toast(format!("Could not start {title}: {err}"), true);
        } else if let Some(code) = code.filter(|c| *c != 0) {
            self.toast(format!("{title} ended (exit code {code})"), true);
        }
    }

    /// Clicking a session: running -> show its terminal; past -> open it right away (no intermediate page).
    pub fn show_session(&mut self, id: &str) {
        let id = id.to_string();
        self.selected = Some(id.clone());
        if self.live_view(&id).is_some() {
            self.activate(&id);
            self.remember(&id);
            self.main = Main::Terminals;
            self.sync_github();
            self.focus_active();
            self.persist_layout();
        } else {
            self.resume(&id);
        }
    }

    /// Give the keyboard to the shown session's terminal, or to nothing on the home page.
    pub fn focus_active(&mut self) {
        match (self.main, self.active.clone()) {
            (Main::Terminals, Some(id)) if self.live_view(&id).is_some() => self.focus_terminal(&id),
            _ => {
                self.keys_to = None;
                self.blur = true;
            }
        }
    }

    /// End a session. One that ran in a Claude Code worktree no other session uses then gets the offer to remove it,
    /// since Claude Code's own prompt only appears when a session exits with `/exit`.
    pub(super) fn close_live(&mut self, id: &str) {
        let was_in = self.store.session(id).map(|s| (s.cwd.clone(), s.project_id.clone()));
        self.end_live(id);
        if let Some((cwd, project)) = was_in {
            self.offer_worktree_removal(&cwd, project.as_deref());
        }
    }

    pub(super) fn end_live(&mut self, id: &str) {
        if self.logins.remove(id) {
            self.refresh_accounts();
        }
        let was_active = self.active_id().is_some_and(|a| a == id);
        self.live.retain(|l| l.id != id); // dropping the view drops the Terminal, which closes the PTY
        self.shells.remove(id);
        // Its language servers and folder watcher stop with the pane.
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
        self.focus_active();
    }

    /// The running sessions, in the sidebar's order.
    pub fn running_ids(&self) -> Vec<Id> {
        self.live.iter().filter(|l| l.view.is_running()).map(|l| l.id.clone()).collect()
    }

    /// Show running session number `n` (from 1), as the sidebar numbers them while Ctrl is held.
    pub fn show_numbered(&mut self, n: usize) {
        if let Some(id) = numbered(&self.running_ids(), n).cloned() {
            self.show_session(&id);
        }
    }

    pub fn cycle(&mut self, delta: isize) {
        let running = self.running_ids();
        if running.is_empty() {
            return;
        }
        let cur = self
            .active_id()
            .and_then(|a| running.iter().position(|r| r == a))
            .unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(running.len() as isize) as usize;
        self.show_session(&running[next].clone());
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
    use super::*;

    #[test]
    fn numbers_count_from_one() {
        let running = ["a", "b", "c"];
        assert_eq!(numbered(&running, 1), Some(&"a"));
        assert_eq!(numbered(&running, 3), Some(&"c"));
        assert_eq!(numbered(&running, 4), None, "no fourth session");
        assert_eq!(numbered(&running, 0), None);
    }

    #[test]
    fn a_restart_keeps_a_hand_picked_effort_only_while_auto_is_off_by_hand() {
        let mut meta = ClaudeMeta { effort: Some("low".into()), auto_state: usage::AutoState::Manual, ..Default::default() };
        assert_eq!(hand_effort_env(&meta), Some(("CLAUDIU_HAND_EFFORT".into(), "low".into())));
        meta.auto_state = usage::AutoState::On;
        assert_eq!(hand_effort_env(&meta), None, "auto picks again");
        meta.auto_state = usage::AutoState::Off;
        assert_eq!(hand_effort_env(&meta), None);
        meta = ClaudeMeta { auto_state: usage::AutoState::Manual, ..Default::default() };
        assert_eq!(hand_effort_env(&meta), None, "no level known");
    }
}
