//! The Visual panel's state: the cards each session's model drew (`visual/`), polled from the files its mod writes.

use super::*;
use crate::visual::{self, Card, Stamp};

impl Workspace {
    /// Look at the card files a few times a second, off the UI thread. What was there at start-up belongs to an
    /// earlier run and is not shown.
    pub(super) fn start_visual_poll(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut seen: HashMap<String, Stamp> = HashMap::new();
            let mut first = true;
            loop {
                let (changes, back) = cx
                    .background_executor()
                    .spawn(async move {
                        let changes = visual::read_changed(&visual::dir(), &mut seen);
                        (changes, seen)
                    })
                    .await;
                seen = back;
                if !std::mem::take(&mut first) && !changes.is_empty() && this.update(cx, |ws, cx| ws.apply_visuals(changes, cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(200)).await;
            }
        })
        .detach();
    }

    /// Add the cards that arrived. A new one for the shown session opens the panel (the keyboard stays where it is).
    pub(super) fn apply_visuals(&mut self, changes: Vec<(String, Card)>, cx: &mut Context<Self>) {
        let mut open = false;
        for (stem, card) in changes {
            let Some(id) = self.store.data.sessions.iter().find(|s| datadir::safe_file_stem(&s.id) == stem).map(|s| s.id.clone()) else { continue };
            let new = visual::upsert(self.visuals.entry(id.clone()).or_default(), card);
            open |= new && self.active_id() == Some(&id);
        }
        if open {
            self.right_visual = true;
            self.right_gh = None;
            self.store.data.layout.right_hidden = false;
            self.store.mark_dirty();
        }
        cx.notify();
    }

    /// The shown session's cards, oldest first.
    pub fn visual_cards(&self) -> &[Card] {
        self.active_id().and_then(|id| self.visuals.get(id)).map_or(&[], Vec::as_slice)
    }

    /// Whether the right sidebar is showing the Visual panel now.
    pub fn visual_shown(&self) -> bool {
        self.right_gh.is_none() && self.right_visual && !self.visual_cards().is_empty()
    }

    pub fn show_visual(&mut self, cx: &mut Context<Self>) {
        self.right_visual = true;
        self.right_gh = None;
        self.store.data.layout.right_hidden = false;
        self.store.mark_dirty();
        cx.notify();
    }

    /// Back to the menu (`clear` also forgets the shown session's cards).
    pub fn hide_visual(&mut self, clear: bool, cx: &mut Context<Self>) {
        self.right_visual = false;
        if clear && let Some(id) = self.active_id().cloned() {
            self.visuals.remove(&id);
        }
        cx.notify();
    }
}
