//! What a language server adds to the editor: a card about the word under the pointer (its type or docs, and any
//! problem there), the completion list Ctrl+Space opens, and the requests for a definition. The editor asks through
//! `Editor::requests`; the workspace answers from the file's server (`app::language_servers`).

use rune::{Color, El, Transition, col, el, row, text};

use super::buffer::{Pos, Selection};
use super::Editor;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{font, label};

/// How long the pointer rests on a word before its card is asked for.
pub const HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(450);
/// Rows the completion list shows at once.
const COMPLETION_ROWS: usize = 10;

/// What the editor asks of the file's language server.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Request {
    /// The pointer came to rest on this word (`serial` tells a later rest from it); ask after `HOVER_DELAY`.
    Hover(Pos, u64),
    Definition(Pos),
    Complete(Pos),
}

/// The card under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub struct HoverCard {
    pub at: Pos,
    /// Problems the server reported there, then what it says about the word (Markdown).
    pub problems: Vec<String>,
    pub docs: Option<String>,
}

/// One completion, with what accepting it writes.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub label: String,
    pub detail: Option<String>,
    /// Replaces the word typed so far, or `replace` when the server named a range.
    pub insert: String,
    pub replace: Option<(Pos, Pos)>,
}

pub struct Completion {
    /// Where the word being completed starts; the list closes when the caret leaves it.
    pub from: Pos,
    pub items: Vec<Item>,
    /// The items that match what is typed after `from`, best first.
    pub shown: Vec<usize>,
    pub selected: usize,
}

impl Editor {
    /// Where the word (letters, digits, `_`) that ends at `p` starts.
    pub fn word_start(&self, p: Pos) -> Pos {
        let text = &self.buffer.line(p.line)[..p.col];
        let start = text.char_indices().rev().take_while(|(_, c)| c.is_alphanumeric() || *c == '_').last().map_or(p.col, |(i, _)| i);
        Pos::new(p.line, start)
    }

    /// The pointer moved over the editor's area: a new word under it hides the card and starts the wait for its own.
    pub fn pointer_moved(&mut self, x: f32, y: f32) {
        let p = self.pos_at(x, y);
        let (a, b) = self.buffer.word_at(p);
        let same = self.hover_word.is_some_and(|(wa, wb)| wa == a && wb == b);
        if same {
            return;
        }
        self.hover_word = Some((a, b));
        if self.hover.take().is_some() {
            self.dirty = true;
        }
        // Spaces and the empty end of a line have nothing to tell.
        if a == b || self.buffer.line(a.line)[a.col..b.col].trim().is_empty() {
            return;
        }
        self.hover_serial += 1;
        self.requests.push(Request::Hover(a, self.hover_serial));
    }

    pub fn pointer_left(&mut self) {
        self.hover_word = None;
        self.hover_serial += 1;
        if self.hover.take().is_some() {
            self.dirty = true;
        }
    }

    /// Whether the pointer still rests where hover request `serial` was made.
    pub fn hover_wanted(&self, serial: u64) -> bool {
        self.hover_serial == serial
    }

    /// The problems reported at `p`.
    pub fn problems_at(&self, p: Pos) -> Vec<String> {
        self.problems.iter().filter(|d| d.from <= p && (p < d.to || d.from == d.to && p == d.from)).map(|d| d.message.clone()).collect()
    }

    /// Show the card for the word at `at`, unless the pointer moved on (`serial`) or there is nothing to say.
    pub fn show_hover(&mut self, serial: u64, at: Pos, docs: Option<String>) {
        let problems = self.problems_at(at);
        if !self.hover_wanted(serial) || (problems.is_empty() && docs.is_none()) {
            return;
        }
        self.hover = Some(HoverCard { at, problems, docs });
        self.dirty = true;
    }

    /// Open the completion list for the word being typed at the caret.
    pub fn show_completions(&mut self, at: Pos, items: Vec<Item>) {
        if self.sel.head != at || items.is_empty() {
            return;
        }
        self.completion = Some(Completion { from: self.word_start(at), items, shown: vec![], selected: 0 });
        self.filter_completions();
    }

