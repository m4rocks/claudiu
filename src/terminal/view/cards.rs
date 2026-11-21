//! The model's cards, painted live over rows Claude Code reserves in its transcript.
//!
//! The mod draws one row of a hidden link per card row, `claudiu://visual/<call>/<row of the card>`. Claude Code owns
//! those rows, so they scroll and clip with the transcript; each frame this finds the ones on screen and paints that
//! part of the card over them. The card's measured height goes back to the mod, which reserves that many rows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::*;
use crate::visual::Card;
use crate::visual::ui::{CardUi, Scope, SendText};

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

pub(super) struct Cards {
    shown: HashMap<String, Shown>,
    ui: Entity<CardUi>,
    /// The height each card last wanted, read by the write that runs next, so a slow older write cannot leave its
    /// height in the file after a newer one.
    wanted_rows: Arc<Mutex<HashMap<String, usize>>>,
    /// Held while a height is written, so writes to the file run one after another.
    writing_rows: Arc<Mutex<()>>,
}

impl Cards {
    /// Button clicks reach the session as `TerminalEvent::SendText`. A click only counts as sent once it could go.
    pub(super) fn new(cx: &mut Context<TerminalView>) -> Self {
        let ui = cx.new(|_| CardUi::default());
        cx.observe(&ui, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&ui, |view, ui, e: &SendText, cx| {
            if view.is_running() {
                ui.update(cx, |ui, cx| ui.mark_sent(&e.key, cx));
            }
            cx.emit(TerminalEvent::SendText(e.text.clone()));
        })
        .detach();
        Self { shown: HashMap::new(), ui, wanted_rows: Arc::default(), writing_rows: Arc::default() }
    }
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
    pub fn set_card(&mut self, card: Card, rows_file: PathBuf, cx: &mut Context<Self>) {
        let rows = self.cards.shown.get(&card.call).and_then(|s| s.rows);
        self.cards.shown.insert(card.call.clone(), Shown { card, rows_file, rows });
        cx.notify();
    }

    #[cfg(test)]
    pub fn has_card(&self, call: &str) -> bool {
        self.cards.shown.contains_key(call)
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
    /// the transcript.
    pub(super) fn inline_cards(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let Some(m) = self.metrics.filter(|_| !self.cards.shown.is_empty()) else { return vec![] };
        let width = (m.cell_w * self.terminal.size().0.saturating_sub(4) as f32).min(px(MAX_WIDTH));
        let margin = m.cell_w * MARGIN_COLS;
        let line_h = f32::from(m.line_h);
        let view = cx.entity().downgrade();
        let mut out = vec![];
        for slot in self.visible_slots() {
            let Some(Shown { card, .. }) = self.cards.shown.get(&slot.call) else { continue };
            let scope = Scope { call: card.call.clone(), ui: self.cards.ui.clone() };
            let (call, view) = (card.call.clone(), view.clone());
            let clear_view = view.clone();
            let body = div()
                .absolute()
                .left(margin)
                .top(-(m.line_h * slot.first as f32))
                .w(width)
                .text_size(crate::panels::scale::px(13.0))
                .on_children_prepainted(move |bounds, _, cx| {
                    let Some(height) = bounds.first().map(|b| f32::from(b.size.height)) else { return };
                    let rows = (height / line_h).ceil() as usize;
                    view.update(cx, |view, cx| view.note_card_rows(&call, rows, cx)).ok();
                })
                .child(crate::visual::ui::markdown(format!("card-{}", card.call), &card.content, scope));
            out.push(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(m.line_h * slot.row as f32 + m.shift)
                    .w(width + margin)
                    .h(m.line_h * slot.count as f32)
                    .overflow_hidden()
                    .block_mouse_except_scroll()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        clear_view.update(cx, |view, cx| view.clear_terminal_selection(cx)).ok();
                    })
                    .child(body)
                    .into_any_element(),
            );
        }
        out
    }

    /// A click on a card never reaches the terminal behind it, so its older selection is dropped here: Ctrl+C copies
    /// the terminal's selection first, and would otherwise copy that instead of the card text now being selected.
    fn clear_terminal_selection(&mut self, cx: &mut Context<Self>) {
        if self.terminal.term.lock().selection.take().is_some() {
            cx.notify();
        }
    }

    /// Tell the mod a card's height when it changes. Runs in a frame, so the file is written off the UI thread.
    fn note_card_rows(&mut self, call: &str, rows: usize, cx: &mut Context<Self>) {
        let Some(shown) = self.cards.shown.get_mut(call).filter(|s| s.rows != Some(rows) && rows > 0) else { return };
        shown.rows = Some(rows);
        let file = shown.rows_file.clone();
        let call = call.to_string();
        let wanted = self.cards.wanted_rows.clone();
        let writing = self.cards.writing_rows.clone();
        wanted.lock().unwrap_or_else(|e| e.into_inner()).insert(call.clone(), rows);
        cx.background_spawn(async move {
            let _one_at_a_time = writing.lock().unwrap_or_else(|e| e.into_inner());
            let latest = wanted.lock().unwrap_or_else(|e| e.into_inner()).get(&call).copied();
            if let Some(latest) = latest {
                let _ = crate::datadir::write_atomic(&file, &latest.to_string());
            }
        })
        .detach();
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
