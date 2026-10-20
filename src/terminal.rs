//! Terminal emulation + PTY. No GPUI types here: one `Terminal` = one PTY = one `alacritty_terminal::Term`.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

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

pub struct Terminal {
    pub term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    cols: AtomicU32,
    rows: AtomicU32,
}

impl Terminal {
    pub fn spawn(spec: &SpawnSpec, cols: u16, rows: u16) -> anyhow::Result<(Self, UnboundedReceiver<Event>)> {
        let (tx, rx) = unbounded();
        let listener = Listener(tx);

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
        let window_size = WindowSize { num_lines: rows, num_cols: cols, cell_width: 8, cell_height: 16 };
        // alacritty's ConPTY setup asserts on failure; surface that as a normal startup error instead of a crash.
        let pty = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tty::new(&options, window_size, 0)))
            .map_err(|_| anyhow::anyhow!("the pseudo terminal could not be created"))??;

        let size = TermSize::new(cols as usize, rows as usize);
        let config = Config { scrolling_history: 10_000, ..Default::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));

        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        event_loop.spawn();

        Ok((
            Self { term, sender, cols: AtomicU32::new(cols as u32), rows: AtomicU32::new(rows as u32) },
            rx,
        ))
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
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
        let _ = self.sender.send(Msg::Resize(self.window_size(cell_w, cell_h)));
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Closing the pseudo console also terminates the child.
        let _ = self.sender.send(Msg::Shutdown);
    }
}

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
}
