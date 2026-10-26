//! Alt accounts. Each is a Claude Code config folder (`CLAUDE_CONFIG_DIR`) inside Claudiu's data dir that holds
//! only its own login (`.credentials.json`, `.claude.json`). Settings, `CLAUDE.md` and keybindings are copied from
//! the main `~/.claude` before each launch; folders (agents, skills, plugins, session history) link back to it.
//! The main account is the user's normal Claude Code: launched without `CLAUDE_CONFIG_DIR`. Claudiu never writes
//! to its folder.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Entries that belong to one login and are never shared.
const OWN: &[&str] = &[".credentials.json", "backups", "daemon", "daemon.log"];

/// Top-level files copied from the main folder into each alt, one way.
const SYNCED_FILES: &[&str] = &["settings.json", "CLAUDE.md", "keybindings.json"];

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

/// A new alt folder with the shared entries already in place.
pub fn create(main: &Path) -> io::Result<PathBuf> {
    let dir = root().join(crate::store::new_id());
    fs::create_dir_all(&dir)?;
    sync_shared(main, &dir)?;
    Ok(dir)
}

/// Bring `alt` up to date with `main`, one way; nothing is ever written to `main`. Runs before each launch, so
/// entries Claude Code adds to the main folder later are picked up too. Returns the last error but does what it can.
/// - Config files (`SYNCED_FILES`) are copied, so an alt session that saves one changes only its own copy. The copy
///   replaces the alt's file every time: it may still be a leftover hard link to main's file, and writing through
///   that would change main.
/// - Folders are linked to main's, so skills, agents, plugins and session history are the same for every account.
/// - Anything else at the top level (command history, caches, the server-pushed limits) is the alt's own.
pub fn sync_shared(main: &Path, alt: &Path) -> io::Result<()> {
    let mut result = Ok(());
    for name in SYNCED_FILES {
        if let Err(e) = copy_config_file(&main.join(name), &alt.join(name)) {
            result = Err(e);
        }
    }
    for e in fs::read_dir(main)?.flatten() {
        let name = e.file_name();
        let src = e.path();
        if is_own(&name) || !src.is_dir() {
            continue;
        }
        let dst = alt.join(&name);
        // shortcut: a folder the alt already has (e.g. one Claude Code made first) is left alone.
        if fs::symlink_metadata(&dst).is_ok() {
            continue;
        }
        if let Err(e) = link_dir(&src, &dst) {
            result = Err(e);
        }
    }
    result
}

