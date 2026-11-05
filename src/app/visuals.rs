//! Hands the cards each session's model drew (`visual/`), polled from the files its mod writes, to its terminal.

use super::*;
use crate::visual::{self, Card, Stamp};

impl Workspace {
    /// Look at the card files a few times a second, off the UI thread. What was there at start-up belongs to an
    /// earlier run: a session that shows those cards again has its mod write them again.
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

    /// Give each card that arrived to its session's terminal. `changes` holds the file stem of the session.
    pub(super) fn apply_visuals(&mut self, changes: Vec<(String, Card)>, cx: &mut Context<Self>) {
        for (stem, card) in changes {
            let Some(session) = self.store.data.sessions.iter().find(|s| datadir::safe_file_stem(&s.id) == stem) else { continue };
            let Some(view) = self.live_view(&session.id) else { continue };
            let rows_file = visual::rows_file(&stem, &card.call);
            view.update(cx, |view, cx| view.set_card(card, rows_file, cx));
        }
    }
}
