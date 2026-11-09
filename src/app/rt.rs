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

/// A job on its way to the UI thread. A quiet one doesn't make the view run again by itself: it sets `Rt::dirty` when
/// it changes what is shown.
struct Queued<S> {
    job: Job<S>,
    quiet: bool,
}

/// Sends jobs to the UI thread from any thread.
pub struct Mailbox<S>(Sender<Queued<S>>, WindowWaker);

impl<S> Clone for Mailbox<S> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), self.1.clone())
    }
}

impl<S: 'static> Mailbox<S> {
    /// Run `f` with the state on the UI thread. Returns false once the state is gone (the window closed).
    pub fn send(&self, f: impl FnOnce(&mut S) + Send + 'static) -> bool {
        self.queue(f, false)
    }

    fn queue(&self, f: impl FnOnce(&mut S) + Send + 'static, quiet: bool) -> bool {
        let sent = self.0.send(Queued { job: Box::new(f), quiet }).is_ok();
        wake_window(&self.1);
        sent
    }
}

struct Timer<S> {
    due: Instant,
    job: Box<dyn FnOnce(&mut S)>,
    quiet: bool,
}

pub struct Rt<S> {
    tx: Sender<Queued<S>>,
    rx: Receiver<Queued<S>>,
    timers: Vec<Timer<S>>,
    fx: Vec<Fx>,
    waker: WindowWaker,
    /// Something changed outside a handler (a quiet job changed what is shown, a toast came up), so the view must
    /// run again.
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
        self.spawn_as(work, done, false);
    }

    /// `spawn` for work that repeats and mostly finds nothing new: `done` sets `dirty` when it changes what is shown,
    /// so an idle window isn't drawn again for nothing.
    pub fn spawn_quietly<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&mut S, T) + Send + 'static) {
        self.spawn_as(work, done, true);
    }

    fn spawn_as<T: Send + 'static>(&self, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&mut S, T) + Send + 'static, quiet: bool) {
        let mailbox = self.mailbox();
        std::thread::spawn(move || {
            let out = work();
            mailbox.queue(move |s| done(s, out), quiet);
        });
    }

    /// Run `work` off the UI thread with nothing to report back.
    pub fn background(&self, work: impl FnOnce() + Send + 'static) {
        std::thread::spawn(work);
    }

    /// Run `f` with the state once `after` has passed (on the first frame after it).
    pub fn after(&mut self, after: Duration, f: impl FnOnce(&mut S) + 'static) {
        self.timers.push(Timer { due: Instant::now() + after, job: Box::new(f), quiet: false });
    }

    /// `after` for a timer that repeats and mostly changes nothing: `f` sets `dirty` when it changes what is shown.
    pub fn after_quietly(&mut self, after: Duration, f: impl FnOnce(&mut S) + 'static) {
        self.timers.push(Timer { due: Instant::now() + after, job: Box::new(f), quiet: true });
    }

    /// Run `f` with the frame's `Tx` once the current handler ends.
    pub fn with_tx(&mut self, f: impl FnOnce(&mut Tx) + 'static) {
        self.fx.push(Box::new(f));
    }

    /// The jobs that arrived and the timers that are due, to run now in order, each with whether it is quiet.
    #[allow(clippy::type_complexity)] // Jobs and timers both run as plain boxed closures.
    fn take_due(&mut self) -> Vec<(Box<dyn FnOnce(&mut S)>, bool)> {
        let mut out: Vec<(Box<dyn FnOnce(&mut S)>, bool)> = vec![];
        while let Ok(q) = self.rx.try_recv() {
            out.push((q.job, q.quiet));
        }
        let now = Instant::now();
        let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.timers).into_iter().partition(|t| t.due <= now);
        self.timers = waiting;
        out.extend(due.into_iter().map(|t| (t.job, t.quiet)));
        out
    }

    fn next_deadline(&self) -> Option<Duration> {
        self.timers.iter().map(|t| t.due).min().map(|due| due.saturating_duration_since(Instant::now()))
    }
}

/// State with an `Rt`: the workspace and the setup window.
pub trait HasRt: Sized + 'static {
    fn rt(&mut self) -> &mut Rt<Self>;

    /// Run every arrived job and due timer. Returns whether any that isn't quiet ran (the view must run again).
    fn run_jobs(&mut self) -> bool {
        let mut ran = false;
        // A job may queue another one (or a timer of zero), which runs on this same frame.
        for _ in 0..8 {
            let jobs = self.rt().take_due();
            if jobs.is_empty() {
                break;
            }
            for (job, quiet) in jobs {
                ran |= !quiet;
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
    fn quiet_jobs_run_without_asking_for_a_redraw() {
        let mut s = S::default();
        s.rt.after_quietly(Duration::ZERO, |s: &mut S| s.seen.push(1));
        s.rt.spawn_quietly(|| 2, |s: &mut S, v| s.seen.push(v));
        let start = Instant::now();
        let mut ran = false;
        while s.seen.len() < 2 && start.elapsed() < Duration::from_secs(5) {
            ran |= s.run_jobs();
        }
        s.seen.sort();
        assert_eq!(s.seen, [1, 2]);
        assert!(!ran, "quiet jobs say for themselves when the view must run again");
        s.rt.after_quietly(Duration::ZERO, |s: &mut S| s.rt.after(Duration::ZERO, |s: &mut S| s.seen.push(3)));
        assert!(s.run_jobs(), "a plain timer a quiet one queued still asks for a redraw");
    }

    #[test]
    fn a_job_can_queue_a_timer_that_runs_on_the_same_frame() {
        let mut s = S::default();
        s.rt.mailbox().send(|s: &mut S| s.rt.after(Duration::ZERO, |s: &mut S| s.seen.push(1)));
        assert!(s.run_jobs());
        assert_eq!(s.seen, [1]);
    }
}
