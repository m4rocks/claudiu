//! Terminal emulation + PTY. No GPUI types here: one `Terminal` = one PTY = one `alacritty_terminal::Term`.
//! Children: `glyphs` (block and box drawing), `keys` (keystrokes to VT bytes), `skeleton` (the stand-in prompt), `trace`
//! (start-up timing) and `view` (the GPUI view over a `Terminal`).

pub mod glyphs;
pub mod keys;
pub mod skeleton;
pub mod trace;
pub mod view;

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
#[cfg(test)]
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

/// Forwards alacritty events to whoever owns the receiving end (the GPUI view).
#[derive(Clone)]
pub struct Listener(UnboundedSender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.0.unbounded_send(event);
    }
}

/// Give child processes the environment of *this* terminal rather than whatever launched Claudiu.
/// Call once at startup, before any threads exist (mutating the environment is not thread-safe).
///
/// Scrubs identity variables of other terminals and the markers Claude Code sets for its own
/// sub-shells (so a Claude launched here is not mistaken for a nested session). `NO_COLOR` is only
/// dropped when it demonstrably came from a Claude Code tool shell; a user's own `NO_COLOR` is honored.
pub fn prepare_child_env() {
    let from_claude_shell = std::env::var_os("CLAUDECODE").is_some();
    let exact = [
        "WT_SESSION", "WT_PROFILE_ID", "VTE_VERSION", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "CLAUDECODE",
        "CLAUDE_PID", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_ENTRYPOINT",
    ];
    let prefixes = ["ITERM_", "KITTY_", "ALACRITTY_", "WEZTERM_", "CLAUDE_CODE_SESSION", "CLAUDE_CODE_MESSAGING", "CLAUDE_CODE_BRIDGE"];
    let doomed: Vec<std::ffi::OsString> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| {
            let name = k.to_string_lossy();
            exact.contains(&&*name) || prefixes.iter().any(|p| name.starts_with(p)) || (from_claude_shell && name == "NO_COLOR")
        })
        .collect();
    for key in doomed {
        // SAFETY: called from main() before any other thread is started.
        unsafe { std::env::remove_var(key) };
    }
    unsafe {
        std::env::set_var("TERM_PROGRAM", "Claudiu");
        std::env::set_var("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    }
}

/// What to run inside the PTY.
#[derive(Clone, Debug, Default)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Extra environment for the child (added on top of the inherited one).
    pub env: Vec<(String, String)>,
}

/// How messages reach the PTY's event loop: queued while the process is still being created, then sent directly.
enum Link {
    Starting(Vec<Msg>),
    Ready(EventLoopSender),
    Failed,
}

pub struct Terminal {
    pub term: Arc<FairMutex<Term<Listener>>>,
    link: Arc<Mutex<Link>>,
    start_error: Arc<Mutex<Option<String>>>,
    cols: AtomicU32,
    rows: AtomicU32,
    /// The child's process id (0 until it exists), so closing can make sure it ends (Unix) and it can be
    /// raised in priority (Windows).
    pid: Arc<AtomicU32>,
    /// When it was made, and what to call it in the start-up trace (`trace.rs`).
    pub born: Instant,
    pub tag: String,
}

/// Create the PTY, start the child and its event loop. Slow for a big executable (the OS scans it first).
fn start_pty(
    spec: &SpawnSpec,
    window_size: WindowSize,
    term: Arc<FairMutex<Term<Listener>>>,
    listener: Listener,
    pid: &AtomicU32,
) -> anyhow::Result<EventLoopSender> {
    let program = if spec.program.contains(' ') && !spec.program.starts_with('"') {
        format!("\"{}\"", spec.program)
    } else {
        spec.program.clone()
    };
    let options = tty::Options {
        shell: Some(tty::Shell::new(program, spec.args.clone())),
        working_directory: spec.cwd.clone(),
        drain_on_exit: true,
        env: spec.env.iter().cloned().collect(),
        // Arguments may contain spaces (e.g. a settings file path): quote them properly.
        #[cfg(target_os = "windows")]
        escape_args: true,
    };
    // alacritty's ConPTY setup asserts on failure; surface that as a normal startup error instead of a crash.
    let pty = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tty::new(&options, window_size, 0)))
        .map_err(|_| anyhow::anyhow!("the pseudo terminal could not be created"))??;
    #[cfg(unix)]
    pid.store(pty.child().id(), Ordering::Relaxed);
    #[cfg(windows)]
    pid.store(pty.child_watcher().pid().map_or(0, |p| p.get()), Ordering::Relaxed);
    let event_loop = EventLoop::new(term, listener, pty, true, false)?;
    let sender = event_loop.channel();
    event_loop.spawn();
    Ok(sender)
}

