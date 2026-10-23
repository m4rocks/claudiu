//! The Visual panel: Markdown (and the `ui` dialect, `ui/`) the model draws beside the terminal.
//!
//! The model calls Claudiu's `show_visual` tool; the per-process mod answers it in-process and, while the model is
//! still writing the call, streams what it has so far into `<data>/visual/<record id>.json` (`mod/hooks/register.js`).
//! Claudiu polls that folder and keeps a stack of cards per session. Nothing goes through the PTY.

pub mod ui;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::Deserialize;

/// Env var holding the file the mod writes the card being drawn into.
pub const VISUAL_ENV: &str = "CLAUDIU_VISUAL_FILE";

/// Most cards kept per session; the oldest go first.
pub const MAX_CARDS: usize = 20;

pub fn dir() -> PathBuf {
    crate::datadir::data_dir().join("visual")
}

/// The card file of one session, named by Claudiu's own record id (which survives `/clear`).
pub fn file(record_id: &str) -> PathBuf {
    dir().join(format!("{}.json", crate::datadir::safe_file_stem(record_id)))
}

/// One `show_visual` call as the mod writes it: the same `call` until it is finished, `done` once it is.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Card {
    pub call: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub done: bool,
}

/// What a file looked like when it was last read.
pub type Stamp = (SystemTime, u64);

/// The cards that changed since `seen` (file stem and card), updating `seen`. Half-written or unreadable files are
/// left for the next look.
pub fn read_changed(dir: &std::path::Path, seen: &mut HashMap<String, Stamp>) -> Vec<(String, Card)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut out = vec![];
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
        let Ok(meta) = e.metadata() else { continue };
        let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
        if seen.get(&stem) == Some(&stamp) {
            continue;
        }
        let Some(card) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Card>(&t).ok()) else { continue };
        seen.insert(stem.clone(), stamp);
        out.push((stem, card));
    }
    out
}

/// Add a card, or replace the one with the same `call` (the stream of one tool call), keeping at most `MAX_CARDS`.
/// Returns whether the call is new to the stack.
pub fn upsert(cards: &mut Vec<Card>, card: Card) -> bool {
    if let Some(slot) = cards.iter_mut().find(|c| c.call == card.call) {
        *slot = card;
        return false;
    }
    cards.push(card);
    let extra = cards.len().saturating_sub(MAX_CARDS);
    cards.drain(..extra);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(call: &str, content: &str, done: bool) -> Card {
        Card { call: call.into(), title: String::new(), content: content.into(), done }
    }

    #[test]
    fn a_call_replaces_its_own_card_and_the_stack_is_capped() {
        let mut cards = vec![];
        assert!(upsert(&mut cards, card("a", "# Hi", false)));
        assert!(!upsert(&mut cards, card("a", "# Hi there", true)));
        assert_eq!(cards, vec![card("a", "# Hi there", true)]);
        for i in 0..30 {
            upsert(&mut cards, card(&format!("c{i}"), "x", true));
        }
        assert_eq!(cards.len(), MAX_CARDS);
        assert_eq!(cards.last().unwrap().call, "c29");
        assert!(cards.iter().all(|c| c.call != "a"));
    }

    #[test]
    fn only_changed_files_are_read_again_and_garbage_is_left_for_later() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("one.json"), r##"{"call":"t1","title":"T","content":"# A","done":false}"##).unwrap();
        std::fs::write(tmp.path().join("bad.json"), r#"{"call":"#).unwrap();
        std::fs::write(tmp.path().join("note.txt"), "x").unwrap();
        let mut seen = HashMap::new();
        let got = read_changed(tmp.path(), &mut seen);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].0.as_str(), got[0].1.title.as_str(), got[0].1.done), ("one", "T", false));
        assert!(read_changed(tmp.path(), &mut seen).is_empty(), "unchanged");

        std::fs::write(tmp.path().join("one.json"), r##"{"call":"t1","title":"T","content":"# AB","done":true}"##).unwrap();
        std::fs::write(tmp.path().join("bad.json"), r#"{"call":"t2","content":"ok","done":true}"#).unwrap();
        let mut got = read_changed(tmp.path(), &mut seen);
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(got.iter().map(|(s, c)| (s.as_str(), c.done)).collect::<Vec<_>>(), [("bad", true), ("one", true)]);
    }
}
