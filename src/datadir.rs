//! Claudiu's own data directory and the small file helpers every module that writes into it shares.

use std::path::{Path, PathBuf};

pub fn data_dir() -> PathBuf {
    crate::store::Store::default_path().parent().map(Path::to_path_buf).unwrap_or_default()
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

/// Drop leftovers: per-session files (MCP configs, names, titles) after a week, usage snapshots after a month.
pub fn cleanup() {
    let prune = |dir: PathBuf, max_age_secs: u64| {
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
    };
    let [mcp_configs, titles, names] = crate::mcp::cleanup_dirs();
    prune(names, 7 * 86_400);
    prune(mcp_configs, 7 * 86_400);
    // Titles are copied into the store on the next tick, so an old file is only a duplicate.
    prune(titles, 7 * 86_400);
    prune(crate::usage::snapshot_dir(), 30 * 86_400);
    // The status-line helper's per-session settings, gone since the mod reports usage; sweep what older builds left.
    let _ = std::fs::remove_dir_all(data_dir().join("statusline"));
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