impl Terminal {
    fn build(spec: &SpawnSpec, cols: u16, rows: u16) -> (Self, UnboundedReceiver<Event>, Listener, WindowSize) {
        let (tx, rx) = unbounded();
        let listener = Listener(tx);
        let window_size = WindowSize { num_lines: rows, num_cols: cols, cell_width: 8, cell_height: 16 };
        let size = TermSize::new(cols as usize, rows as usize);
        let config = Config { scrolling_history: 10_000, ..Default::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let tag = spec.cwd.as_ref().and_then(|c| c.file_name()).map_or_else(|| spec.program.clone(), |n| n.to_string_lossy().into_owned());
        let this = Self {
            term,
            link: Arc::new(Mutex::new(Link::Starting(vec![]))),
            start_error: Arc::new(Mutex::new(None)),
            cols: AtomicU32::new(cols as u32),
            rows: AtomicU32::new(rows as u32),
            pid: Arc::new(AtomicU32::new(0)),
            born: Instant::now(),
            tag,
        };
        (this, rx, listener, window_size)
    }

    /// Start the child and wait for it to exist (an unstartable program is an `Err`). The app itself uses
    /// `spawn_deferred`.
    #[cfg(test)]
    pub fn spawn(spec: &SpawnSpec, cols: u16, rows: u16) -> anyhow::Result<(Self, UnboundedReceiver<Event>)> {
        let (this, rx, listener, window_size) = Self::build(spec, cols, rows);
        let sender = start_pty(spec, window_size, this.term.clone(), listener, &this.pid)?;
        *this.link.lock().unwrap() = Link::Ready(sender);
        Ok((this, rx))
    }

    /// Return at once and start the child on another thread: creating a process from a very large executable
    /// takes about a second, which would freeze the window. Input and resizes sent meanwhile are queued. If the
    /// child cannot start, the terminal reports an exit and `start_error` says why.
    pub fn spawn_deferred(spec: &SpawnSpec, cols: u16, rows: u16) -> (Self, UnboundedReceiver<Event>) {
        let (this, rx, listener, window_size) = Self::build(spec, cols, rows);
        let (spec, term, link, error) = (spec.clone(), this.term.clone(), Arc::downgrade(&this.link), this.start_error.clone());
        let (born, tag, pid) = (this.born, this.tag.clone(), this.pid.clone());
        std::thread::spawn(move || {
            let outcome = start_pty(&spec, window_size, term, listener.clone(), &pid);
            crate::terminal::trace::note(&tag, born.elapsed(), "child process created");
            // The terminal may be gone already (the session was closed while starting): then drop the new child.
            let Some(link) = link.upgrade() else {
                if let Ok(sender) = outcome {
                    let _ = sender.send(Msg::Shutdown);
                    end_child(pid.load(Ordering::Relaxed));
                }
                return;
            };
            let mut link = link.lock().unwrap();
            match outcome {
                Ok(sender) => {
                    if let Link::Starting(queued) = std::mem::replace(&mut *link, Link::Failed) {
                        for msg in queued {
                            let _ = sender.send(msg);
                        }
                    }
                    *link = Link::Ready(sender);
                }
                Err(e) => {
                    *link = Link::Failed;
                    *error.lock().unwrap() = Some(e.to_string());
                    listener.send_event(Event::Exit);
                }
            }
        });
        (this, rx)
    }

    /// The child's process id, once it exists.
    #[cfg(windows)]
    pub fn process_id(&self) -> Option<u32> {
        Some(self.pid.load(Ordering::Relaxed)).filter(|&pid| pid != 0)
    }

    /// Why the child could not be started, for a terminal made by `spawn_deferred`.
    pub fn start_error(&self) -> Option<String> {
        self.start_error.lock().unwrap().clone()
    }

    fn send(&self, msg: Msg) {
        match &mut *self.link.lock().unwrap() {
            Link::Starting(queue) => queue.push(msg),
            Link::Ready(sender) => {
                let _ = sender.send(msg);
            }
            Link::Failed => {}
        }
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        self.send(Msg::Input(bytes.into()));
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols.load(Ordering::Relaxed) as u16, self.rows.load(Ordering::Relaxed) as u16)
    }

    pub fn window_size(&self, cell_w: f32, cell_h: f32) -> WindowSize {
        let (cols, rows) = self.size();
        WindowSize { num_lines: rows, num_cols: cols, cell_width: cell_w as u16, cell_height: cell_h as u16 }
    }

