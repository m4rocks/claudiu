//! Alt accounts. Each is a Claude Code config folder (`CLAUDE_CONFIG_DIR`) inside Claudiu's data dir that holds
//! only its own login (`.credentials.json`, `.claude.json`); every other entry links back to the main
//! `~/.claude`, so settings, instructions, agents, skills, plugins and session history are shared.
//! The main account is the user's normal Claude Code: launched without `CLAUDE_CONFIG_DIR`, never written to.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Entries that belong to one login and are never shared.
const OWN: &[&str] = &[".credentials.json", "backups", "daemon", "daemon.log"];

pub fn root() -> PathBuf {
    crate::datadir::data_dir().join("accounts")
}

/// Every alt folder on disk.
pub fn folders() -> Vec<PathBuf> {
    fs::read_dir(root()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()
}

fn is_own(name: &OsStr) -> bool {
    let n = name.to_string_lossy();
    OWN.contains(&n.as_ref()) || n.starts_with(".claude.json")
}

/// A new, empty alt folder with the shared entries already linked.
pub fn create(main: &Path) -> io::Result<PathBuf> {
    let dir = root().join(crate::store::new_id());
    fs::create_dir_all(&dir)?;
    link_shared(main, &dir)?;
    Ok(dir)
}

/// Link every shared entry of `main` that `alt` doesn't have yet. Runs before each launch, so entries Claude
/// Code adds to the main folder later are picked up too. Returns the last error but links what it can.
pub fn link_shared(main: &Path, alt: &Path) -> io::Result<()> {
    let mut result = Ok(());
    for e in fs::read_dir(main)?.flatten() {
        let name = e.file_name();
        let dst = alt.join(&name);
        let src = e.path();
        if is_own(&name) {
            continue;
        }
        if let Ok(meta) = fs::symlink_metadata(&dst) {
            // A hard-linked file that a program saved by replacing it is now a separate copy: rejoin it.
            if meta.is_file() && !meta.file_type().is_symlink() && src.is_file() {
                let r = rejoin(&src, &dst);
                if r.is_err() {
                    result = r;
                }
            }
            // shortcut: any other entry the alt already has (e.g. a folder Claude Code made first) is left alone.
            continue;
        }
        let r = if src.is_dir() { link_dir(&src, &dst) } else { link_file(&src, &dst) };
        if r.is_err() {
            result = r;
        }
    }
    result
}

/// The newer of the two copies wins: an alt's change is written into the main file (in place, so other
/// hard links to it see it), then the alt's copy is replaced by a link to main again.
/// The losing copy is kept in the alt's own `backups` folder first.
fn rejoin(main: &Path, alt: &Path) -> io::Result<()> {
    let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified());
    let (m, a) = (mtime(main)?, mtime(alt)?);
    if m == a {
        return Ok(()); // still one file (or identical): nothing to do
    }
    let (loser, side) = if a > m { (main, "main") } else { (alt, "alt") };
    let name = main.file_name().unwrap_or_default().to_string_lossy();
    let backups = alt.parent().unwrap_or(alt).join("backups");
    fs::create_dir_all(&backups)?;
    fs::copy(loser, backups.join(format!("{name}.{side}.{}.bak", crate::store::now())))?;
    if a > m {
        fs::copy(alt, main)?;
    }
    fs::remove_file(alt)?;
    link_file(main, alt)
}

#[cfg(windows)]
fn link_dir(src: &Path, dst: &Path) -> io::Result<()> {
    // Junctions need no admin rights or Developer Mode, unlike directory symlinks.
    junction::create(src, dst)
}

#[cfg(windows)]
fn link_file(src: &Path, dst: &Path) -> io::Result<()> {
    // shortcut: without Developer Mode file symlinks fail and a hard link is used; a program that saves the main
    // file by replacing it leaves the alt on the old copy until the alt is re-created.
    std::os::windows::fs::symlink_file(src, dst).or_else(|_| fs::hard_link(src, dst))
}

