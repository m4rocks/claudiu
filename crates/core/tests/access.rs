mod common;

use claudiu_core::Decision;
use common::*;
use serde_json::json;

fn write_outside(h: &Harness, id: &claudiu_core::SessionId, name: &str) -> serde_json::Value {
    pre(&h.core, id, "Write", json!({"file_path": h.dir.path().join("shared").join(name), "content": "x"}))
}

#[test]
fn the_claudiu_server_is_offered_to_the_adapter_with_few_tokens() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();

    let servers = &h.adapter_calls("session/new")[0]["mcpServers"];
    assert_eq!(servers[0]["name"], "claudiu");
    assert_eq!(servers[0]["type"], "http");
    assert_eq!(servers[0]["url"], h.core.mcp_url(&id));

    let init = mcp(&h.core, &id, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}));
    assert_eq!(init["serverInfo"]["name"], "claudiu");
    let tools = mcp(&h.core, &id, "tools/list", json!({}));
    let tools = tools["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "request_access"));
    for t in tools {
        let words = t["description"].as_str().unwrap().len() / 4;
        assert!(words <= 40, "{} costs about {words} tokens to describe", t["name"]);
    }
    let all = serde_json::to_string(&tools).unwrap().len() / 4;
    assert!(all <= 700, "the tools cost about {all} tokens in total");
}

#[test]
fn access_is_requested_without_blocking_and_granted_for_the_task_or_the_project() {
    let h = Harness::new(json!({}));
    let rx = h.core.subscribe();
    let id = h.core.start_session(&h.project()).unwrap();
    assert_eq!(decision(&write_outside(&h, &id, "a.txt")), Some("deny"));

    let shared = h.dir.path().join("shared");
    let answer = call_tool(&h.core, &id, "request_access", json!({"path": shared, "reason": "the build output goes there"}));
    assert!(answer.to_lowercase().contains("pending"), "returns at once: {answer}");
    let seen = until(&rx, "access_requested");
    assert_eq!(seen.last().unwrap().payload["reason"], "the build output goes there");
    let pending = h.core.pending_access(&id);
    assert_eq!(pending.len(), 1);
    assert_eq!(decision(&write_outside(&h, &id, "a.txt")), Some("deny"), "nothing changes until the user answers");

    // Approved for this task: the next call goes through, with no relaunch.
    h.core.resolve_access(&id, pending[0].id, Decision::Task).unwrap();
    assert_eq!(decision(&write_outside(&h, &id, "a.txt")), None);
    assert!(h.core.pending_access(&id).is_empty());

    // A task in the same project does not get the task grant...
    let other = h.core.start_session(&h.project()).unwrap();
    assert_eq!(decision(&write_outside(&h, &other, "a.txt")), Some("deny"));
    // ...but does get a project grant.
    call_tool(&h.core, &other, "request_access", json!({"path": shared, "reason": "again"}));
    let req = h.core.pending_access(&other)[0].id;
    h.core.resolve_access(&other, req, Decision::Project).unwrap();
    assert_eq!(decision(&write_outside(&h, &other, "b.txt")), None);
    let third = h.core.start_session(&h.project()).unwrap();
    assert_eq!(decision(&write_outside(&h, &third, "c.txt")), None, "later tasks of the project inherit it");
}

#[test]
fn a_decision_is_told_to_the_model_once_in_the_next_prompt() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let shared = h.dir.path().join("shared");
    call_tool(&h.core, &id, "request_access", json!({"path": shared, "reason": "r"}));
    let req = h.core.pending_access(&id)[0].id;
    h.core.resolve_access(&id, req, Decision::Deny).unwrap();
    assert_eq!(decision(&write_outside(&h, &id, "a.txt")), Some("deny"), "a refusal stays a refusal");

    let prompt = |h: &Harness| post(&h.core.hook_url(&id, "prompt"), &json!({"hook_event_name": "UserPromptSubmit"})).1;
    let first = prompt(&h);
    let note = first["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or("");
    assert!(note.contains("declined") && note.contains("shared"), "{note}");
    assert!(!note.contains('\n'), "one line");
    assert_eq!(prompt(&h)["hookSpecificOutput"]["additionalContext"], serde_json::Value::Null, "told once");
}

#[test]
fn a_request_for_a_place_already_allowed_is_answered_at_once() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let answer = call_tool(&h.core, &id, "request_access", json!({"path": h.project().join("src"), "reason": "r"}));
    assert!(answer.to_lowercase().contains("already"), "{answer}");
    assert!(h.core.pending_access(&id).is_empty());
}
