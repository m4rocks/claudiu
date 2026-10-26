//! The mod Claudiu ships (`mod/` in the repository), embedded in the binary and written to the data folder so every
//! Claude session can load it with `--plugin-dir`.

use std::path::{Path, PathBuf};

/// The mod's files: path inside the mod folder, and contents.
const FILES: [(&str, &str); 3] = [
    (".claude-plugin/plugin.json", include_str!("../../mod/.claude-plugin/plugin.json")),
    ("hooks/hooks.json", include_str!("../../mod/hooks/hooks.json")),
    ("hooks/register.js", include_str!("../../mod/hooks/register.js")),
];

/// The mod that answers the MCP server's tools in-process (see `mod/`), written to the data dir and loaded per
/// process through `--plugin-dir`. Each distinct copy of the mod gets its own folder, so two builds of Claudiu (a
/// release and a dev build) never overwrite each other's files: a rewrite reloads the mod in every running session
/// that loaded the folder, including sessions the other build started.
pub fn install() -> std::io::Result<PathBuf> {
    install_into(&crate::datadir::data_dir(), &FILES)
}

fn install_into(data: &Path, files: &[(&str, &str)]) -> std::io::Result<PathBuf> {
    let dir = data.join(folder_name(files));
    for (rel, text) in files {
        let path = dir.join(rel);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*text) {
            crate::datadir::write_atomic(&path, text)?;
        }
    }
    Ok(dir)
}

/// `mod-` and a hash of the files, so equal copies share a folder. FNV-1a, because the name must not change between
/// Rust versions (a std hasher may).
fn folder_name(files: &[(&str, &str)]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (rel, text) in files {
        for byte in rel.bytes().chain([0]).chain(text.bytes()).chain([0]) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("mod-{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_builds_with_different_mods_use_different_folders_and_leave_each_other_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let release = [("hooks/register.js", "release")];
        let dev = [("hooks/register.js", "dev")];
        let release_dir = install_into(tmp.path(), &release).unwrap();
        let dev_dir = install_into(tmp.path(), &dev).unwrap();
        assert_ne!(release_dir, dev_dir);
        assert_eq!(std::fs::read_to_string(release_dir.join("hooks/register.js")).unwrap(), "release");
        assert_eq!(std::fs::read_to_string(dev_dir.join("hooks/register.js")).unwrap(), "dev");
    }

    #[test]
    fn the_same_mod_keeps_its_folder_and_is_not_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let files = [("hooks/register.js", "same")];
        let first = install_into(tmp.path(), &files).unwrap();
        let written = std::fs::metadata(first.join("hooks/register.js")).unwrap().modified().unwrap();
        let again = install_into(tmp.path(), &files).unwrap();
        assert_eq!(first, again);
        assert_eq!(std::fs::metadata(again.join("hooks/register.js")).unwrap().modified().unwrap(), written);
    }

    #[test]
    fn a_changed_path_or_content_changes_the_name() {
        let a = folder_name(&[("a", "bc")]);
        assert_ne!(a, folder_name(&[("ab", "c")]), "the split between path and text counts");
        assert_ne!(a, folder_name(&[("a", "bd")]));
        assert!(a.starts_with("mod-") && a.len() == 4 + 16);
    }
}
