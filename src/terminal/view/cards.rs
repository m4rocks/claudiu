//! The model's cards, painted live over rows Claude Code reserves in its transcript.
//!
//! The mod draws one row of a hidden link per card row, `claudiu://visual/<call>/<row of the card>`. Claude Code owns
//! those rows, so they scroll and clip with the transcript; each frame this finds the ones on screen and paints that
//! part of the card over them. The card's measured height goes back to the mod, which reserves that many rows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rune::{El, el};

use super::*;
use crate::app::Workspace;
use crate::visual::Card;
use crate::visual::ui::{CardUi, Scope};

/// What the links on reserved rows start with.
pub(super) const INLINE_LINK: &str = "claudiu://visual/";

/// The mod's links cover this many columns. Past them, only Claude Code's own overlays draw on a reserved row.
const LINK_COLS: usize = 16;
/// Columns between the terminal's edge and a card, where the transcript's text starts.
const MARGIN_COLS: f32 = 2.0;
const MAX_WIDTH: f32 = 720.0;

/// The part of a card on screen: `count` grid rows from `row`, showing the card from its row `first`.
#[derive(Debug, PartialEq)]
struct Slot {
    call: String,
    row: usize,
    first: usize,
    count: usize,
}

struct Shown {
    card: Card,
    rows_file: PathBuf,
    /// The height last told to the mod.
    rows: Option<usize>,
}

#[derive(Default)]
pub(super) struct Cards {
    shown: HashMap<String, Shown>,
    pub(super) ui: CardUi,
    /// The height each card last wanted, read by the write that runs next, so a slow older write cannot leave its
    /// height in the file after a newer one.
    wanted_rows: Arc<Mutex<HashMap<String, usize>>>,
    /// Held while a height is written, so writes to the file run one after another.
    writing_rows: Arc<Mutex<()>>,
}

/// The call and card row a reserved row's link names.
fn parse_link(uri: &str) -> Option<(&str, usize)> {
    let (call, row) = uri.strip_prefix(INLINE_LINK)?.rsplit_once('/')?;
    Some((call, row.parse().ok()?))
}

/// Joins the reserved rows of the screen, top to bottom, into the part of each card they show.
fn slots(rows: impl Iterator<Item = Option<(String, usize)>>) -> Vec<Slot> {
    let mut out: Vec<Slot> = vec![];
    for (y, reserved) in rows.enumerate() {
        let Some((call, first)) = reserved else { continue };
        match out.last_mut() {
            Some(s) if s.call == call && s.row + s.count == y => s.count += 1,
            _ => out.push(Slot { call, row: y, first, count: 1 }),
        }
    }
    out
}

impl TerminalView {
    /// A card arrived from the mod, or changed. Its height goes to `rows_file`.
    pub fn set_card(&mut self, card: Card, rows_file: PathBuf) {
        let rows = self.cards.shown.get(&card.call).and_then(|s| s.rows);
        self.cards.shown.insert(card.call.clone(), Shown { card, rows_file, rows });
        self.dirty = true;
    }

    #[cfg(test)]
    pub fn has_card(&self, call: &str) -> bool {
        self.cards.shown.contains_key(call)
    }

    /// What the user has done in this terminal's cards.
    pub fn card_ui(&mut self) -> &mut CardUi {
        self.dirty = true;
        &mut self.cards.ui
    }

    /// A card's button: its text goes out as the user's next message. It counts as sent only once it could go.
    pub fn card_send(&mut self, key: &str, text: String) {
        if self.is_running() {
            self.cards.ui.mark_sent(key);
        }
        self.events.push(TerminalEvent::SendText(text));
        self.dirty = true;
    }

    fn visible_slots(&self) -> Vec<Slot> {
        let term = self.terminal.term.lock();
        let grid = term.grid();
        let offset = grid.display_offset() as i32;
        slots((0..grid.screen_lines()).map(|y| {
            let row = &grid[Line(y as i32 - offset)];
            let link = row[Column(0)].hyperlink()?;
            let (call, first) = parse_link(link.uri())?;
            // Claude Code's "Jump to bottom" is drawn on top of the transcript: that row stays its own.
            let covered = (LINK_COLS..grid.columns()).any(|c| !matches!(row[Column(c)].c, ' ' | '\0'));
            (!covered).then(|| (call.to_string(), first))
        }))
    }