#[cfg(unix)]
fn link_dir(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(unix)]
fn link_file(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

/// Delete an alt folder: its login plus the links, never what they point at. Refuses anything outside `root()`.
pub fn remove(alt: &Path) -> io::Result<()> {
    if !alt.starts_with(root()) || alt == root() {
        return Err(io::Error::other("not a Claudiu account folder"));
    }
    delete(alt)
}

fn delete(alt: &Path) -> io::Result<()> {
    for e in fs::read_dir(alt)?.flatten() {
        // `DirEntry::file_type` doesn't follow links; on Windows junctions count as symlinks too.
        if e.file_type()?.is_symlink() {
            let p = e.path();
            // Windows removes directory links with RemoveDirectory; Unix removes any symlink as a file.
            if cfg!(windows) && p.is_dir() { fs::remove_dir(&p)? } else { fs::remove_file(&p)? }
        }
    }
    // Hard links left over are just extra names: removing them leaves the main file in place.
    fs::remove_dir_all(alt)
}

/// What a Claude process for the alt at `dir` is started with. `CLAUDE_SECURESTORAGE_CONFIG_DIR` keys the macOS
/// Keychain entry to the alt too (third-party wrappers report older versions shared one entry otherwise).
pub fn env(dir: &Path) -> Vec<(String, String)> {
    let d = dir.to_string_lossy().into_owned();
    vec![("CLAUDE_CONFIG_DIR".into(), d.clone()), ("CLAUDE_SECURESTORAGE_CONFIG_DIR".into(), d)]
}

#[derive(Debug, Default, Deserialize)]
pub struct AuthStatus {
    #[serde(rename = "loggedIn", default)]
    pub logged_in: bool,
    pub email: Option<String>,
    #[serde(rename = "subscriptionType")]
    pub plan: Option<String>,
}

/// `claude auth status --json` for one account (`dir == None` is the main one). Run off the UI thread.
pub fn status(exe: &Path, dir: Option<&Path>) -> Option<AuthStatus> {
    let mut cmd = crate::claude::command(exe);
    cmd.args(["auth", "status", "--json"]);
    if let Some(d) = dir {
        cmd.envs(env(d));
    }
    // Logged out exits non-zero but still prints the JSON.
    serde_json::from_slice(&cmd.output().ok()?.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_shares_everything_but_the_login_and_removal_keeps_main_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, alt) = (tmp.path().join("main"), tmp.path().join("alt"));
        fs::create_dir_all(main.join("projects/p")).unwrap();
        fs::create_dir_all(&alt).unwrap();
        for f in ["settings.json", ".credentials.json", ".claude.json", ".claude.json.backup"] {
            fs::write(main.join(f), f).unwrap();
        }
        link_shared(&main, &alt).unwrap();
        assert_eq!(fs::read_to_string(alt.join("settings.json")).unwrap(), "settings.json");
        assert!(!alt.join(".credentials.json").exists() && !alt.join(".claude.json").exists() && !alt.join(".claude.json.backup").exists());

        // Writes through the alt land in main.
        fs::write(alt.join("projects/p/s.jsonl"), "x").unwrap();
        assert!(main.join("projects/p/s.jsonl").is_file());

        // The alt's own login stays its own; re-linking is idempotent.
        fs::write(alt.join(".credentials.json"), "alt").unwrap();
        link_shared(&main, &alt).unwrap();

        delete(&alt).unwrap();
        assert!(!alt.exists());
        assert!(main.join("projects/p/s.jsonl").is_file(), "removing an alt never touches main");
        assert_eq!(fs::read_to_string(main.join("settings.json")).unwrap(), "settings.json");
        assert_eq!(fs::read_to_string(main.join(".credentials.json")).unwrap(), ".credentials.json");
    }

    #[test]
    fn a_replaced_file_link_is_rejoined_with_the_newer_content_winning() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, alt) = (tmp.path().join("main"), tmp.path().join("alt"));
        fs::create_dir_all(&main).unwrap();
        fs::create_dir_all(&alt).unwrap();
        fs::write(main.join("settings.json"), "old").unwrap();
        // The alt's copy was saved by replacing it, later than main.
        fs::write(alt.join("settings.json"), "alt edit").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        fs::File::options().write(true).open(alt.join("settings.json")).unwrap().set_modified(later).unwrap();

        link_shared(&main, &alt).unwrap();
        assert_eq!(fs::read_to_string(main.join("settings.json")).unwrap(), "alt edit");
        assert!(fs::read_dir(alt.join("backups")).unwrap().next().is_some(), "main's old copy is backed up");

        // Rejoined: a later write to main shows in the alt.
        fs::write(main.join("settings.json"), "main edit").unwrap();
        assert_eq!(fs::read_to_string(alt.join("settings.json")).unwrap(), "main edit");

        // Main newer: the alt's own copy is backed up before it is replaced.
        fs::remove_file(alt.join("settings.json")).unwrap();
        fs::write(alt.join("settings.json"), "alt only").unwrap();
        let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        fs::File::options().write(true).open(alt.join("settings.json")).unwrap().set_modified(earlier).unwrap();
        link_shared(&main, &alt).unwrap();
        assert_eq!(fs::read_to_string(alt.join("settings.json")).unwrap(), "main edit");
        let saved = fs::read_dir(alt.join("backups")).unwrap().flatten().any(|e| fs::read_to_string(e.path()).unwrap() == "alt only");
        assert!(saved, "the alt's copy is backed up");
    }

    #[test]
    fn remove_refuses_folders_outside_the_accounts_root() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(remove(tmp.path()).is_err());
        assert!(remove(&root()).is_err());
        assert!(tmp.path().exists());
    }
}
