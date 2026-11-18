//! Auto-update via Velopack, fed by GitHub Releases (no Claudiu-operated server).
//!
//! * Only active when the app was installed by the Velopack installer; dev builds and portable
//!   copies without an install silently skip it.
//! * The only network request is the public GitHub Releases API call Velopack makes. No identifiers
//!   or usage data are sent. Set `settings.update_url` to `""` in state.json to turn off background
//!   checks; Check now in Settings still looks once, at the default feed.

use std::time::Duration;

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

/// Look for a newer release in the background, and again every six hours while the app stays open.
pub fn check(ws: &mut Workspace) {
    ws.rt.after(Duration::from_secs(6 * 3600), check);
    let Some(url) = repo_url(ws) else { return };
    look(ws, url, false);
}

/// The Check now button in Settings: one look, made even with background checks off, that says what it found.
pub fn check_now(ws: &mut Workspace) {
    let url = repo_url(ws).unwrap_or_else(|| DEFAULT_REPO.to_string());
    look(ws, url, true);
}

fn look(ws: &mut Workspace, url: String, asked: bool) {
    // Once an update is downloading or downloaded, a re-check would only throw that away.
    let busy = match &ws.update.status {
        Status::Checking => Some(None),
        Status::Downloading => Some(Some("An update is downloading".to_string())),
        Status::Ready { version } => Some(Some(format!("Claudiu {version} is ready. Restart to update."))),
        _ => None,
    };
    if let Some(said) = busy {
        if let Some(text) = said.filter(|_| asked) {
            ws.toast(text, false);
        }
        return;
    }
    let previous = std::mem::replace(&mut ws.update.status, Status::Checking);
    ws.rt.spawn(
        move || {
            let manager = UpdateManager::new(GithubSource::new(&url, None, false), None, None).map_err(|e| match e {
                velopack::Error::NotInstalled(_) => "updates work only in a copy installed with the Claudiu installer".to_string(),
                e => e.to_string(),
            })?;
            let check = manager.check_for_updates().map_err(|e| e.to_string())?;
            Ok::<_, String>((manager, check))
        },
        move |ws: &mut Workspace, result| {
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
            if let Some((text, error)) = said {
                ws.toast(text, error);
            }
        },
    );
}

/// What a check found: a newer release, or that the installed version is the latest.
#[derive(Debug)]
enum Found {
    Newer(String),
    Latest(String),
}

/// The status a check leaves behind, and, when the user asked for the check, what to tell them (text, is an error).
fn outcome(previous: Status, found: Result<Found, String>, asked: bool) -> (Status, Option<(String, bool)>) {
    let (status, said) = match found {
        Ok(Found::Newer(version)) => (Status::Available { version: version.clone() }, (format!("Claudiu {version} is available"), false)),
        Ok(Found::Latest(installed)) => (Status::UpToDate, (format!("Claudiu {installed} is the latest version"), false)),
        // A failed re-check (e.g. offline) keeps an update already offered.
        Err(e) if matches!(previous, Status::Available { .. }) => (previous, (format!("Could not check for updates: {e}"), true)),
        // Not installed via Velopack (dev build / portable): nothing to do, and nothing to show unless asked.
        Err(e) => (Status::Idle, (format!("Could not check for updates: {e}"), true)),
    };
    (status, asked.then_some(said))
}

/// Download (first click) then apply and restart (second click).
pub fn install(ws: &mut Workspace) {
    let (Some(manager), Some(info)) = (ws.update.manager.clone(), ws.update.info.clone()) else { return };
    match ws.update.status.clone() {
        Status::Available { version } => {
            ws.update.status = Status::Downloading;
            ws.rt.spawn(
                move || manager.download_updates(&info, None).map_err(|e| e.to_string()),
                move |ws: &mut Workspace, result| {
                    ws.update.status = match result {
                        Ok(()) => Status::Ready { version },
                        Err(e) => Status::Error(e),
                    };
                    if let Status::Error(e) = &ws.update.status {
                        let msg = format!("Update failed: {e}");
                        ws.toast(msg, true);
                    }
                },
            );
        }
        Status::Ready { .. } => {
            // Persist state, then hand over to the updater; it restarts Claudiu into the new version.
            ws.finalize();
            if let Err(e) = manager.apply_updates_and_restart(&info) {
                ws.toast(format!("Could not apply update: {e}"), true);
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
        assert_eq!(outcome(Status::Idle, Ok(Found::Newer("9.9.9".into())), true), (available, Some(("Claudiu 9.9.9 is available".into(), false))));
        let latest = "Claudiu 1.2.3 is the latest version".to_string();
        assert_eq!(outcome(Status::Idle, Ok(Found::Latest("1.2.3".into())), true), (Status::UpToDate, Some((latest, false))));
        assert_eq!(outcome(Status::Idle, Err("offline".into()), false), (Status::Idle, None));
        assert_eq!(outcome(Status::Idle, Err("offline".into()), true), (Status::Idle, Some(("Could not check for updates: offline".into(), true))));
    }

    #[test]
    fn a_failed_check_keeps_an_update_already_offered() {
        let available = Status::Available { version: "9.9.9".into() };
        assert_eq!(outcome(available.clone(), Err("offline".into()), false), (available.clone(), None));
        assert_eq!(outcome(available.clone(), Err("offline".into()), true).0, available);
    }
}
