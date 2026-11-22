//! Background work: the timers and polls that keep the workspace current (activity, live statuses, usage, updates).

use super::*;

/// What a Claude session's row shows, given what Claude Code says it is doing, what the row showed before, and
/// whether the user is looking at it. A question is shown even to someone looking at it, since only they can answer;
/// a finished turn is shown only to someone who was elsewhere and clears once they look.
fn next_activity(working: bool, waiting: bool, before: Option<Activity>, viewing: bool) -> Option<Activity> {
    match (working, waiting, before) {
        (true, _, _) => Some(Activity::Working),
        (false, true, _) => Some(Activity::Waiting),
        (false, false, Some(Activity::Working | Activity::Attention)) if !viewing => Some(Activity::Attention),
        _ => None,
    }
}

/// What the system notification says when a session stops working while the window is in the background: it
/// finished its turn, or it asks something. Nothing for any other change.
fn notice(before: Option<Activity>, now: Option<Activity>, window_active: bool) -> Option<&'static str> {
    match (window_active, before, now) {
        (false, Some(Activity::Working), Some(Activity::Attention)) => Some("Claude finished."),
        (false, Some(Activity::Working), Some(Activity::Waiting)) => Some("Claude is waiting for your answer."),
        _ => None,
    }
}

impl Workspace {
    /// Runs `work` off the UI thread, then `done` with its result back on it; saves what `done` changed and redraws.
    pub(crate) fn in_background<T: Send + 'static>(
        &mut self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { work() }).await;
            let _ = this.update(cx, |ws, cx| {
                done(ws, result, cx);
                ws.store.save_if_dirty();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn start_ticker(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                if this.update(cx, |ws, cx| ws.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// While a session works, its sidebar icon spins: the session list is redrawn many times a second (with the
    /// window's top level, which reuses the other cached parts). It stops when idle or out of view.
    pub(crate) fn start_spin(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.spinning || !self.spin_visible(window) {
            return;
        }
        self.spinning = true;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(crate::panels::status_icon::SPIN_STEP).await;
                let working = this.update_in(cx, |ws, window, cx| {
                    let working = ws.spin_visible(window);
                    match &ws.session_list {
                        Some(list) if working => list.update(cx, |_, cx| cx.notify()),
                        _ => ws.spinning = working,
                    }
                    working
                });
                if !matches!(working, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn spin_visible(&self, window: &Window) -> bool {
        spin_visible(
            self.activity.values().any(|a| *a == Activity::Working),
            window.is_window_active(),
            self.store.data.layout.sidebar_visible,
            self.editing(),
        )
    }

    /// Once a second: derive each Claude session's `Activity` from its terminal output. When one stops working
    /// while the user isn't looking at it, mark it for attention, and if the whole window is in the
    /// background as well, send a system notification.
    pub(super) fn start_activity_poll(&mut self, window: &Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let mut seen = claude::StatusFiles::default();
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let (statuses, requests, back) = cx
                    .background_executor()
                    .spawn(async move {
                        let statuses = claude::claude_home().map(|h| claude::session_statuses(&h, &mut seen)).unwrap_or_default();
                        (statuses, crate::claude::bridge::take_requests(), seen)
                    })
                    .await;
                seen = back;
                let update = this.update_in(cx, |ws, window, cx| {
                    ws.serve_requests(requests, window, cx);
                    ws.run_pending_switches(&statuses, window, cx);
                    ws.check_open_files(window, cx);
                    ws.update_activity(window, cx, &statuses)
                });
                if update.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// `statuses` is Claude Code's own busy/idle/waiting per session id; sessions it doesn't list (older Claude
    /// Code, no registry file) fall back to the terminal-output heuristic.
    fn update_activity(&mut self, window: &mut Window, cx: &mut Context<Self>, statuses: &HashMap<String, String>) {
        let window_active = window.is_window_active();
        let shown = self.active_id().cloned();
        let mut next: HashMap<Id, Activity> = HashMap::new();
        let mut notices = vec![];
        for l in &self.live {
            let is_claude = self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude);
            if !is_claude || !l.view.is_running(cx) {
                continue;
            }
            let viewing = window_active && shown.as_deref() == Some(l.id.as_str());
            // A thread knows from ACP itself: a turn is out, or a permission question is open.
            let (working, waiting) = match &l.view {
                SessionView::Thread(v) => (v.read(cx).is_working(), v.read(cx).is_waiting()),
                SessionView::Terminal(v) => {
                    let status = self.store.session(&l.id).and_then(|r| statuses.get(r.claude_id()?));
                    (status.map_or_else(|| v.read(cx).is_working(), |s| s == "busy"), status.is_some_and(|s| s == "waiting"))
                }
            };
            let before = self.activity.get(&l.id).copied();
            let now = next_activity(working, waiting, before, viewing);
            if let Some(body) = notice(before, now, window_active) {
                notices.push((self.store.session(&l.id).map(|r| r.display_title().to_string()).unwrap_or_default(), body));
            }
            if let Some(now) = now {
                next.insert(l.id.clone(), now);
            }
        }
        if next != self.activity {
            self.activity = next;
            cx.notify();
        }
        if self.activity.values().any(|a| *a == Activity::Working) {
            self.start_spin(window, cx);
        }
        for (title, body) in notices {
            platform::notify(title, body);
        }
        self.sync_claude_names();
    }

    /// Claude Code keeps its own session name (`/resume`, Remote Control, the mobile apps). `--name` covers
    /// launches; for a running session the name goes to its name file and the mod runs `/rename` once Claude
    /// Code is idle. Nothing is typed into the PTY.
    fn sync_claude_names(&mut self) {
        for l in &mut self.live {
            let Some((cid, w)) = self
                .store
                .session(&l.id)
                .and_then(|r| Some((r.claude_id()?, r.chosen_title()?)))
                .filter(|(_, w)| l.claude_name.as_deref() != Some(*w))
            else {
                continue;
            };
            l.claude_name = Some(w.to_string());
            mcp::write_name(cid, Some(w));
        }
    }

    pub(super) fn tick(&mut self, cx: &mut Context<Self>) {
        let purged = self.purge_dead_shells();
        self.store.save_if_dirty();
        self.remember_open(cx);
        self.ingest_usage(cx);
        self.sync_attached(cx);
        self.sync_configs(cx);
        self.refresh_if_stale(cx);
        self.refresh_marks(cx);
        self.raise_claude_priority(cx);
        self.refresh_pending(cx);
        // The steps above redraw when they change something. The sidebar's "5m ago" and the reset countdowns count
        // in minutes, so they need a redraw only when the minute turns.
        let minute = crate::store::now() / 60;
        if purged || minute != self.tick_minute {
            self.tick_minute = minute;
            cx.notify();
        }
    }

    /// Windows: run the Claude sessions above the builds and tests they start.
    fn raise_claude_priority(&self, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            // Only a session still drawn in a terminal has a process id here. Agents now run as ACP processes,
            // whose ids the chat view doesn't expose yet, so they keep the normal priority until it does.
            let claude_pids: Vec<u32> = self
                .live
                .iter()
                .filter(|l| self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude))
                .filter_map(|l| l.view.terminal()?.read(cx).terminal.process_id())
                .collect();
            cx.background_executor().spawn(async move { crate::priority::raise_claude_priority(&claude_pids) }).detach();
        }
        #[cfg(not(windows))]
        let _ = cx;
    }

    /// Terminal sessions live only in CURRENT SESSION: once a shell's process is gone and its pane has been
    /// closed or replaced, its record is dropped instead of lingering in project history.
    /// True when it dropped anything.
    pub(super) fn purge_dead_shells(&mut self) -> bool {
        let keep: Vec<Id> = self.live.iter().map(|l| l.id.clone()).collect();
        let before = self.store.data.sessions.len();
        self.store
            .data
            .sessions
            .retain(|s| s.kind != SessionKind::Shell || keep.contains(&s.id));
        let mut purged = self.store.data.sessions.len() != before;
        if self
            .selected
            .as_ref()
            .is_some_and(|id| self.store.session(id).is_none())
        {
            self.selected = None;
            purged = true;
        }
        purged
    }

    pub fn check_updates(&mut self, cx: &mut Context<Self>) {
        crate::updater::check(self, cx);
    }
}

fn spin_visible(working: bool, active: bool, sidebar: bool, editing: bool) -> bool {
    working && active && sidebar && !editing
}

#[cfg(test)]
mod tests {
    use super::{Activity, next_activity, notice, spin_visible};

    #[test]
    fn a_notification_says_a_background_session_finished_or_asks() {
        use Activity::*;
        assert_eq!(notice(Some(Working), Some(Attention), false), Some("Claude finished."));
        assert_eq!(notice(Some(Working), Some(Waiting), false), Some("Claude is waiting for your answer."));
        assert_eq!(notice(Some(Working), Some(Attention), true), None, "the window is in front");
        assert_eq!(notice(Some(Attention), Some(Attention), false), None, "already told");
        assert_eq!(notice(Some(Waiting), Some(Working), false), None, "it started working");
        assert_eq!(notice(None, Some(Waiting), false), None, "it never worked meanwhile");
    }

    #[test]
    fn spin_only_runs_for_a_visible_busy_sidebar() {
        for working in [false, true] {
            for active in [false, true] {
                for sidebar in [false, true] {
                    for editing in [false, true] {
                        assert_eq!(spin_visible(working, active, sidebar, editing),
                            working && active && sidebar && !editing);
                    }
                }
            }
        }
    }

    #[test]
    fn busy_is_working_whatever_came_before() {
        for before in [None, Some(Activity::Working), Some(Activity::Waiting), Some(Activity::Attention)] {
            assert_eq!(next_activity(true, false, before, true), Some(Activity::Working));
            assert_eq!(next_activity(true, false, before, false), Some(Activity::Working));
        }
    }

    #[test]
    fn a_question_shows_even_to_someone_looking_at_the_session() {
        assert_eq!(next_activity(false, true, Some(Activity::Working), true), Some(Activity::Waiting));
        assert_eq!(next_activity(false, true, None, false), Some(Activity::Waiting));
    }

    #[test]
    fn a_finished_turn_waits_to_be_seen_and_clears_once_looked_at() {
        assert_eq!(next_activity(false, false, Some(Activity::Working), false), Some(Activity::Attention));
        assert_eq!(next_activity(false, false, Some(Activity::Attention), false), Some(Activity::Attention));
        assert_eq!(next_activity(false, false, Some(Activity::Working), true), None);
        assert_eq!(next_activity(false, false, Some(Activity::Attention), true), None);
    }

    #[test]
    fn an_idle_session_nobody_is_owed_news_from_shows_nothing() {
        assert_eq!(next_activity(false, false, None, false), None);
        assert_eq!(next_activity(false, false, Some(Activity::Waiting), false), None);
    }
}
