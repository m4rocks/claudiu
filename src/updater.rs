//! Auto-update via Velopack, fed by GitHub Releases (no Claudiu-operated server).
//!
//! * Only active when the app was installed by the Velopack installer; dev builds and portable
//!   copies without an install silently skip it.
//! * The only network request is the public GitHub Releases API call Velopack makes. No identifiers
//!   or usage data are sent. Set `settings.update_url` to `""` in state.json to disable entirely.

use std::time::Duration;

use gpui::{Context, Window};
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, sources::GithubSource};

use crate::app::Workspace;

pub const DEFAULT_REPO: &str = "https://github.com/m4rocks/claudiu";

#[derive(Default, Clone, Debug, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available { version: String },
    Downloading,
    Ready { version: String },
    Error(String),
}

#[derive(Default)]
pub struct UpdateState {
    pub status: Status,
    manager: Option<UpdateManager>,
    info: Option<UpdateInfo>,
}

fn repo_url(ws: &Workspace) -> Option<String> {
    match ws.store.data.settings.update_url.as_deref() {
        Some("") => None,
        Some(url) => Some(url.to_string()),
        None => Some(DEFAULT_REPO.to_string()),
    }
}

/// Look for a newer release in the background.
pub fn check(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(url) = repo_url(ws) else { return };
    if matches!(ws.update.status, Status::Checking | Status::Downloading) {
        return;
    }
    ws.update.status = Status::Checking;
    cx.spawn(async move |this, cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                let manager = UpdateManager::new(GithubSource::new(&url, None, false), None, None).map_err(|e| e.to_string())?;
                let check = manager.check_for_updates().map_err(|e| e.to_string())?;
                Ok::<_, String>((manager, check))
            })
            .await;
        let _ = this.update(cx, |ws, cx| {
            match result {
                Ok((manager, UpdateCheck::UpdateAvailable(info))) => {
                    ws.update.status = Status::Available { version: info.TargetFullRelease.Version.clone() };
                    ws.update.manager = Some(manager);
                    ws.update.info = Some(*info);
                }
                Ok(_) => ws.update.status = Status::UpToDate,
                // Not installed via Velopack (dev build / portable): nothing to do, and nothing to show.
                Err(_) => ws.update.status = Status::Idle,
            }
            cx.notify();
        });
        // Re-check periodically while the app stays open.
        cx.background_executor().timer(Duration::from_secs(6 * 3600)).await;
        let _ = this.update(cx, |ws, cx| check(ws, cx));
    })
    .detach();
}

/// Download (first click) then apply and restart (second click).
pub fn install(ws: &mut Workspace, _window: &mut Window, cx: &mut Context<Workspace>) {
    let (Some(manager), Some(info)) = (ws.update.manager.clone(), ws.update.info.clone()) else { return };
    match ws.update.status.clone() {
        Status::Available { version } => {
            ws.update.status = Status::Downloading;
            cx.spawn(async move |this, cx| {
                let m = manager.clone();
                let i = info.clone();
                let result = cx.background_executor().spawn(async move { m.download_updates(&i, None).map_err(|e| e.to_string()) }).await;
                let _ = this.update(cx, |ws, cx| {
                    ws.update.status = match result {
                        Ok(()) => Status::Ready { version },
                        Err(e) => Status::Error(e),
                    };
                    if let Status::Error(e) = &ws.update.status {
                        let msg = format!("Update failed: {e}");
                        ws.toast(msg, true, cx);
                    }
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
        }
        Status::Ready { .. } => {
            // Persist state, then hand over to the updater; it restarts Claudiu into the new version.
            ws.persist_for_restart();
            if let Err(e) = manager.apply_updates_and_restart(&info) {
                ws.toast(format!("Could not apply update: {e}"), true, cx);
            }
        }
        _ => {}
    }
}
