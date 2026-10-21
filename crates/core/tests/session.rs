mod common;

use common::*;
use serde_json::json;

#[test]
fn a_session_streams_its_events_in_order() {
    let h = Harness::new(json!({"turns": [{"steps": [text_chunk("hel"), text_chunk("lo")]}]}));
    let rx = h.core.subscribe();
    let id = h.core.start_session(&h.project()).unwrap();
    h.core.send_prompt(&id, "say hello").unwrap();

    let seen = until(&rx, "turn_ended");
    assert_eq!(kinds(&seen), ["session_started", "user_prompt", "agent_update", "agent_update", "turn_ended"]);
    assert_eq!(seen[1].payload["text"], "say hello");
    assert_eq!(seen[4].payload["stop_reason"], "end_turn");
    assert!(seen.windows(2).all(|w| w[0].id < w[1].id), "event ids grow");
}

fn hanging_turn() -> serde_json::Value {
    json!({"turns": [{"steps": [text_chunk("working"), {"wait_cancel": true}]}]})
}

#[test]
fn sessions_and_events_survive_a_restart() {
    let h = Harness::new(json!({"turns": [{"steps": [text_chunk("done")]}]}));
    let rx = h.core.subscribe();
    let id = h.core.start_session(&h.project()).unwrap();
    h.core.send_prompt(&id, "go").unwrap();
    until(&rx, "turn_ended");
    let before = kinds_of(&h.core.events(&id).unwrap());

    let h = h.reopen(json!({}));

    let sessions = h.core.sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, id);
    assert!(sessions[0].cwd.ends_with("project"));
    assert_eq!(kinds_of(&h.core.events(&id).unwrap()), before);
}

#[test]
fn cancelling_a_running_turn_records_an_interruption() {
    let h = Harness::new(hanging_turn());
    let rx = h.core.subscribe();
    let id = h.core.start_session(&h.project()).unwrap();
    h.core.send_prompt(&id, "go").unwrap();
    until(&rx, "agent_update");

    h.core.cancel(&id).unwrap();

    let seen = until(&rx, "interrupted");
    assert_eq!(seen.last().unwrap().payload["stop_reason"], "cancelled");
    let stored = kinds_of(&h.core.events(&id).unwrap());
    assert_eq!(stored.last().map(String::as_str), Some("interrupted"));
    assert!(!stored.contains(&"turn_ended".to_string()));
    // The session takes the next prompt after an interruption.
    h.core.send_prompt(&id, "again").unwrap();
}

#[test]
fn quitting_mid_turn_records_the_interruption_and_flushes() {
    let h = Harness::new(hanging_turn());
    let rx = h.core.subscribe();
    let id = h.core.start_session(&h.project()).unwrap();
    h.core.send_prompt(&id, "go").unwrap();
    until(&rx, "agent_update");

    let h = h.reopen(json!({}));

    let stored = kinds_of(&h.core.events(&id).unwrap());
    assert_eq!(stored.last().map(String::as_str), Some("interrupted"));
    assert!(!stored.contains(&"adapter_exit".to_string()), "a deliberate quit is not an adapter crash");
}

fn kinds_of(events: &[claudiu_core::Event]) -> Vec<String> {
    events.iter().map(|e| e.kind.clone()).collect()
}
