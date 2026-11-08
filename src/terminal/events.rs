//! Redraw requests describe the current grid, so only one needs to wait in the queue.

use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::task::{Context, Poll};

use alacritty_terminal::event::{Event, EventListener};
use futures::{
    Stream,
    channel::mpsc::{self, UnboundedReceiver, UnboundedSender},
};

#[derive(Clone)]
pub struct Listener {
    tx: UnboundedSender<Event>,
    wakeup: Arc<AtomicBool>,
}

pub struct EventReceiver {
    rx: UnboundedReceiver<Event>,
    wakeup: Arc<AtomicBool>,
}

pub fn channel() -> (Listener, EventReceiver) {
    let (tx, rx) = mpsc::unbounded();
    let wakeup = Arc::new(AtomicBool::new(false));
    (
        Listener {
            tx,
            wakeup: wakeup.clone(),
        },
        EventReceiver { rx, wakeup },
    )
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if matches!(event, Event::Wakeup) && self.wakeup.swap(true, Ordering::Relaxed) {
            return;
        }
        let _ = self.tx.unbounded_send(event);
        // The UI thread reads terminals on its next frame.
        crate::app::rt::wake();
    }
}

impl EventReceiver {
    fn received(&self, event: &Event) {
        if matches!(event, Event::Wakeup) {
            // A producer can now queue the next redraw while this one is being handled.
            // The terminal grid itself is synchronized by alacritty's mutex.
            self.wakeup.store(false, Ordering::Relaxed);
        }
    }

    pub fn try_recv(&mut self) -> Result<Event, mpsc::TryRecvError> {
        let event = self.rx.try_recv()?;
        self.received(&event);
        Ok(event)
    }
}

impl Stream for EventReceiver {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let event = Pin::new(&mut self.rx).poll_next(cx);
        if let Poll::Ready(Some(ref event)) = event {
            self.received(event);
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[test]
    fn redraws_coalesce_but_protocol_events_keep_their_order() {
        let (tx, mut rx) = channel();
        for _ in 0..10_000 {
            tx.send_event(Event::Wakeup);
        }
        tx.send_event(Event::PtyWrite("reply".into()));
        tx.send_event(Event::Title("title".into()));
        tx.send_event(Event::Bell);
        tx.send_event(Event::Exit);
        assert!(matches!(rx.try_recv(), Ok(Event::Wakeup)));
        assert!(matches!(rx.try_recv(), Ok(Event::PtyWrite(s)) if s == "reply"));
        assert!(matches!(rx.try_recv(), Ok(Event::Title(s)) if s == "title"));
        assert!(matches!(rx.try_recv(), Ok(Event::Bell)));
        assert!(matches!(rx.try_recv(), Ok(Event::Exit)));
        assert!(rx.try_recv().is_err());
        tx.send_event(Event::Wakeup);
        assert!(matches!(futures::executor::block_on(rx.next()), Some(Event::Wakeup)));
        tx.send_event(Event::Wakeup);
        assert!(matches!(rx.try_recv(), Ok(Event::Wakeup)));
        drop(tx);
        assert!(futures::executor::block_on(rx.next()).is_none());
    }

    #[test]
    fn concurrent_producers_leave_a_redraw_after_the_consumer_catches_up() {
        let (tx, mut rx) = channel();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let tx = tx.clone();
                scope.spawn(move || {
                    for _ in 0..10_000 {
                        tx.send_event(Event::Wakeup);
                    }
                });
            }
            for _ in 0..10_000 {
                let _ = rx.try_recv();
            }
        });
        while rx.try_recv().is_ok() {}
        tx.send_event(Event::Wakeup);
        assert!(matches!(rx.try_recv(), Ok(Event::Wakeup)));
    }

    #[test]
    #[ignore = "manual performance measurement"]
    fn benchmark_redraw_queue() {
        const EVENTS: usize = 1_000_000;
        for round in 0..7 {
            let (tx, mut rx) = mpsc::unbounded();
            let start = std::time::Instant::now();
            for _ in 0..EVENTS {
                tx.unbounded_send(Event::Wakeup).unwrap();
            }
            let mut before = 0;
            while rx.try_recv().is_ok() {
                before += 1;
            }
            let baseline = start.elapsed();
            let (tx, mut rx) = channel();
            let start = std::time::Instant::now();
            for _ in 0..EVENTS {
                tx.send_event(Event::Wakeup);
            }
            let mut after = 0;
            while rx.try_recv().is_ok() {
                after += 1;
            }
            println!(
                "round={round} events={EVENTS} before_queued={before} after_queued={after} before_us={} after_us={}",
                baseline.as_micros(),
                start.elapsed().as_micros()
            );
            assert_eq!(before, EVENTS);
            assert_eq!(after, 1);
        }
    }
}
