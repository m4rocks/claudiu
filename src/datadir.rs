//! m4code's own data directory and the small file helpers every module that writes into it shares.

use std::path::{Path, PathBuf};

pub fn data_dir() -> PathBuf {
    crate::store::Store::default_path().parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Points m4code's data folder, and Claude Code's, at throwaway ones for the rest of the test run, and returns them.
/// Set once and never changed: other tests read them from their own threads at any time.
#[cfg(test)]
pub fn use_throwaway_folders() -> (PathBuf, PathBuf) {
    static DIRS: std::sync::OnceLock<(tempfile::TempDir, tempfile::TempDir)> = std::sync::OnceLock::new();
    let (data, claude) = DIRS.get_or_init(|| {
        let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // SAFETY: set once, before the tests that need it start threads of their own.
        unsafe {
            std::env::set_var("M4CODE_DATA_DIR", dirs.0.path());
            std::env::set_var("CLAUDE_CONFIG_DIR", dirs.1.path());
            // Keeps the sidebar's Codex limits from reading the real `~/.codex`.
            std::env::set_var("CODEX_HOME", dirs.0.path().join("codex"));
        }
        dirs
    });
    (dunce::canonicalize(data.path()).unwrap(), dunce::canonicalize(claude.path()).unwrap())
}

/// The folders an [`own_folder`] guard holds: m4code's data folder, and optionally a folder whose programs
/// `platform::find_executable` finds first. Each wins over its environment variable while the guard lives. An adapter
/// that reads a folder of its own takes the guard to override it, as `harness::claude::own_claude_home` does.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct Overrides {
    pub data: Option<PathBuf>,
    pub programs: Option<PathBuf>,
}

#[cfg(test)]
static OVERRIDES: std::sync::RwLock<Overrides> = std::sync::RwLock::new(Overrides { data: None, programs: None });

#[cfg(test)]
pub(crate) fn overrides() -> Overrides {
    OVERRIDES.read().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
fn set_overrides(change: impl FnOnce(&mut Overrides)) {
    change(&mut OVERRIDES.write().unwrap_or_else(|e| e.into_inner()));
}

/// A data folder of a test's own, for headless tests that need a fresh `state.json`. It never changes
/// `M4CODE_DATA_DIR`, which the unit tests share (#149), nor any other environment variable, but every lookup in this
/// process uses it while the guard lives. So it only runs in a binary started with `--ignored`, where no unit test runs
/// beside it, and two such tests take turns.
#[cfg(test)]
pub fn own_folder() -> OwnFolder {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let ignored_only = std::env::args().any(|a| a == "--ignored");
    assert!(ignored_only, "a test with its own data folder runs only with `cargo test -- --ignored`, apart from the unit tests");
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    set_overrides(|o| *o = Overrides { data: Some(dir.path().to_path_buf()), ..Default::default() });
    OwnFolder { dir, _turn: turn }
}

#[cfg(test)]
pub struct OwnFolder {
    dir: tempfile::TempDir,
    _turn: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl OwnFolder {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Finds programs in `dir` before the PATH.
    pub fn with_programs_from(self, dir: PathBuf) -> Self {
        set_overrides(|o| o.programs = Some(dir));
        self
    }
}

#[cfg(test)]
impl Drop for OwnFolder {
    fn drop(&mut self) {
        set_overrides(|o| *o = Overrides::default());
    }
}

pub fn safe_file_stem(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, text)?;
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// Takes the data folder for this process: the lock lasts as long as the returned file is open. `Ok(None)` when another
/// m4code holds it, so two apps never race on `state.json` and the request files. It waits up to `patience` first,
/// because an update or restart starts the new app while the old one is still closing.
pub fn lock(dir: &Path, patience: std::time::Duration) -> std::io::Result<Option<std::fs::File>> {
    std::fs::create_dir_all(dir)?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("m4code.lock"))?;
    let start = std::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < patience => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// Remove the files in `dir` that have not been modified for `max_age_secs`. The caller decides the age.
pub fn prune(dir: PathBuf, max_age_secs: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > max_age_secs);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_app_cannot_take_a_data_folder_until_the_first_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let none = std::time::Duration::ZERO;
        let first = lock(dir.path(), none).unwrap().expect("the first app takes the folder");
        assert!(lock(dir.path(), none).unwrap().is_none(), "a second app finds it taken");
        let other = tempfile::tempdir().unwrap();
        assert!(lock(other.path(), none).unwrap().is_some(), "another data folder is free");
        drop(first);
        assert!(lock(dir.path(), none).unwrap().is_some(), "free again once the first app is gone");
    }

    #[test]
    fn session_ids_cannot_escape_the_directory() {
        assert_eq!(safe_file_stem("../../evil"), "______evil");
        assert_eq!(safe_file_stem("6829d7a1-9e3d_47ca"), "6829d7a1-9e3d_47ca");
    }
}
