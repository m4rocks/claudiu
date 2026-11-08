//! The UI thread's link to everything else. Rune runs handlers and `poll` with the app's state and a `Tx` (the frame's
//! requests: clipboard, dialogs, quitting). Background work hands its result back as a job that runs on the UI thread
//! with the state, timers run due jobs, and work that needs the `Tx` waits in `fx` until the handler that queued it
//! ends (`flush`). Each window's state (the workspace, the setup window) has its own `Rt`.

use std::sync::{Arc, OnceLock};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use rune::Tx;

/// A change to the state, made on the UI thread.
pub type Job<S> = Box<dyn FnOnce(&mut S) + Send>;

/// Work that needs the frame's `Tx` (opening a link, copying), queued by code that has none.
pub type Fx = Box<dyn FnOnce(&mut Tx)>;

/// Wakes the UI thread so its next frame runs `poll`. Set once, when Rune hands it over.
static WAKER: OnceLock<rune::Waker> = OnceLock::new();

pub fn set_waker(waker: rune::Waker) {
    let _ = WAKER.set(waker);
}

/// Ask for a frame: a job, a terminal's output or a timer is waiting. Free when there is no window yet (tests).
pub fn wake() {
    if let Some(w) = WAKER.get() {
        w();
    }
}

/// The waker of one window, set once Rune hands it over. Until then (and in tests) the main window's is used.
type WindowWaker = Arc<OnceLock<rune::Waker>>;

fn wake_window(waker: &WindowWaker) {
    match waker.get() {
        Some(w) => w(),
        None => wake(),
    }
}

/// Sends jobs to the UI thread from any thread.
pub struct Mailbox<S>(Sender<Job<S>>, WindowWaker);

impl<S> Clone for Mailbox<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), self.1.clone())
    }
}

impl<S: 'static> Mailbox<S> {
    /// Run `f` with the state on the UI thread. Returns false once the state is gone (the window closed).
    pub fn send(&self, f: impl FnOnce(&mut S) + Send + 'static) -> bool {
        let sent = self.0.send(Box::new(f)).is_ok();
        wake_window(&self.1);
        sent
    }
}

struct Timer<S> {
    due: Instant,
    job: Box<dyn FnOnce(&mut S)>,
}

pub struct Rt<S> {
    tx: Sender<Job<S>>,
    rx: Receiver<Job<S>>,
    timers: Vec<Timer<S>>,
    fx: Vec<Fx>,
    waker: WindowWaker,
    /// Something changed outside a handler (a job ran, a terminal printed), so the view must run again.
    pub dirty: bool,
}

impl<S: 'static> Default for Rt<S> {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, timers: vec![], fx: vec![], waker: Arc::default(), dirty: false }
    }
}

impl<S: 'static> Rt<S> {
    pub fn mailbox(&self) -> Mailbox<S> {
        Mailbox(self.tx.clone(), self.waker.clone())
    }

    /// The waker of the window this state belongs to, from Rune's `setup`.
    /// Hand over the window's waker, and ask for a frame at once: a job that arrived before the window existed woke
    /// the main window instead, and nothing else would run it.
    pub fn set_waker(&self, waker: rune::Waker) {
        let _ = self.waker.set(waker);
        wake_window(&self.waker);
    }

    /// Run `work` on a thread of its own, then `done` with its result on the UI thread.
    pub fn spawn<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&mut S, T) + Send + 'static) {
        let mailbox = self.mailbox();
        std::thread::spawn(move || {
            let out = work();
            mailbox.send(move |s| done(s, out));
        });
    }

    /// Run `work` off the UI thread with nothing to report back.
    pub fn background(&self, work: impl FnOnce() + Send + 'static) {
        std::thread::spawn(work);
    }

    /// Run `f` with the state once `after` has passed (on the first frame after it).
    pub fn after(&mut self, after: Duration, f: impl FnOnce(&mut S) + 'static) {
        self.timers.push(Timer { due: Instant::now() + after, job: Box::new(f) });
    }

    /// Run `f` with the frame's `Tx` once the current handler ends.
    pub fn with_tx(&mut self, f: impl FnOnce(&mut Tx) + 'static) {
        self.fx.push(Box::new(f));
    }

    /// The jobs that arrived and the timers that are due, to run now in order.
    #[allow(clippy::type_complexity)] // Jobs and timers both run as plain boxed closures.
    fn take_due(&mut self) -> Vec<Box<dyn FnOnce(&mut S)>> {
        let mut out: Vec<Box<dyn FnOnce(&mut S)>> = vec![];
        while let Ok(job) = self.rx.try_recv() {
            out.push(job);
        }
        let now = Instant::now();
        let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.timers).into_iter().partition(|t| t.due <= now);
        self.timers = waiting;
        out.extend(due.into_iter().map(|t| t.job));
        out
    }

    fn next_deadline(&self) -> Option<Duration> {
        self.timers.iter().map(|t| t.due).min().map(|due| due.saturating_duration_since(Instant::now()))
    }
}

/// State with an `Rt`: the workspace and the setup window.
pub trait HasRt: Sized + 'static {
    fn rt(&mut self) -> &mut Rt<Self>;

    /// Run every arrived job and due timer. Returns whether any ran (the view must run again).
    fn run_jobs(&mut self) -> bool {
        let mut ran = false;
        // A job may queue another one (or a timer of zero), which runs on this same frame.
        for _ in 0..8 {
            let jobs = self.rt().take_due();
            if jobs.is_empty() {
                break;
            }
            ran = true;
            for job in jobs {
                job(self);
            }
        }
        ran
    }

    /// Hand the queued `Tx` work to the frame, and ask for a wake at the next timer.
    fn flush(&mut self, tx: &mut Tx) {
        for f in std::mem::take(&mut self.rt().fx) {
            f(tx);
        }
        if let Some(wait) = self.rt().next_deadline() {
            tx.wake_after(wait.max(Duration::from_millis(1)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct S {
        rt: Rt<S>,
        seen: Vec<u32>,
    }

    impl HasRt for S {
        fn rt(&mut self) -> &mut Rt<S> {
            &mut self.rt
        }
    }

    #[test]
    fn background_results_and_due_timers_run_on_the_state_in_order() {
        let mut s = S::default();
        s.rt.spawn(|| 2, |s: &mut S, v| s.seen.push(v));
        s.rt.after(Duration::ZERO, |s: &mut S| s.seen.push(3));
        s.rt.after(Duration::from_secs(60), |s: &mut S| s.seen.push(99));
        let start = Instant::now();
        while s.seen.len() < 2 && start.elapsed() < Duration::from_secs(5) {
            s.run_jobs();
        }
        s.seen.sort();
        assert_eq!(s.seen, [2, 3], "the timer a minute away has not run");
        assert!(!s.run_jobs(), "nothing more is due");
    }

    #[test]
    fn a_job_can_queue_a_timer_that_runs_on_the_same_frame() {
        let mut s = S::default();
        s.rt.mailbox().send(|s: &mut S| s.rt.after(Duration::ZERO, |s: &mut S| s.seen.push(1)));
        assert!(s.run_jobs());
        assert_eq!(s.seen, [1]);
    }
}
