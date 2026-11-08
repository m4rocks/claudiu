//! Claudiu's own data directory and the small file helpers every module that writes into it shares.

use std::path::{Path, PathBuf};

pub fn data_dir() -> PathBuf {
    crate::store::Store::default_path().parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Points Claudiu's data folder, and Claude Code's, at throwaway ones for the rest of the test run, and returns them.
/// Set once and never changed: other tests read them from their own threads at any time.
#[cfg(test)]
pub fn use_throwaway_folders() -> (PathBuf, PathBuf) {
    static DIRS: std::sync::OnceLock<(tempfile::TempDir, tempfile::TempDir)> = std::sync::OnceLock::new();
    let (data, claude) = DIRS.get_or_init(|| {
        let dirs = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // SAFETY: set once, before the tests that need it start threads of their own.
        unsafe {
            std::env::set_var("CLAUDIU_DATA_DIR", dirs.0.path());
            std::env::set_var("CLAUDE_CONFIG_DIR", dirs.1.path());
        }
        dirs
    });
    (dunce::canonicalize(data.path()).unwrap(), dunce::canonicalize(claude.path()).unwrap())
}

pub fn safe_file_stem(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
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
    fn session_ids_cannot_escape_the_directory() {
        assert_eq!(safe_file_stem("../../evil"), "______evil");
        assert_eq!(safe_file_stem("6829d7a1-9e3d_47ca"), "6829d7a1-9e3d_47ca");
    }
}
