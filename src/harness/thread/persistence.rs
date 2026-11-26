//! One ordered writer for transcript snapshots. Reads and deletes are barriers, so an old snapshot cannot
//! resurrect deleted history or land after a newer one. Serialization never runs on the UI thread.
use super::{Entry, entries_path};
use std::{collections::HashMap, path::PathBuf, sync::{OnceLock, mpsc}, time::{Duration, Instant}};

enum Job {
    Save(PathBuf, Vec<Entry>),
    Delete(PathBuf, mpsc::Sender<std::io::Result<()>>),
    Flush(mpsc::Sender<()>),
}

fn summaries() -> &'static std::sync::Mutex<HashMap<PathBuf, bool>> {
    static SUMMARIES: OnceLock<std::sync::Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    SUMMARIES.get_or_init(Default::default)
}

/// Unknown on-disk history is kept conservatively; checking whether a session is empty must not parse its transcript.
pub fn has_saved_conversation(record: &str) -> bool {
    let path = entries_path(record);
    summaries().lock().unwrap().get(&path).copied().unwrap_or_else(|| path.is_file())
}

fn writer() -> &'static mpsc::Sender<Job> {
    static WRITER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut pending = HashMap::new();
            let mut next = None;
            while let Some(job) = next.take().or_else(|| rx.recv().ok()) {
                match job {
                    Job::Save(path, entries) => {
                        pending.insert(path, entries);
                        let deadline = Instant::now() + Duration::from_millis(40);
                        while std::time::Instant::now() < deadline && let Ok(job) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                            match job {
                                Job::Save(p, e) => { pending.insert(p, e); }
                                barrier => { next = Some(barrier); break; }
                            }
                        }
                        pending.retain(|path, entries| {
                            serde_json::to_string(entries).ok().is_none_or(|json| crate::datadir::write_atomic(path, &json).is_err())
                        });
                    }
                    Job::Delete(path, done) => {
                        pending.remove(&path);
                        let result = match std::fs::remove_file(path) {
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                            result => result,
                        };
                        let _ = done.send(result);
                    }
                    Job::Flush(done) => {
                        pending.retain(|path, entries| {
                            serde_json::to_string(entries).ok().is_none_or(|json| crate::datadir::write_atomic(path, &json).is_err())
                        });
                        let _ = done.send(());
                    }
                }
            }
        });
        tx
    })
}

pub fn save_entries(record: &str, entries: &[Entry]) {
    if !record.is_empty() && !entries.is_empty() {
        let path = entries_path(record);
        let conversation = entries.iter().any(|e| matches!(e, Entry::User(_) | Entry::Agent(_) | Entry::Tool(_)));
        summaries().lock().unwrap().insert(path.clone(), conversation);
        let _ = writer().send(Job::Save(path, entries.to_vec()));
    }
}

pub fn flush_entries() {
    let (done, wait) = mpsc::channel();
    if writer().send(Job::Flush(done)).is_ok() { let _ = wait.recv(); }
}

pub fn clear_entries(record: &str) -> std::io::Result<()> {
    if record.is_empty() { return Ok(()); }
    let (done, wait) = mpsc::channel();
    writer().send(Job::Delete(entries_path(record), done)).map_err(std::io::Error::other)?;
    wait.recv().map_err(std::io::Error::other)??;
    summaries().lock().unwrap().remove(&entries_path(record));
    Ok(())
}

/// Call from a background job. A preceding save is visible before this read.
pub fn load_entries(record: &str) -> Vec<Entry> {
    if record.is_empty() { return Vec::new(); }
    flush_entries();
    std::fs::read(entries_path(record)).ok().and_then(|text| serde_json::from_slice(&text).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_ordered_and_a_delete_is_a_barrier() {
        let _dirs = crate::datadir::use_throwaway_folders();
        for i in 0..100 { save_entries("writer-order", &[Entry::Agent(i.to_string())]); }
        assert_eq!(load_entries("writer-order"), [Entry::Agent("99".into())]);
        save_entries("writer-order", &[Entry::Agent("before deletion".into())]);
        clear_entries("writer-order").unwrap();
        flush_entries();
        assert!(!entries_path("writer-order").exists());
        save_entries("writer-order", &[Entry::Agent("new history".into())]);
        assert_eq!(load_entries("writer-order"), [Entry::Agent("new history".into())]);
    }
}
