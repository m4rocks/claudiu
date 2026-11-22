//! Auto-update via Velopack, fed by GitHub Releases (no m4code-operated server).
//!
//! * Only active when the app was installed by the Velopack installer; dev builds and portable
//!   copies without an install silently skip it.
//! * The only network request is the public GitHub Releases API call Velopack makes. No identifiers
//!   or usage data are sent. Set `settings.update_url` to `""` in state.json to turn off background
//!   checks; Check now in Settings still looks once, at the default feed.

use std::time::Duration;

use gpui::Context;
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, sources::GithubSource};

use crate::app::Workspace;

pub const DEFAULT_REPO: &str = "https://github.com/m4rocks/m4code";

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

/// Look for a newer release in the background, and again every six hours while the app stays open.
pub fn check(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(Duration::from_secs(6 * 3600)).await;
        let _ = this.update(cx, check);
    })
    .detach();
    let Some(url) = repo_url(ws) else { return };
    look(ws, url, false, cx);
}

/// The Check now button in Settings: one look, made even with background checks off, that says what it found.
pub fn check_now(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let url = repo_url(ws).unwrap_or_else(|| DEFAULT_REPO.to_string());
    look(ws, url, true, cx);
}

fn look(ws: &mut Workspace, url: String, asked: bool, cx: &mut Context<Workspace>) {
    // Once an update is downloading or downloaded, a re-check would only throw that away.
    let busy = match &ws.update.status {
        Status::Checking => Some(None),
        Status::Downloading => Some(Some("An update is downloading".to_string())),
        Status::Ready { version } => Some(Some(format!("m4code {version} is ready. Restart to update."))),
        _ => None,
    };
    if let Some(said) = busy {
        if let Some(text) = said.filter(|_| asked) {
            ws.toast(text, cx);
        }
        return;
    }
    let previous = std::mem::replace(&mut ws.update.status, Status::Checking);
    ws.in_background(
        move || {
            let manager = UpdateManager::new(GithubSource::new(&url, None, false), None, None).map_err(|e| match e {
                velopack::Error::NotInstalled(_) => "updates work only in a copy installed with the m4code installer".to_string(),
                e => e.to_string(),
            })?;
            let check = manager.check_for_updates().map_err(|e| e.to_string())?;
            Ok::<_, String>((manager, check))
        },
        move |ws, result, cx| {
            let found = match result {
                Ok((manager, UpdateCheck::UpdateAvailable(info))) => {
                    let version = info.TargetFullRelease.Version.clone();
                    ws.update.manager = Some(manager);
                    ws.update.info = Some(*info);
                    Ok(Found::Newer(version))
                }
                Ok((manager, UpdateCheck::NoUpdateAvailable)) => Ok(Found::Latest(manager.get_current_version_as_string())),
                Ok((_, UpdateCheck::RemoteIsEmpty)) => Err("the update feed has no releases".to_string()),
                Err(e) => Err(e),
            };
            let (status, said) = outcome(previous, found, asked);
            ws.update.status = status;
            if let Some(text) = said {
                ws.toast(text, cx);
            }
        },
        cx,
    );
}

/// What a check found: a newer release, or that the installed version is the latest.
#[derive(Debug)]
enum Found {
    Newer(String),
    Latest(String),
}

/// The status a check leaves behind, and, when the user asked for the check, what to tell them.
fn outcome(previous: Status, found: Result<Found, String>, asked: bool) -> (Status, Option<String>) {
    let (status, said) = match found {
        Ok(Found::Newer(version)) => (Status::Available { version: version.clone() }, format!("m4code {version} is available")),
        Ok(Found::Latest(installed)) => (Status::UpToDate, format!("m4code {installed} is the latest version")),
        // A failed re-check (e.g. offline) keeps an update already offered.
        Err(e) if matches!(previous, Status::Available { .. }) => (previous, format!("Could not check for updates: {e}")),
        // Not installed via Velopack (dev build / portable): nothing to do, and nothing to show unless asked.
        Err(e) => (Status::Idle, format!("Could not check for updates: {e}")),
    };
    (status, asked.then_some(said))
}

/// Download (first click) then apply and restart (second click).
pub fn install(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let (Some(manager), Some(info)) = (ws.update.manager.clone(), ws.update.info.clone()) else { return };
    match ws.update.status.clone() {
        Status::Available { version } => {
            ws.update.status = Status::Downloading;
            ws.in_background(
                move || manager.download_updates(&info, None).map_err(|e| e.to_string()),
                move |ws, result, cx| {
                    ws.update.status = match result {
                        Ok(()) => Status::Ready { version },
                        Err(e) => Status::Error(e),
                    };
                    if let Status::Error(e) = &ws.update.status {
                        let msg = format!("Update failed: {e}");
                        ws.toast(msg, cx);
                    }
                },
                cx,
            );
        }
        Status::Ready { .. } => {
            // Persist state, then hand over to the updater; it restarts m4code into the new version.
            ws.finalize(cx);
            if let Err(e) = manager.apply_updates_and_restart(&info) {
                ws.toast(format!("Could not apply update: {e}"), cx);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{Found, Status, outcome};

    #[test]
    fn only_a_check_the_user_asked_for_says_what_it_found() {
        let available = Status::Available { version: "9.9.9".into() };
        assert_eq!(outcome(Status::Idle, Ok(Found::Newer("9.9.9".into())), false), (available.clone(), None));
        assert_eq!(outcome(Status::Idle, Ok(Found::Newer("9.9.9".into())), true), (available, Some("m4code 9.9.9 is available".into())));
        let latest = "m4code 1.2.3 is the latest version".to_string();
        assert_eq!(outcome(Status::Idle, Ok(Found::Latest("1.2.3".into())), true), (Status::UpToDate, Some(latest)));
        assert_eq!(outcome(Status::Idle, Err("offline".into()), false), (Status::Idle, None));
        assert_eq!(outcome(Status::Idle, Err("offline".into()), true), (Status::Idle, Some("Could not check for updates: offline".into())));
    }

    #[test]
    fn a_failed_check_keeps_an_update_already_offered() {
        let available = Status::Available { version: "9.9.9".into() };
        assert_eq!(outcome(available.clone(), Err("offline".into()), false), (available.clone(), None));
        assert_eq!(outcome(available.clone(), Err("offline".into()), true).0, available);
    }
}
