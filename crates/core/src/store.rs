//! The SQLite store: WAL mode, one writer thread, read-only connections for queries.
//!
//! Writes are closures sent to the writer thread, which runs them in order; reads open their own read-only
//! connection, so a long query never holds up a write. The schema version lives in `PRAGMA user_version` and
//! only ever moves forward.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;

use crate::event::{Event, SessionId, SessionRecord, UsageRow};

/// Forward-only: append a new entry, never edit one that shipped.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE sessions (
        id TEXT PRIMARY KEY,
        cwd TEXT NOT NULL,
        title TEXT,
        adapter_session_id TEXT,
        created_ms INTEGER NOT NULL
    );
    CREATE TABLE events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL,
        ts_ms INTEGER NOT NULL,
        kind TEXT NOT NULL,
        payload TEXT NOT NULL
    );
    CREATE INDEX events_by_session ON events (session_id, id);",
    "CREATE TABLE usage (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL,
        ts_ms INTEGER NOT NULL,
        used INTEGER,
        size INTEGER,
        raw TEXT NOT NULL
    );
    CREATE INDEX usage_by_session ON usage (session_id, id);
    CREATE TABLE blobs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL,
        ts_ms INTEGER NOT NULL,
        data BLOB NOT NULL
    );",
];

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// Tool-call and receipt bodies are dropped after this long; usage rows are never dropped.
pub const BODY_RETENTION_MS: i64 = 30 * DAY_MS;
/// Full tool output kept aside for `expand` is dropped after this long.
pub const BLOB_RETENTION_MS: i64 = 14 * DAY_MS;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

pub struct Store {
    path: PathBuf,
    jobs: Mutex<Option<Sender<Job>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(10))?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let version: usize = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > MIGRATIONS.len() {
        bail!("this data was written by a newer version (schema {version}, this build knows {})", MIGRATIONS.len());
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i + 1)?;
        tx.commit()?;
    }
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let mut conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        configure(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn)?;
        let (tx, rx) = channel::<Job>();
        let writer = std::thread::Builder::new().name("store-writer".into()).spawn(move || {
            for job in rx {
                job(&mut conn);
            }
        })?;
        Ok(Store { path: path.to_path_buf(), jobs: Mutex::new(Some(tx)), writer: Mutex::new(Some(writer)) })
    }

    fn write<R: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> Result<R> + Send + 'static) -> Result<R> {
        let (reply, answer) = sync_channel(1);
        let job: Job = Box::new(move |conn| {
            let _ = reply.send(f(conn));
        });
        self.jobs.lock().unwrap().as_ref().ok_or_else(|| anyhow!("the store is closed"))?.send(job).map_err(|_| anyhow!("the store writer stopped"))?;
        answer.recv().map_err(|_| anyhow!("the store writer stopped"))?
    }

    fn read<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        configure(&conn)?;
        f(&conn)
    }

    /// Returns once every write queued before it has reached the database.
    pub fn flush(&self) -> Result<()> {
        self.write(|_| Ok(()))
    }

    pub fn insert_session(&self, rec: &SessionRecord) -> Result<()> {
        let rec = rec.clone();
        self.write(move |c| {
            c.execute(
                "INSERT INTO sessions (id, cwd, title, adapter_session_id, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![rec.id.0, rec.cwd, rec.title, rec.adapter_session_id, rec.created_ms],
            )?;
            Ok(())
        })
    }

    pub fn append_event(&self, session: &SessionId, ts_ms: i64, kind: &str, payload: &Value) -> Result<i64> {
        let (session, kind, payload) = (session.0.clone(), kind.to_string(), payload.to_string());
        self.write(move |c| {
            c.execute("INSERT INTO events (session_id, ts_ms, kind, payload) VALUES (?1, ?2, ?3, ?4)", params![session, ts_ms, kind, payload])?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn record_usage(&self, session: &SessionId, ts_ms: i64, used: Option<i64>, size: Option<i64>, raw: &Value) -> Result<()> {
        let (session, raw) = (session.0.clone(), raw.to_string());
        self.write(move |c| {
            c.execute("INSERT INTO usage (session_id, ts_ms, used, size, raw) VALUES (?1, ?2, ?3, ?4, ?5)", params![session, ts_ms, used, size, raw])?;
            Ok(())
        })
    }

    pub fn usage(&self, session: &SessionId) -> Result<Vec<UsageRow>> {
        self.read(|c| {
            let mut q = c.prepare("SELECT ts_ms, used, size, raw FROM usage WHERE session_id = ?1 ORDER BY id")?;
            let rows = q.query_map([&session.0], |r| {
                let raw: String = r.get(3)?;
                Ok(UsageRow { ts_ms: r.get(0)?, used: r.get(1)?, size: r.get(2)?, raw: serde_json::from_str(&raw).unwrap_or(Value::Null) })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    pub fn put_blob(&self, session: &SessionId, ts_ms: i64, data: &[u8]) -> Result<i64> {
        let (session, data) = (session.0.clone(), data.to_vec());
        self.write(move |c| {
            c.execute("INSERT INTO blobs (session_id, ts_ms, data) VALUES (?1, ?2, ?3)", params![session, ts_ms, data])?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn blob(&self, id: i64) -> Result<Option<Vec<u8>>> {
        self.read(|c| Ok(c.query_row("SELECT data FROM blobs WHERE id = ?1", [id], |r| r.get(0)).optional()?))
    }

    /// Drops what has outlived its retention: old tool-call and receipt bodies (the event stays, its payload goes)
    /// and old blobs. Cost and every other event are kept.
    pub fn prune(&self, now_ms: i64) -> Result<()> {
        self.write(move |c| {
            c.execute(
                "UPDATE events SET payload = '{\"expired\":true}' WHERE kind IN ('tool_call', 'receipt') AND ts_ms < ?1 AND payload != '{\"expired\":true}'",
                [now_ms - BODY_RETENTION_MS],
            )?;
            c.execute("DELETE FROM blobs WHERE ts_ms < ?1", [now_ms - BLOB_RETENTION_MS])?;
            Ok(())
        })
    }

    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        self.read(|c| {
            let mut q = c.prepare("SELECT id, cwd, title, adapter_session_id, created_ms FROM sessions ORDER BY created_ms, rowid")?;
            let rows = q.query_map([], |r| {
                Ok(SessionRecord { id: SessionId(r.get(0)?), cwd: r.get(1)?, title: r.get(2)?, adapter_session_id: r.get(3)?, created_ms: r.get(4)? })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    pub fn events(&self, session: &SessionId) -> Result<Vec<Event>> {
        self.read(|c| {
            let mut q = c.prepare("SELECT id, ts_ms, kind, payload FROM events WHERE session_id = ?1 ORDER BY id")?;
            let rows = q.query_map([&session.0], |r| {
                let payload: String = r.get(3)?;
                Ok(Event { id: r.get(0)?, session: session.clone(), ts_ms: r.get(1)?, kind: r.get(2)?, payload: serde_json::from_str(&payload).unwrap_or(Value::Null) })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Closing the queue lets the writer finish what is queued and stop.
        self.jobs.lock().unwrap().take();
        if let Some(writer) = self.writer.lock().unwrap().take() {
            let _ = writer.join();
        }
    }
}
