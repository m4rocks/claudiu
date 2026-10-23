//! Start-up timing for Claude sessions, to see where the seconds go. Off unless `CLAUDIU_TRACE_START` is set; then each
//! step is appended to `<data>/start-trace.log` as `session folder, time since the terminal was made, step`.

use std::io::Write;
use std::sync::OnceLock;
use std::time::Duration;

pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CLAUDIU_TRACE_START").is_some())
}

pub fn note(tag: &str, since_start: Duration, step: &str) {
    if !on() {
        return;
    }
    let path = crate::datadir::data_dir().join("start-trace.log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{tag}\t{:>6} ms\t{step}", since_start.as_millis());
    }
}
