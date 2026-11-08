//! Keeps one configuration across the main account and every alt account (see `claude/config_sync.rs`): in the
//! background every few seconds, and just before a session starts.

use std::time::{Duration, Instant};

use super::*;
use crate::claude::config_sync::{self as sync, Account as Files};

const EVERY: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct Runner {
    last_try: Option<Instant>,
    running: bool,
    /// The failure was already shown this run.
    told: bool,
}

impl Workspace {
    /// Where each account keeps its files. An account with a running session has its `.claude.json` left alone,
    /// because Claude Code rewrites that file from memory.
    fn account_files(&self) -> Vec<Files> {
        let busy = |account: &Account| {
            self.live.iter().any(|l| l.view.is_running() && self.store.session_account(&l.id).is_some_and(|a| a.id == account.id))
        };
        self.store
            .data
            .accounts
            .iter()
            .filter_map(|a| match &a.dir {
                Some(dir) => Some(sync::alt_account(dir, busy(a))),
                None => sync::main_account(busy(a)),
            })
            .collect()
    }

    pub(super) fn sync_configs(&mut self) {
        let accounts = self.account_files();
        if accounts.len() < 2 || self.config_sync.running || self.config_sync.last_try.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.config_sync.running = true;
        self.config_sync.last_try = Some(Instant::now());
        self.rt.spawn(
            move || { sync::sync(&accounts, &sync::state_path()) },
            move |ws: &mut Workspace, result| {
                ws.config_sync.running = false;
                ws.tell_sync_failure(result);
            },
        );
    }

    /// Before a session starts, so it begins with what the other accounts changed a moment ago.
    pub(super) fn sync_configs_now(&mut self) {
        let accounts = self.account_files();
        if accounts.len() >= 2 {
            let result = sync::sync(&accounts, &sync::state_path());
            self.tell_sync_failure(result);
        }
    }

    fn tell_sync_failure(&mut self, result: std::io::Result<()>) {
        if result.is_err() && !std::mem::replace(&mut self.config_sync.told, true) {
            self.toast("Couldn't keep the Claude Code settings the same in every account. Will keep trying.", true);
        }
    }
}