    /// Resize grid + PTY. No-op when nothing changed.
    pub fn resize(&self, cols: u16, rows: u16, cell_w: f32, cell_h: f32) {
        if self.size() == (cols, rows) {
            return;
        }
        self.cols.store(cols as u32, Ordering::Relaxed);
        self.rows.store(rows as u32, Ordering::Relaxed);
        self.term.lock().resize(TermSize::new(cols as usize, rows as usize));
        self.send(Msg::Resize(self.window_size(cell_w, cell_h)));
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Closing the pseudo console also terminates the child.
        self.send(Msg::Shutdown);
        end_child(self.pid.load(Ordering::Relaxed));
    }
}

/// Closing a Unix PTY only sends the child SIGHUP, which Claude Code can outlive: it then keeps running (and keeps
/// Remote Control for the conversation) after its session was closed, so reopening starts a second one. Ask it to
/// end, and force it if it has not after a moment.
#[cfg(unix)]
fn end_child(pid: u32) {
    if pid == 0 {
        return;
    }
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        // ponytail: a pid reused within these two seconds would be hit; the PTY drop reaps the child, so it is rare.
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    });
}

#[cfg(not(unix))]
fn end_child(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn echo_spec(text: &str) -> SpawnSpec {
        if cfg!(windows) {
            SpawnSpec { program: "cmd.exe".into(), args: vec!["/c".into(), format!("echo {text}")], cwd: None, ..Default::default() }
        } else {
            SpawnSpec { program: "/bin/sh".into(), args: vec!["-c".into(), format!("echo {text}")], cwd: None, ..Default::default() }
        }
    }

    fn wait_for_exit(rx: &mut UnboundedReceiver<Event>) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            while let Ok(ev) = rx.try_recv() {
                if matches!(ev, Event::ChildExit(_) | Event::Exit) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        false
    }

    fn screen_text(t: &Terminal) -> String {
        let term = t.term.lock();
        term.renderable_content().display_iter.map(|c| c.cell.c).collect()
    }

    #[test]
    fn spawn_produces_output_and_reports_exit() {
        let (term, mut rx) = Terminal::spawn(&echo_spec("claudiu-lifecycle-ok"), 80, 24).expect("spawn");
        assert!(wait_for_exit(&mut rx), "child exit must be reported");
        assert!(screen_text(&term).contains("claudiu-lifecycle-ok"), "output reaches the grid");
    }

    #[test]
    fn startup_failure_is_an_error_not_a_panic() {
        let spec = SpawnSpec { program: "definitely-not-a-real-program-claudiu".into(), args: vec![], cwd: None, ..Default::default() };
        assert!(Terminal::spawn(&spec, 80, 24).is_err());
    }

    #[test]
    fn resize_is_tracked_and_idempotent() {
        let (term, _rx) = Terminal::spawn(&echo_spec("x"), 80, 24).expect("spawn");
        assert_eq!(term.size(), (80, 24));
        term.resize(100, 30, 8.0, 16.0);
        assert_eq!(term.size(), (100, 30));
        assert_eq!(term.term.lock().grid().columns(), 100);
        term.resize(100, 30, 8.0, 16.0);
        assert_eq!(term.size(), (100, 30));
    }

    #[cfg(unix)]
    #[test]
    fn closing_ends_a_child_that_ignores_hangup() {
        let spec = SpawnSpec { program: "sh".into(), args: vec!["-c".into(), "trap '' HUP TERM; sleep 60".into()], cwd: None, ..Default::default() };
        let (t, _rx) = Terminal::spawn(&spec, 80, 24).unwrap();
        let pid = t.pid.load(Ordering::Relaxed) as i32;
        std::thread::sleep(Duration::from_millis(300));
        drop(t);
        std::thread::sleep(Duration::from_secs(3));
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "child must be gone");
    }

    #[cfg(windows)]
    #[test]
    fn closing_ends_a_running_child() {
        let spec = SpawnSpec { program: "ping.exe".into(), args: vec!["-n".into(), "60".into(), "127.0.0.1".into()], cwd: None, ..Default::default() };
        let (t, _rx) = Terminal::spawn(&spec, 80, 24).unwrap();
        let pid = t.process_id().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(crate::platform::process_running(pid));
        drop(t);
        let began = Instant::now();
        while crate::platform::process_running(pid) {
            assert!(began.elapsed() < Duration::from_secs(10), "the child outlived its terminal");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
