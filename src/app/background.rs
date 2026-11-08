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

impl Workspace {
    /// Startup reconciliation: scan Claude Code's records off the UI thread, then merge.
    pub fn start_scan(&mut self) {
        let Some(home) = claude::claude_home() else {
            return;
        };
        self.scanning = true;
        self.rt.spawn(
            move || claude::scan_all(&home),
            |ws: &mut Workspace, result| {
                ws.scanning = false;
                ws.store.reconcile(&result.sessions, result.ok);
                if result.ok {
                    // Records from earlier runs that never produced a transcript: nothing to resume.
                    let live: Vec<Id> = ws.live.iter().map(|l| l.id.clone()).collect();
                    ws.store.data.sessions.retain(|s| {
                        live.contains(&s.id)
                            || !s.claude.as_ref().is_some_and(|c| {
                                c.external == ExternalState::Unknown && c.transcript.is_none()
                            })
                    });
                }
                ws.store.save_if_dirty();
            },
        );
    }

    /// Every five seconds: `tick`.
    pub(super) fn start_ticker(&mut self) {
        self.rt.after(Duration::from_secs(5), |ws: &mut Workspace| {
            ws.tick();
            ws.start_ticker();
        });
    }

    /// Whether a working session's sidebar icon turns: only while someone can see it, since every turning frame is
    /// drawn. Rune stops drawing a hidden or occluded window by itself.
    pub fn spin_visible(&self) -> bool {
        spin_visible(self.activity.values().any(|a| *a == Activity::Working), self.window_active, self.store.data.layout.sidebar_visible)
    }

    /// Once a second: derive each Claude session's `Activity` from its terminal output. When one stops working
    /// while the user isn't looking at it, mark it for attention, and if the whole window is in the
    /// background as well, flash the taskbar icon / bounce the dock icon.
    pub(super) fn start_activity_poll(&mut self) {
        self.rt.after(Duration::from_secs(1), |ws: &mut Workspace| {
            let mut seen = std::mem::take(&mut ws.status_files);
            ws.rt.spawn(
                move || {
                    let statuses = claude::claude_home().map(|h| claude::session_statuses(&h, &mut seen)).unwrap_or_default();
                    (statuses, crate::claude::bridge::take_requests(), seen)
                },
                |ws: &mut Workspace, (statuses, requests, seen)| {
                    ws.status_files = seen;
                    ws.serve_requests(requests);
                    ws.finish_handoffs();
                    ws.run_pending_switches(&statuses);
                    ws.update_activity(&statuses);
                    ws.start_activity_poll();
                },
            );
        });
    }

    /// `statuses` is Claude Code's own busy/idle/waiting per session id; sessions it doesn't list (older Claude
    /// Code, no registry file) fall back to the terminal-output heuristic.
    fn update_activity(&mut self, statuses: &HashMap<String, String>) {
        let window_active = self.window_active;
        let shown = self.active_id().cloned();
        let mut next: HashMap<Id, Activity> = HashMap::new();
        let mut flash = false;
        for l in &self.live {
            let is_claude = self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude);
            let view = &l.view;
            if !is_claude || !view.is_running() {
                continue;
            }
            let viewing = window_active && shown.as_deref() == Some(l.id.as_str());
            let status = self.store.session(&l.id).and_then(|r| statuses.get(r.claude_id()?));
            let working = status.map_or_else(|| view.is_working(), |s| s == "busy");
            let waiting = status.is_some_and(|s| s == "waiting");
            let before = self.activity.get(&l.id).copied();
            let now = next_activity(working, waiting, before, viewing);
            flash |= !window_active && before == Some(Activity::Working) && matches!(now, Some(Activity::Attention | Activity::Waiting));
            if let Some(now) = now {
                next.insert(l.id.clone(), now);
            }
        }
        if next != self.activity {
            self.activity = next;
        }
        if flash && let Some(window) = lifecycle::main_window() {
            platform::request_attention(&window);
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

    pub(super) fn tick(&mut self) {
        let purged = self.purge_dead_shells();
        self.store.save_if_dirty();
        self.refresh_live();
        self.remember_open();
        self.ingest_usage();
        self.sync_attached();
        self.sync_configs();
        self.raise_claude_priority();
        self.refresh_pending();
        // The steps above redraw when they change something. The sidebar's "5m ago" and the reset countdowns count
        // in minutes, so they need a redraw only when the minute turns; the prompt cache's bar shrinks as it runs out.
        let minute = crate::store::now() / 60;
        let cache_running = self.selected_cache().is_some_and(|(_, left, _)| left > 0);
        if purged || cache_running || minute != self.tick_minute {
            self.tick_minute = minute;
        }
    }

    /// Windows: run the Claude sessions above the builds and tests they start.
    fn raise_claude_priority(&self) {
        #[cfg(windows)]
        {
            let claude_pids: Vec<u32> = self
                .live
                .iter()
                .filter(|l| self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude))
                .filter_map(|l| l.view.terminal.process_id())
                .collect();
            self.rt.background(move || crate::priority::raise_claude_priority(&claude_pids));
        }
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

    /// Re-read transcripts of running Claude sessions so the context meter follows reality.
    fn refresh_live(&mut self) {
        let Some(home) = claude::claude_home() else {
            return;
        };
        let jobs: Vec<(Id, String, Option<PathBuf>, Option<SystemTime>)> = self
            .live
            .iter()
            .filter(|l| l.view.is_running())
            .filter_map(|l| {
                let rec = self.store.session(&l.id)?;
                let meta = rec.claude.as_ref()?;
                Some((
                    l.id.clone(),
                    meta.session_id.clone(),
                    meta.transcript.clone(),
                    self.transcript_mtimes.get(&l.id).copied(),
                ))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        self.rt.spawn(
            move || {
                    let mut out = Vec::new();
                    for (id, claude_id, known, last_mtime) in jobs {
                        let Some(path) = known
                            .filter(|p| p.is_file())
                            .or_else(|| claude::find_transcript(&home, &claude_id))
                        else {
                            continue;
                        };
                        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                        if mtime.is_some() && mtime == last_mtime {
                            continue;
                        }
                        if let Some(scan) = claude::read_session(&path) {
                            out.push((id, mtime, scan));
                        }
                    }
                    out
            },
            |ws: &mut Workspace, out| {
                for (id, mtime, scan) in out {
                    if let Some(m) = mtime {
                        ws.transcript_mtimes.insert(id.clone(), m);
                    }
                    ws.store
                        .reconcile(std::slice::from_ref(&scan.session), false);
                }
            },
        );
    }

    pub fn check_updates(&mut self) {
        crate::updater::check(self);
    }
}

fn spin_visible(working: bool, active: bool, sidebar: bool) -> bool {
    working && active && sidebar
}

#[cfg(test)]
mod tests {
    use super::{Activity, next_activity, spin_visible};

    #[test]
    fn spin_only_runs_for_a_visible_busy_sidebar() {
        for working in [false, true] {
            for active in [false, true] {
                for sidebar in [false, true] {
                    assert_eq!(spin_visible(working, active, sidebar), working && active && sidebar);
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
