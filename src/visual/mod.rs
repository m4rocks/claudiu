//! Cards: Markdown (and the `ui` dialect, `ui/`) the model draws inside the terminal.
//!
//! The model calls Claudiu's `show_visual` tool; the per-process mod answers it in-process and, while the model is
//! still writing the call, streams what it has so far into `<data>/visual/<record id>--<call>.json`
//! (`mod/hooks/register.js`). Claudiu polls that folder and hands each card to its session's terminal, which paints it
//! over rows the mod reserves in the transcript (`terminal/view/cards.rs`). Nothing goes through the PTY.
//!
//! The transcript keeps every call's input, so the mod writes a card again when it next draws the call's row. That is
//! what brings cards back after a restart: these files are a mailbox, not the record.

pub mod ui;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

/// Env var holding the start of the path of every card file of a session: the mod adds `<call>.json`.
pub const VISUAL_ENV: &str = "CLAUDIU_VISUAL_PREFIX";

/// Between a session's record id and a call's id in a file name. Neither contains it.
const SEPARATOR: &str = "--";

pub fn dir() -> PathBuf {
    crate::datadir::data_dir().join("visual")
}

/// What the mod puts a call's id after: named by Claudiu's own record id, which survives `/clear`.
pub fn prefix(record_id: &str) -> PathBuf {
    dir().join(format!("{}{SEPARATOR}", crate::datadir::safe_file_stem(record_id)))
}

/// The file telling the mod how many terminal rows a card needs.
pub fn rows_file(record_stem: &str, call: &str) -> PathBuf {
    dir().join(format!("{record_stem}{SEPARATOR}{}.rows", crate::datadir::safe_file_stem(call)))
}

/// One `show_visual` call as the mod writes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Card {
    pub call: String,
    #[serde(default)]
    pub content: String,
}

/// What a file looked like when it was last read.
pub type Stamp = (SystemTime, u64);

/// The cards that changed since `seen` (their session's file stem and the card), updating `seen`. Half-written or
/// unreadable files are left for the next look.
pub fn read_changed(dir: &Path, seen: &mut HashMap<String, Stamp>) -> Vec<(String, Card)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut out = vec![];
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
        let Some((record, _)) = stem.split_once(SEPARATOR) else { continue };
        let record = record.to_string();
        let Ok(meta) = e.metadata() else { continue };
        let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
        if seen.get(&stem) == Some(&stamp) {
            continue;
        }
        let Some(card) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Card>(&t).ok()) else { continue };
        seen.insert(stem, stamp);
        out.push((record, card));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_call_of_a_session_is_read_and_only_changed_files_again() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("rec--c1.json"), dir.path().join("rec--c2.json"));
        std::fs::write(&one, r##"{"call":"c1","content":"# One"}"##).unwrap();
        std::fs::write(&two, r##"{"call":"c2","content":"# Two"}"##).unwrap();
        let mut seen = HashMap::new();

        let mut first = read_changed(dir.path(), &mut seen);
        first.sort_by(|a, b| a.1.call.cmp(&b.1.call));
        assert_eq!(first.len(), 2, "two calls of one session are two cards");
        assert_eq!((first[0].0.as_str(), first[0].1.content.as_str()), ("rec", "# One"));
        assert!(read_changed(dir.path(), &mut seen).is_empty(), "nothing changed");

        std::fs::write(&one, r##"{"call":"c1","content":"# One, longer"}"##).unwrap();
        let again = read_changed(dir.path(), &mut seen);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].1.content, "# One, longer");
    }

    #[test]
    fn half_written_files_wait_and_other_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let card = dir.path().join("rec--c1.json");
        std::fs::write(&card, "{\"call\":\"c1\",\"cont").unwrap();
        std::fs::write(dir.path().join("rec--c1.rows"), "12").unwrap();
        std::fs::write(dir.path().join("old-format.json"), r#"{"call":"c0"}"#).unwrap();
        let mut seen = HashMap::new();
        assert!(read_changed(dir.path(), &mut seen).is_empty());

        std::fs::write(&card, r#"{"call":"c1","content":"done"}"#).unwrap();
        assert_eq!(read_changed(dir.path(), &mut seen).len(), 1);
    }
}