    /// The cards on screen, each clipped to the rows it has there. The mouse stops at them, but the wheel still scrolls
    /// the transcript (a card takes no wheel, so it goes on to the terminal behind).
    pub(super) fn inline_cards(&self, m: Metrics, shift: f32) -> Vec<El> {
        if self.cards.shown.is_empty() {
            return vec![];
        }
        let width = (m.cell_w * self.terminal.size().0.saturating_sub(4) as f32).min(MAX_WIDTH);
        let margin = m.cell_w * MARGIN_COLS;
        let line_h = m.line_h;
        let mut out = vec![];
        for slot in self.visible_slots() {
            let Some(Shown { card, .. }) = self.cards.shown.get(&slot.call) else { continue };
            let scope = Scope { call: card.call.clone(), terminal: self.key.clone() };
            let (call, term) = (card.call.clone(), self.key.clone());
            let clear = self.key.clone();
            let body = el()
                .absolute(margin, -(line_h * slot.first as f32))
                .w(width)
                .on_layout(move |ws: &mut Workspace, r: [f32; 4]| {
                    let rows = (r[3] / line_h).ceil() as usize;
                    if let Some(t) = ws.terminal_mut(&term) {
                        t.note_card_rows(&call, rows);
                    }
                })
                .child(crate::visual::ui::markdown(&format!("{}/card-{}", self.key, card.call), &card.content, &scope, &self.cards.ui));
            out.push(
                el().key(format!("{}/slot-{}-{}", self.key, slot.call, slot.first))
                    // Follows the scroll at once, as the rows around it do.
                    .instant_changes()
                    .absolute(0.0, line_h * slot.row as f32 + shift)
                    .w(width + margin)
                    .h(line_h * slot.count as f32)
                    .clip_rounded(0.0)
                    // A click on a card never reaches the terminal behind it, so its older selection is dropped here:
                    // Ctrl+C copies the terminal's selection first, and would otherwise copy that instead of the card
                    // text now being selected.
                    .on_press(move |ws: &mut Workspace| {
                        if let Some(t) = ws.terminal_mut(&clear)
                            && t.terminal.term.lock().selection.take().is_some()
                        {
                            t.dirty = true;
                        }
                    })
                    .child(body),
            );
        }
        out
    }

    /// Tell the mod a card's height when it changes. The file is written off the UI thread.
    fn note_card_rows(&mut self, call: &str, rows: usize) {
        let Some(shown) = self.cards.shown.get_mut(call).filter(|s| s.rows != Some(rows) && rows > 0) else { return };
        shown.rows = Some(rows);
        let file = shown.rows_file.clone();
        let call = call.to_string();
        let wanted = self.cards.wanted_rows.clone();
        let writing = self.cards.writing_rows.clone();
        wanted.lock().unwrap_or_else(|e| e.into_inner()).insert(call.clone(), rows);
        std::thread::spawn(move || {
            let _one_at_a_time = writing.lock().unwrap_or_else(|e| e.into_inner());
            let latest = wanted.lock().unwrap_or_else(|e| e.into_inner()).get(&call).copied();
            if let Some(latest) = latest {
                let _ = crate::datadir::write_atomic(&file, &latest.to_string());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_names_its_call_and_row() {
        assert_eq!(parse_link("claudiu://visual/toolu_1/12"), Some(("toolu_1", 12)));
        assert_eq!(parse_link("claudiu://visual/toolu_1"), None);
        assert_eq!(parse_link("claudiu://visual/toolu_1/x"), None);
        assert_eq!(parse_link("https://example.com/1/2"), None);
    }

    #[test]
    fn rows_join_into_the_visible_part_of_each_card() {
        let row = |call: &str, n| Some((call.to_string(), n));
        // Card a is clipped at the top, its row 4 is covered by an overlay, and card b follows without a gap.
        let screen = vec![None, row("a", 2), row("a", 3), None, row("a", 5), row("b", 0), row("b", 1)];
        assert_eq!(
            slots(screen.into_iter()),
            [
                Slot { call: "a".into(), row: 1, first: 2, count: 2 },
                Slot { call: "a".into(), row: 4, first: 5, count: 1 },
                Slot { call: "b".into(), row: 5, first: 0, count: 2 },
            ]
        );
    }
}
