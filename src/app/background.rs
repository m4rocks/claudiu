//! Background work: the timers and polls that keep the workspace current (activity, live statuses, usage, updates).

use super::*;

/// What a session's row shows, given whether it is working or waiting on the user, what the row showed before, and
/// whether the user is looking at it. A pending question takes precedence over working (ACP keeps the turn open until
/// answered) and is shown even to someone looking at the session; a finished turn is shown only to someone who was
/// elsewhere and clears once they look.
fn next_activity(working: bool, waiting: bool, before: Option<Activity>, viewing: bool) -> Option<Activity> {
    match (waiting, working, before) {
        (true, _, _) => Some(Activity::Waiting),
        (false, true, _) => Some(Activity::Working),
        (false, false, Some(Activity::Working | Activity::Attention)) if !viewing => Some(Activity::Attention),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttentionEvent {
    Finished,
    Waiting,
}

/// How long a session must stay idle before its turn counts as finished. Agents often end one turn and start the next
/// a moment later (a queued follow-up, a steer, a background task reporting back), which isn't worth an alert.
const SETTLE: Duration = Duration::from_secs(6);

/// Whether a transition should alert the user (with a system notification when the window is in the background, or an
/// in-app toast when another session is shown). A turn that ended with work still running in the background
/// (`pending`) gets its check mark quietly: the agent will usually pick the work up again and finish later.
fn attention_event(before: Option<Activity>, now: Option<Activity>, viewing: bool, pending: bool) -> Option<AttentionEvent> {
    if viewing {
        return None;
    }
    match (before, now) {
        (Some(Activity::Working), Some(Activity::Attention)) if !pending => Some(AttentionEvent::Finished),
        (prev, Some(Activity::Waiting)) if prev != Some(Activity::Waiting) => Some(AttentionEvent::Waiting),
        _ => None,
    }
}

fn notice_body(event: AttentionEvent, harness: Harness, question: Option<&str>) -> String {
    let who = harness.name();
    match event {
        AttentionEvent::Finished => format!("{who} finished."),
        AttentionEvent::Waiting => match question.filter(|q| !q.trim().is_empty()) {
            Some(q) => format!("{who} needs your answer: {q}"),
            None => format!("{who} is waiting for your answer."),
        },
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
                        (statuses, crate::mcp::bridge::take_requests(), seen)
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
        let mut system_notices: Vec<(String, String)> = vec![];
        let mut toast_notice = None;
        for l in &self.live {
            let Some(rec) = self.store.session(&l.id) else { continue };
            if rec.kind == SessionKind::Shell || !l.view.is_running(cx) {
                continue;
            }
            let viewing = window_active && shown.as_deref() == Some(l.id.as_str());
            // A thread knows from ACP itself: a turn is out, or a permission/ask question is open.
            let (mut working, waiting, question_title, pending) = match &l.view {
                SessionView::Thread(v) => {
                    v.update(cx, |thread, cx| thread.set_viewing(viewing, cx));
                    let t = v.read(cx);
                    (t.is_working(), t.is_waiting(), t.pending_question_title(), t.has_background_work())
                }
                SessionView::Terminal(v) => {
                    let status = rec.provider_session_id().and_then(|cid| statuses.get(cid));
                    (status.map_or_else(|| v.read(cx).is_working(), |s| s == "busy"), status.is_some_and(|s| s == "waiting"), None, false)
                }
            };
            let before = self.activity.get(&l.id).copied();
            if !working && !waiting && before == Some(Activity::Working) {
                let since = *self.idle_since.entry(l.id.clone()).or_insert_with(std::time::Instant::now);
                working = since.elapsed() < SETTLE;
            }
            if working || waiting || before != Some(Activity::Working) {
                self.idle_since.remove(&l.id);
            }
            let now = next_activity(working, waiting, before, viewing);
            if before == Some(Activity::Working) && now != Some(Activity::Working)
                && let SessionView::Thread(v) = &l.view
            {
                v.update(cx, |_, cx| cx.notify());
            }
            if let Some(ev) = attention_event(before, now, viewing, pending) {
                let title = rec.display_title().to_string();
                let harness = rec.agent.as_ref().map_or(self.store.data.settings.harness, |m| m.harness);
                let body = notice_body(ev, harness, question_title.as_deref());
                if window_active {
                    let text = match ev {
                        AttentionEvent::Finished => format!("“{title}” finished."),
                        AttentionEvent::Waiting => format!("“{title}” needs your answer."),
                    };
                    toast_notice = Some(text);
                } else {
                    system_notices.push((title, body));
                }
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
        for (title, body) in system_notices {
            platform::notify(title, body);
        }
        if let Some(text) = toast_notice {
            self.toast(text, cx);
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
                .filter(|r| r.agent.as_ref().is_some_and(|m| m.harness == Harness::Claude))
                .and_then(|r| Some((r.provider_session_id()?, r.chosen_title()?)))
                .filter(|(_, w)| l.claude_name.as_deref() != Some(*w))
            else {
                continue;
            };
            l.claude_name = Some(w.to_string());
            claude::write_name(cid, Some(w));
        }
    }

    pub(super) fn tick(&mut self, cx: &mut Context<Self>) {
        let purged = self.purge_dead_shells();
        self.store.save_if_dirty();
        self.remember_open(cx);
        self.ingest_usage(cx);
        self.sync_attached(cx);
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
                .filter(|l| self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Agent))
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
    use super::{Activity, AttentionEvent, Harness, attention_event, next_activity, notice_body, spin_visible};

    #[test]
    fn a_notification_says_a_background_session_finished_or_asks() {
        use Activity::*;
        assert_eq!(attention_event(Some(Working), Some(Attention), false, false), Some(AttentionEvent::Finished));
        assert_eq!(attention_event(Some(Working), Some(Waiting), false, false), Some(AttentionEvent::Waiting));
        assert_eq!(attention_event(None, Some(Waiting), false, false), Some(AttentionEvent::Waiting));
        assert_eq!(attention_event(Some(Working), Some(Attention), true, false), None, "already viewing it");
        assert_eq!(attention_event(Some(Working), Some(Waiting), true, false), None, "already viewing it");
        assert_eq!(attention_event(Some(Attention), Some(Attention), false, false), None, "already told");
        assert_eq!(attention_event(Some(Waiting), Some(Waiting), false, false), None, "already told");
        assert_eq!(attention_event(Some(Waiting), Some(Working), false, false), None, "it started working");
        assert_eq!(attention_event(Some(Working), Some(Attention), false, true), None, "work is still running in the background");
        assert_eq!(attention_event(Some(Working), Some(Waiting), false, true), Some(AttentionEvent::Waiting), "a question still asks");
        assert_eq!(notice_body(AttentionEvent::Finished, Harness::Codex, None), "Codex finished.");
        assert_eq!(notice_body(AttentionEvent::Waiting, Harness::Claude, Some("Run tests?")), "Claude Code needs your answer: Run tests?");
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
    fn busy_is_working_whatever_came_before_unless_waiting() {
        for before in [None, Some(Activity::Working), Some(Activity::Waiting), Some(Activity::Attention)] {
            assert_eq!(next_activity(true, false, before, true), Some(Activity::Working));
            assert_eq!(next_activity(true, false, before, false), Some(Activity::Working));
            assert_eq!(next_activity(true, true, before, true), Some(Activity::Waiting), "a pending question mid-turn shows as waiting");
            assert_eq!(next_activity(true, true, before, false), Some(Activity::Waiting));
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
