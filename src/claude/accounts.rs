//! Alt accounts. Each is a Claude Code config folder (`CLAUDE_CONFIG_DIR`) inside Claudiu's data dir that holds
//! only its own login (`.credentials.json`, `.claude.json`). Folders (agents, skills, plugins, session history) link
//! back to the main `~/.claude`; settings, `CLAUDE.md`, keybindings, MCP servers and project trust are kept equal in
//! every account by `config_sync`. The main account is the user's normal Claude Code: launched without
//! `CLAUDE_CONFIG_DIR`.

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

/// A new alt folder with the shared entries already in place.
pub fn create(main: &Path) -> io::Result<PathBuf> {
    let dir = root().join(crate::store::new_id());
    fs::create_dir_all(&dir)?;
    link_shared(main, &dir)?;
    Ok(dir)
}

/// Link every folder of `main` that `alt` doesn't have yet, so skills, agents, plugins and session history are the
/// same for every account. Runs before each launch, so folders Claude Code adds to the main one later are picked up
/// too. Files are not linked: `config_sync` copies the shared ones, and the rest belong to each account. Returns the
/// last error but links what it can.
pub fn link_shared(main: &Path, alt: &Path) -> io::Result<()> {
    let mut result = Ok(());
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
    fn alt_links_folders_keeps_its_own_files_and_removal_keeps_main_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, alt) = (tmp.path().join("main"), tmp.path().join("alt"));
        fs::create_dir_all(main.join("projects/p")).unwrap();
        fs::create_dir_all(&alt).unwrap();
        for f in ["settings.json", "history.jsonl", ".credentials.json", ".claude.json"] {
            fs::write(main.join(f), f).unwrap();
        }
        link_shared(&main, &alt).unwrap();
        assert!(!alt.join("settings.json").exists() && !alt.join("history.jsonl").exists(), "files are not linked");
        assert!(!alt.join(".credentials.json").exists() && !alt.join(".claude.json").exists());

        // Folders are shared: writes through the alt land in main.
        fs::write(alt.join("projects/p/s.jsonl"), "x").unwrap();
        assert!(main.join("projects/p/s.jsonl").is_file());

        // The alt's own login stays its own; linking again is harmless.
        fs::write(alt.join(".credentials.json"), "alt").unwrap();
        link_shared(&main, &alt).unwrap();
        assert_eq!(fs::read_to_string(alt.join(".credentials.json")).unwrap(), "alt");

        delete(&alt).unwrap();
        assert!(!alt.exists());
        assert!(main.join("projects/p/s.jsonl").is_file(), "removing an alt never touches main");
        assert_eq!(fs::read_to_string(main.join("settings.json")).unwrap(), "settings.json");
        assert_eq!(fs::read_to_string(main.join(".credentials.json")).unwrap(), ".credentials.json");
    }

    #[test]
    fn remove_refuses_folders_outside_the_accounts_root() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(remove(tmp.path()).is_err());
        assert!(remove(&root()).is_err());
        assert!(tmp.path().exists());
    }
}
