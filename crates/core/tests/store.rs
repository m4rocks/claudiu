mod common;

use common::*;
use serde_json::json;

const DAY: i64 = 24 * 60 * 60 * 1000;

/// The first shipped schema, written out by hand so the upgrade test does not depend on the code under test.
fn write_version_one(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT NOT NULL, title TEXT, adapter_session_id TEXT, created_ms INTEGER NOT NULL);
         CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, ts_ms INTEGER NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL);
         CREATE INDEX events_by_session ON events (session_id, id);
         INSERT INTO sessions VALUES ('old', 'C:/work', 'Old task', 'adapter-old', 1000);
         INSERT INTO events (session_id, ts_ms, kind, payload) VALUES ('old', 1001, 'user_prompt', '{\"text\":\"hi\"}');
         PRAGMA user_version = 1;",
    )
    .unwrap();
}

#[test]
fn data_from_an_older_schema_is_upgraded_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    write_version_one(&dir.path().join("data").join("core.db"));

    let now = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(T0));
    let core = claudiu_core::Core::open(config(dir.path(), &json!({"turns": [{"steps": [usage(1200, 4000)]}]}), &now)).unwrap();

    let sessions = core.sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title.as_deref(), Some("Old task"));
    let old = claudiu_core::SessionId("old".into());
    assert_eq!(core.events(&old).unwrap()[0].payload["text"], "hi");

    // The tables added by later versions work on the upgraded file.
    let rx = core.subscribe();
    let id = core.start_session(dir.path()).unwrap();
    core.send_prompt(&id, "go").unwrap();
    until(&rx, "turn_ended");
    assert_eq!(core.usage(&id).unwrap().len(), 1);
}

#[test]
fn data_from_a_newer_version_is_refused_not_damaged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let path = dir.path().join("data").join("core.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);

    let now = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(T0));
    let opened = claudiu_core::Core::open(config(dir.path(), &json!({}), &now));

    assert!(opened.is_err_and(|e| e.to_string().contains("newer version")));
}

#[test]
fn usage_is_recorded_per_turn_and_kept_forever() {
    let h = Harness::new(json!({"turns": [{"steps": [usage(28174, 400000)]}]}));
    let rx = h.core.subscribe();
    let id = h.core.start_session(&h.project()).unwrap();
    h.core.send_prompt(&id, "go").unwrap();
    until(&rx, "turn_ended");

    h.advance(365 * DAY);
    h.core.prune_expired().unwrap();

    let rows = h.core.usage(&id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].used, rows[0].size), (Some(28174), Some(400000)));
    assert_eq!(rows[0].ts_ms, T0);
}

#[test]
fn bodies_expire_on_schedule_and_the_rest_stays() {
    let h = Harness::new(json!({"turns": [{"steps": [tool_call("call-1", "big output")]}]}));
    let rx = h.core.subscribe();
    let id = h.core.start_session(&h.project()).unwrap();
    h.core.send_prompt(&id, "go").unwrap();
    until(&rx, "turn_ended");
    let blob = h.core.put_blob(&id, b"full tool output").unwrap();

    h.advance(13 * DAY);
    h.core.prune_expired().unwrap();
    assert_eq!(h.core.blob(blob).unwrap().as_deref(), Some(&b"full tool output"[..]), "a blob lives 14 days");

    h.advance(2 * DAY);
    h.core.prune_expired().unwrap();
    assert_eq!(h.core.blob(blob).unwrap(), None);
    let call = |h: &Harness| h.core.events(&id).unwrap().into_iter().find(|e| e.kind == "tool_call").unwrap();
    assert_eq!(call(&h).payload["toolCallId"], "call-1", "a tool call body lives 30 days");

    h.advance(16 * DAY);
    h.core.prune_expired().unwrap();
    assert_eq!(call(&h).payload, json!({"expired": true}));
    let kinds: Vec<_> = h.core.events(&id).unwrap().into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds, ["session_started", "user_prompt", "tool_call", "turn_ended"], "only bodies go, never the events");
}
