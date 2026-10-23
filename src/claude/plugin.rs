//! The mod Claudiu ships (`mod/` in the repository), embedded in the binary and written to the data folder so every
//! Claude session can load it with `--plugin-dir`.

use std::path::PathBuf;

/// The mod that answers the MCP server's tools in-process (see `mod/`), written to the data dir and loaded per
/// process through `--plugin-dir`. Files are only rewritten when they changed: a rewrite reloads running sessions' mods.
pub fn install() -> std::io::Result<PathBuf> {
    let dir = crate::datadir::data_dir().join("mod");
    for (rel, text) in [
        (".claude-plugin/plugin.json", include_str!("../../mod/.claude-plugin/plugin.json")),
        ("hooks/hooks.json", include_str!("../../mod/hooks/hooks.json")),
        ("hooks/register.js", include_str!("../../mod/hooks/register.js")),
    ] {
        let path = dir.join(rel);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            crate::datadir::write_atomic(&path, text)?;
        }
    }
    Ok(dir)
}
