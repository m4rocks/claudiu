//! Background work: the timers and polls that keep the workspace current (activity, live statuses, usage, updates).

use super::*;

impl Workspace {
    /// Startup reconciliation: scan Claude Code's records off the UI thread, then merge.
    pub fn start_scan(&mut self, cx: &mut Context<Self>) {
        let Some(home) = claude::claude_home() else {
            return;
        };
        self.scanning = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { claude::scan_all(&home) })
                .await;
            let _ = this.update(cx, |ws, cx| {
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

    /// While a session works, its sidebar dot pulses: the session list is redrawn a few times a second (with the
    /// window's top level, which reuses the other cached parts). It stops once nothing works.
    fn start_pulse(&mut self, cx: &mut Context<Self>) {
        if self.pulsing {
            return;
        }
        self.pulsing = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(crate::panels::sidebar::PULSE_STEP).await;
                let working = this.update(cx, |ws, cx| {
                    let working = ws.activity.values().any(|a| *a == Activity::Working);
                    match &ws.session_list {
                        Some(list) if working => list.update(cx, |_, cx| cx.notify()),
                        _ => ws.pulsing = working,
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

    /// Once a second: derive each Claude session's `Activity` from its terminal output. When one stops working
    /// while the user isn't looking at it, mark it for attention, and if the whole window is in the
    /// background as well, flash the taskbar icon / bounce the dock icon.
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
                    ws.finish_handoffs(window, cx);
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
        let mut flash = false;
        for l in &self.live {
            let is_claude = self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude);
            let view = l.view.read(cx);
            if !is_claude || !view.is_running() {
                continue;
            }
            let viewing = window_active && shown.as_deref() == Some(l.id.as_str());
            let status = self.store.session(&l.id).and_then(|r| statuses.get(r.claude_id()?));
            let working = status.map_or_else(|| view.is_working(), |s| s == "busy");
            match (working, self.activity.get(&l.id)) {
                (true, _) => {
                    next.insert(l.id.clone(), Activity::Working);
                }
                (false, Some(Activity::Working)) if !viewing => {
                    flash |= !window_active;
                    next.insert(l.id.clone(), Activity::Attention);
                }
                (false, Some(Activity::Attention)) if !viewing => {
                    next.insert(l.id.clone(), Activity::Attention);
                }
                _ => {}
            }
        }
        if next != self.activity {
            self.activity = next;
            cx.notify();
        }
        if self.activity.values().any(|a| *a == Activity::Working) {
            self.start_pulse(cx);
        }
        if flash {
            platform::request_attention(window);
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
        self.refresh_live(cx);
        self.remember_open(cx);
        self.ingest_usage(cx);
        self.sync_attached(cx);
        self.sync_configs(cx);
        self.refresh_if_stale(cx);
        self.refresh_marks(cx);
        self.raise_claude_priority(cx);
        self.refresh_pending(cx);
        // The steps above redraw when they change something. The sidebar's "5m ago" and the reset countdowns count
        // in minutes, so they need a redraw only when the minute turns; the prompt cache's bar shrinks as it runs out.
        let minute = crate::store::now() / 60;
        let cache_running = self.selected_cache().is_some_and(|(_, left, _)| left > 0);
        if purged || cache_running || minute != self.tick_minute {
            self.tick_minute = minute;
            cx.notify();
        }
    }

    /// Windows: run the Claude sessions above the builds and tests they start.
    fn raise_claude_priority(&self, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            let claude_pids: Vec<u32> = self
                .live
                .iter()
                .filter(|l| self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude))
                .filter_map(|l| l.view.read(cx).terminal.process_id())
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

    /// Re-read transcripts of running Claude sessions so the context meter follows reality.
    fn refresh_live(&mut self, cx: &mut Context<Self>) {
        let Some(home) = claude::claude_home() else {
            return;
        };
        let jobs: Vec<(Id, String, Option<PathBuf>, Option<SystemTime>)> = self
            .live
            .iter()
            .filter(|l| l.view.read(cx).is_running())
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
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move {
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
                })
                .await;
            let _ = this.update(cx, |ws, cx| {
                for (id, mtime, scan) in out {
                    if let Some(m) = mtime {
                        ws.transcript_mtimes.insert(id.clone(), m);
                    }
                    ws.store
                        .reconcile(std::slice::from_ref(&scan.session), false);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn check_updates(&mut self, cx: &mut Context<Self>) {
        crate::updater::check(self, cx);
    }
}
