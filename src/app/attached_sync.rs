//! Titles and states of the issues and pull requests attached to sessions come from GitHub, not from the model: new
//! items are asked about at once, and every item again now and then, so a rename or a close shows up.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::*;
use crate::github::cli::{Gh, RepoRef};
use crate::github::model::State;

/// Between asking about everything again.
const REFRESH_EVERY: Duration = Duration::from_secs(120);
/// Between tries while some item has no answer yet; doubles with each try that left one unanswered (no `gh`, no
/// network, not a GitHub folder), up to `REFRESH_EVERY`.
const RETRY_EVERY: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct Sync {
    last_try: Option<Instant>,
    running: bool,
    /// Tries in a row that left an item unanswered.
    failures: u32,
}

fn retry_wait(failures: u32) -> Duration {
    RETRY_EVERY.saturating_mul(2u32.saturating_pow(failures)).min(REFRESH_EVERY)
}

/// One answer: the session, which item, and what GitHub said.
type Answer = (Id, bool, u32, String, State);

impl Sync {
    fn due(&self, some_unanswered: bool) -> bool {
        let wait = if some_unanswered { retry_wait(self.failures) } else { REFRESH_EVERY };
        !self.running && self.last_try.is_none_or(|t| t.elapsed() >= wait)
    }
}

impl Workspace {
    pub(super) fn sync_attached(&mut self, cx: &mut Context<Self>) {
        let asks: Vec<(Id, PathBuf, Vec<(bool, u32)>)> = self
            .store
            .data
            .sessions
            .iter()
            // A session whose folder is gone cannot be asked about (and would never be answered).
            .filter(|s| s.cwd.is_dir())
            .filter_map(|s| {
                let items: Vec<_> = s.claude.as_ref()?.attached.iter().map(|a| (a.pull, a.number)).collect();
                (!items.is_empty()).then(|| (s.id.clone(), s.cwd.clone(), items))
            })
            .collect();
        let some_unanswered = self.unanswered_items();
        if asks.is_empty() || !self.attached_sync.due(some_unanswered) {
            return;
        }
        self.attached_sync.running = true;
        self.attached_sync.last_try = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            let answers = cx.background_executor().spawn(async move { ask_github(asks) }).await;
            let _ = this.update(cx, |ws, cx| {
                ws.attached_sync.running = false;
                let changed = answers.into_iter().fold(false, |c, (id, pull, number, title, state)| {
                    ws.edit_attached(&id, |list| links::set_status(list, pull, number, &title, state)) | c
                });
                ws.attached_sync.failures = if ws.unanswered_items() { ws.attached_sync.failures + 1 } else { 0 };
                if changed {
                    ws.store.mark_dirty();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Whether some attached item of a session with a folder has no answer from GitHub yet.
    fn unanswered_items(&self) -> bool {
        let sessions = self.store.data.sessions.iter().filter(|s| s.cwd.is_dir());
        sessions.filter_map(|s| s.claude.as_ref()).flat_map(|m| &m.attached).any(|a| a.state.is_none())
    }
}

/// Runs off the UI thread. An item `gh` cannot answer for keeps what it had.
fn ask_github(asks: Vec<(Id, PathBuf, Vec<(bool, u32)>)>) -> Vec<Answer> {
    let Some(exe) = crate::github::cli::locate() else { return vec![] };
    let mut repos: HashMap<PathBuf, Option<(Gh, RepoRef)>> = HashMap::new();
    let mut answers = vec![];
    for (id, cwd, items) in asks {
        let found = repos.entry(cwd.clone()).or_insert_with(|| {
            let gh = Gh::new(exe.clone(), cwd);
            gh.repo().ok().map(|repo| (gh, repo))
        });
        let Some((gh, repo)) = found else { continue };
        for (pull, number) in items {
            if let Ok((title, state)) = gh.item_status(repo, number) {
                answers.push((id.clone(), pull, number, title, state));
            }
        }
    }
    answers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrying_backs_off_to_the_normal_pace() {
        assert_eq!(retry_wait(0), Duration::from_secs(15));
        assert_eq!(retry_wait(1), Duration::from_secs(30));
        assert_eq!(retry_wait(2), Duration::from_secs(60));
        assert_eq!(retry_wait(3), REFRESH_EVERY);
        assert_eq!(retry_wait(u32::MAX), REFRESH_EVERY, "no overflow");
    }
}
