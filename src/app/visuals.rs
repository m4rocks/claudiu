//! Hands the cards each session's model drew (`visual/`), polled from the files its mod writes, to its terminal.

use super::*;
use crate::visual::{self, Card, Stamp};

impl Workspace {
    /// Look at the card files a few times a second, off the UI thread. What was there at start-up belongs to an
    /// earlier run: a session that shows those cards again has its mod write them again.
    pub(super) fn start_visual_poll(&mut self) {
        let mailbox = self.rt.mailbox();
        self.rt.background(move || {
            let mut seen: HashMap<String, Stamp> = HashMap::new();
            let mut first = true;
            loop {
                let changes = visual::read_changed(&visual::dir(), &mut seen);
                if !std::mem::take(&mut first) && !changes.is_empty() && !mailbox.send(move |ws: &mut Workspace| ws.apply_visuals(changes)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    }

    /// Give each card that arrived to its session's terminal. `changes` holds the file stem of the session.
    pub(super) fn apply_visuals(&mut self, changes: Vec<(String, Card)>) {
        for (stem, card) in changes {
            let Some(session) = self.store.data.sessions.iter().find(|s| datadir::safe_file_stem(&s.id) == stem) else { continue };
            let id = session.id.clone();
            let rows_file = visual::rows_file(&stem, &card.call);
            if let Some(view) = self.terminal_mut(&id) {
                view.set_card(card, rows_file);
            }
        }
    }
}