    /// Keep the items matching what is typed since the list opened: those starting with it first, then those holding
    /// it. The list closes once the caret leaves the word or nothing matches.
    pub(super) fn filter_completions(&mut self) {
        let caret = self.sel.head;
        let Some(c) = &mut self.completion else { return };
        if caret.line != c.from.line || caret.col < c.from.col || !self.sel.is_empty() {
            self.completion = None;
            self.dirty = true;
            return;
        }
        let typed = self.buffer.line(caret.line)[c.from.col..caret.col].to_lowercase();
        let mut starts = vec![];
        let mut holds = vec![];
        for (i, item) in c.items.iter().enumerate() {
            let l = item.label.to_lowercase();
            if l.starts_with(&typed) {
                starts.push(i);
            } else if l.contains(&typed) {
                holds.push(i);
            }
        }
        starts.extend(holds);
        if starts.is_empty() {
            self.completion = None;
        } else {
            c.shown = starts;
            c.selected = c.selected.min(c.shown.len() - 1);
        }
        self.dirty = true;
    }

    /// Write the highlighted completion over the word typed so far.
    pub(super) fn accept_completion(&mut self) {
        let Some(c) = self.completion.take() else { return };
        let Some(item) = c.shown.get(c.selected).map(|&i| c.items[i].clone()) else { return };
        let caret = self.sel.head;
        let (a, b) = match item.replace {
            // The server's range ends where the caret was when it was asked; letters typed since go too.
            Some((a, b)) if a.line == caret.line && b.line == caret.line => (a.min(caret), b.max(caret)),
            _ => (c.from, caret),
        };
        self.sel = Selection::caret(caret);
        self.replace(a, b, &item.insert, false);
    }

    /// Up, Down, Enter, Tab and Escape while the completion list is open; `false` for any other key.
    pub(super) fn completion_key(&mut self, key: &str) -> bool {
        let Some(c) = &mut self.completion else { return false };
        match key {
            "up" => c.selected = c.selected.checked_sub(1).unwrap_or(c.shown.len() - 1),
            "down" => c.selected = (c.selected + 1) % c.shown.len(),
            "enter" | "tab" => self.accept_completion(),
            "escape" => self.completion = None,
            _ => return false,
        }
        self.dirty = true;
        true
    }