/// Make `dst` a plain copy of `src`; when `src` is gone, remove `dst`. Whatever the alt changed in its copy is
/// replaced: the alt follows main, never the other way.
fn copy_config_file(src: &Path, dst: &Path) -> io::Result<()> {
    let bytes = match fs::read(src) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return match fs::remove_file(dst) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        // Unreadable for now (say, locked while Claude Code saves it): keep what the alt has.
        Err(e) => return Err(e),
    };
    // Written beside the target and renamed over it, which replaces the directory entry and never writes through a
    // link: `dst` may still be a hard link to main's file from an earlier version of Claudiu.
    let tmp = dst.with_extension("claudiu-tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, dst).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(windows)]
fn link_dir(src: &Path, dst: &Path) -> io::Result<()> {
    // Junctions need no admin rights or Developer Mode, unlike directory symlinks.
    junction::create(src, dst)
}

#[cfg(unix)]
fn link_dir(src: &Path, dst: &Path) -> io::Result<()> {
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
    fn alt_shares_folders_copies_config_and_keeps_its_own_login_and_removal_keeps_main_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, alt) = (tmp.path().join("main"), tmp.path().join("alt"));
        fs::create_dir_all(main.join("projects/p")).unwrap();
        fs::create_dir_all(&alt).unwrap();
        for f in ["settings.json", "history.jsonl", ".credentials.json", ".claude.json", ".claude.json.backup"] {
            fs::write(main.join(f), f).unwrap();
        }
        sync_shared(&main, &alt).unwrap();
        assert_eq!(fs::read_to_string(alt.join("settings.json")).unwrap(), "settings.json");
        assert!(!alt.join("history.jsonl").exists(), "other top-level files are the alt's own");
        assert!(!alt.join(".credentials.json").exists() && !alt.join(".claude.json").exists() && !alt.join(".claude.json.backup").exists());

        // Folders are shared: writes through the alt land in main.
        fs::write(alt.join("projects/p/s.jsonl"), "x").unwrap();
        assert!(main.join("projects/p/s.jsonl").is_file());

        // The alt's own login stays its own; syncing again is harmless.
        fs::write(alt.join(".credentials.json"), "alt").unwrap();
        sync_shared(&main, &alt).unwrap();
        assert_eq!(fs::read_to_string(alt.join(".credentials.json")).unwrap(), "alt");

        delete(&alt).unwrap();
        assert!(!alt.exists());
        assert!(main.join("projects/p/s.jsonl").is_file(), "removing an alt never touches main");
        assert_eq!(fs::read_to_string(main.join("settings.json")).unwrap(), "settings.json");
        assert_eq!(fs::read_to_string(main.join(".credentials.json")).unwrap(), ".credentials.json");
    }

    #[test]
    fn config_flows_from_main_to_the_alt_and_never_back() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, alt) = (tmp.path().join("main"), tmp.path().join("alt"));
        fs::create_dir_all(&main).unwrap();
        fs::create_dir_all(&alt).unwrap();
        fs::write(main.join("settings.json"), "main v1").unwrap();
        // What the old scheme left behind: the alt's file is a hard link to main's.
        fs::hard_link(main.join("settings.json"), alt.join("settings.json")).unwrap();

        sync_shared(&main, &alt).unwrap();
        fs::write(alt.join("settings.json"), "alt edit").unwrap();
        assert_eq!(fs::read_to_string(main.join("settings.json")).unwrap(), "main v1", "an edit in the alt must not reach main");

        // The next launch replaces the alt's edit with main's file.
        sync_shared(&main, &alt).unwrap();
        assert_eq!(fs::read_to_string(alt.join("settings.json")).unwrap(), "main v1");

        // Main changed, even by replacing the file: the alt follows.
        fs::write(main.join("settings.json.new"), "main v2").unwrap();
        fs::rename(main.join("settings.json.new"), main.join("settings.json")).unwrap();
        sync_shared(&main, &alt).unwrap();
        assert_eq!(fs::read_to_string(alt.join("settings.json")).unwrap(), "main v2");
    }

    #[test]
    fn a_main_file_that_cannot_be_read_leaves_the_alts_copy_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, alt) = (tmp.path().join("main"), tmp.path().join("alt"));
        fs::create_dir_all(main.join("settings.json")).unwrap(); // reading a folder fails, like a locked file
        fs::create_dir_all(&alt).unwrap();
        fs::write(alt.join("settings.json"), "alt").unwrap();
        assert!(sync_shared(&main, &alt).is_err());
        assert_eq!(fs::read_to_string(alt.join("settings.json")).unwrap(), "alt");
        assert!(!alt.join("settings.claudiu-tmp").exists());
    }

    #[test]
    fn a_config_file_removed_from_main_is_removed_from_the_alt() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, alt) = (tmp.path().join("main"), tmp.path().join("alt"));
        fs::create_dir_all(&main).unwrap();
        fs::create_dir_all(&alt).unwrap();
        fs::write(main.join("CLAUDE.md"), "rules").unwrap();
        sync_shared(&main, &alt).unwrap();
        assert!(alt.join("CLAUDE.md").is_file());
        fs::remove_file(main.join("CLAUDE.md")).unwrap();
        sync_shared(&main, &alt).unwrap();
        assert!(!alt.join("CLAUDE.md").exists());
    }

    #[test]
    fn remove_refuses_folders_outside_the_accounts_root() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(remove(tmp.path()).is_err());
        assert!(remove(&root()).is_err());
        assert!(tmp.path().exists());
    }
}