    /// The card and the completion list, placed by the text they are about; `None` while neither is up.
    pub(super) fn render_assist(&self, cell_w: f32, line_h: f32, top_line: usize, top: f32, area: [f32; 2]) -> Vec<El> {
        let place = |p: Pos| {
            let x = (self.column(p) as f32 * cell_w - self.scroll[0]).max(0.0);
            let y = top + line_h * (p.line as f32 - top_line as f32);
            (x, y)
        };
        let mut out = vec![];
        if let Some(c) = &self.completion {
            let (x, y) = place(c.from);
            let mut list = col().key(format!("{}/completions", self.key)).w(380.0).padding(4.0).radius(7.0).fill(rgb(0x18181a)).border(1.0, rgb(0x2c2c30)).shadow(8.0, 24.0, Color::BLACK.alpha(0.5)).transition(Transition::opacity().combined(Transition::offset(0.0, -4.0)));
            let start = c.selected.saturating_sub(COMPLETION_ROWS - 1);
            for (row_ix, &i) in c.shown.iter().enumerate().skip(start).take(COMPLETION_ROWS) {
                let item = &c.items[i];
                let mut r = row().key(format!("{}/completion-{row_ix}", self.key)).w_full().h(22.0).px(8.0).gap(10.0).center_y().radius(4.0).justify_between();
                if row_ix == c.selected {
                    r = r.fill(rgb(theme::ROW_ACTIVE));
                }
                r = r.child(text(item.label.clone()).font(super::element::font(crate::panels::scale::px(self.font_size))).color(rgb(theme::TEXT)).truncate().shrink());
                if let Some(d) = &item.detail {
                    r = r.child(label(d.clone(), 11.0, theme::TEXT_FAINT).truncate().shrink());
                }
                list = list.child(r);
            }
            // Below the line, or above it when it would run off the bottom.
            let h = 8.0 + 22.0 * c.shown.len().min(COMPLETION_ROWS) as f32;
            let y = if y + line_h + h > area[1] && y > h { y - h - 2.0 } else { y + line_h + 2.0 };
            out.push(el().absolute(x.min((area[0] - 380.0).max(0.0)), y).child(list));
        }
        if let Some(h) = self.hover.as_ref().filter(|_| self.completion.is_none()) {
            let (x, y) = place(h.at);
            let mut card = col().key(format!("{}/hover", self.key)).max_w(520.0).padding(10.0).gap(8.0).radius(7.0).fill(rgb(0x18181a)).border(1.0, rgb(0x2c2c30)).shadow(8.0, 24.0, Color::BLACK.alpha(0.5)).transition(Transition::opacity());
            for p in &h.problems {
                card = card.child(row().gap(6.0).child(el().size(6.0, 6.0).radius(3.0).fill(rgb(theme::DANGER)).mt(5.0)).child(label(p.clone(), 12.0, theme::TEXT).wrap().shrink()));
            }
            if let Some(d) = &h.docs {
                card = card.child(crate::panels::markdown::markdown(d).render(&crate::panels::markdown::style().body_font(font(12.0, 400.0))));
            }
            // Above the word, or below it near the top.
            let room = 220.0;
            let pinned = if y > room { el().pin(Some(x.min((area[0] - 520.0).max(0.0))), None, None, Some(area[1] - y + 4.0)) } else { el().absolute(x.min((area[0] - 520.0).max(0.0)), y + line_h + 4.0) };
            out.push(pinned.child(card));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> Item {
        Item { label: label.into(), detail: None, insert: label.into(), replace: None }
    }

    #[test]
    fn the_list_follows_typing_and_accepting_replaces_the_word() {
        let mut e = Editor::new("e", "let x = hel");
        let end = Pos::new(0, 11);
        e.select(Selection::caret(end));
        e.show_completions(end, vec![item("hello"), item("shell"), item("help"), item("other")]);
        let shown = |e: &Editor| e.completion.as_ref().map(|c| c.shown.iter().map(|&i| c.items[i].label.clone()).collect::<Vec<_>>());
        assert_eq!(shown(&e).unwrap(), ["hello", "help", "shell"], "starting with it first, then holding it");
        e.insert("p", true);
        e.filter_completions();
        assert_eq!(shown(&e).unwrap(), ["help"]);
        assert!(e.completion_key("enter"));
        assert_eq!(e.buffer.text(), "let x = help");
        assert!(e.completion.is_none());
    }

    #[test]
    fn the_list_closes_when_the_caret_leaves_the_word() {
        let mut e = Editor::new("e", "x ab");
        let end = Pos::new(0, 4);
        e.select(Selection::caret(end));
        e.show_completions(end, vec![item("abc")]);
        e.select(Selection::caret(Pos::new(0, 2)));
        e.filter_completions();
        assert!(e.completion.is_some(), "the start of the word is still in it");
        e.select(Selection::caret(Pos::new(0, 1)));
        e.filter_completions();
        assert!(e.completion.is_none(), "before the word is out of it");
    }

    #[test]
    fn a_server_range_is_replaced_with_what_was_typed_since() {
        let mut e = Editor::new("e", "x.fo");
        let end = Pos::new(0, 4);
        e.select(Selection::caret(end));
        let it = Item { label: "foo()".into(), detail: None, insert: "foo()".into(), replace: Some((Pos::new(0, 2), Pos::new(0, 3))) };
        e.show_completions(end, vec![it]);
        e.accept_completion();
        assert_eq!(e.buffer.text(), "x.foo()");
    }

    #[test]
    fn the_card_waits_for_the_pointer_to_rest_and_shows_problems_there() {
        let mut e = Editor::new("e", "let value = 1;");
        e.problems.push(super::super::Problem { from: Pos::new(0, 4), to: Pos::new(0, 9), error: true, message: "unused".into() });
        e.hover_serial = 3;
        e.show_hover(2, Pos::new(0, 4), None);
        assert!(e.hover.is_none(), "the pointer moved on since rest 2");
        e.show_hover(3, Pos::new(0, 4), Some("`i32`".into()));
        assert_eq!(e.hover.as_ref().map(|h| h.problems.clone()), Some(vec!["unused".to_string()]));
        e.pointer_left();
        assert!(e.hover.is_none());
        assert_eq!(e.word_start(Pos::new(0, 9)), Pos::new(0, 4));
    }
}
