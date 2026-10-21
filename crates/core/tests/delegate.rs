mod common;

use std::sync::Arc;
use std::time::Duration;

use claudiu_core::{Decision, SessionId, kind};
use common::*;
use serde_json::{Value, json};

fn receipt_block(summary: &str) -> String {
    format!("```receipt\n{}\n```", json!({"status": "done", "summary": summary, "refs": ["src/a.rs:10"], "changed": []}))
}

fn sdk_usage(id: &str, input: u64, write: u64, read: u64, output: u64) -> Value {
    json!({"notify": {"method": "_claude/sdkMessage", "params": {"message": {"type": "assistant", "message": {"id": id, "usage": {
        "input_tokens": input, "cache_creation_input_tokens": write, "cache_read_input_tokens": read, "output_tokens": output}}}}}})
}

fn scout(turns: Value) -> Value {
    json!({"profiles": {"scout": {"turns": turns}}})
}

fn brief(goal: &str) -> Value {
    json!({"profile": "scout", "goal": goal, "scope": "src/", "done": "the file and line", "pointers": ["src/lib.rs"]})
}

fn payload(events: &[claudiu_core::Event], kind: &str) -> Value {
    events.iter().find(|e| e.kind == kind).unwrap_or_else(|| panic!("no {kind} in {:?}", kinds(events))).payload.clone()
}

/// Every prompt text any adapter received, oldest first (the parent is never prompted in these tests).
fn prompts(h: &Harness) -> Vec<String> {
    h.adapter_calls("session/prompt").iter().filter_map(|p| p["prompt"][0]["text"].as_str().map(String::from)).collect()
}

fn in_parallel(h: &Arc<Harness>, parent: &SessionId, n: usize) -> Vec<std::thread::JoinHandle<String>> {
    (0..n)
        .map(|i| {
            let (h, parent) = (h.clone(), parent.clone());
            std::thread::spawn(move || call_tool(&h.core, &parent, "delegate", brief(&format!("task {i}"))))
        })
        .collect()
}

#[test]
fn a_scout_runs_as_its_own_trimmed_session_and_returns_a_receipt() {
    let h = Harness::new(scout(json!([{"steps": [sdk_usage("m1", 20, 3000, 500, 150), tool_call("t1", "x"), text_chunk(&receipt_block("auth lives in src/auth.rs"))]}])));
    let parent = h.core.start_session(&h.project()).unwrap();

    let out = call_tool(&h.core, &parent, "delegate", brief("Where is auth handled?"));

    assert!(out.contains("auth lives in src/auth.rs") && out.contains("src/a.rs:10"), "{out}");
    assert!(out.len() < 1_000, "the parent gets a short answer: {} bytes", out.len());

    let new = &h.adapter_calls("session/new")[1];
    let options = &new["_meta"]["claudeCode"]["options"];
    assert_eq!(options["tools"], json!(["Read", "Grep", "Glob"]), "the scout reads, nothing else");
    assert_eq!(options["settingSources"], json!([]), "no user settings");
    assert!(options["title"].as_str().unwrap().contains("scout"), "a title is always set");
    assert_eq!(new["systemPrompt"]["mode"], "append");
    assert!(new["systemPrompt"]["content"][0]["text"].as_str().unwrap().len() < 1_500, "a short profile prompt");
    assert_eq!(new["mcpServers"].as_array().unwrap().len(), 1, "only our server");
    assert_eq!(new["mcpServers"][0]["name"], "claudiu");

    let set = h.adapter_calls("session/set_config_option");
    assert!(set.iter().any(|c| c["configId"] == "model" && c["value"] == "haiku"), "{set:?}");
    assert!(set.iter().any(|c| c["configId"] == "effort" && c["value"] == "low"), "{set:?}");

    let record = h.core.sessions().unwrap().into_iter().find(|s| s.parent.as_ref() == Some(&parent)).expect("the child is a session under its parent");
    assert!(record.title.unwrap().contains("scout"));
}

#[test]
fn each_delegation_records_what_it_cost() {
    let h = Harness::new(scout(json!([{"steps": [
        sdk_usage("m1", 20, 3000, 500, 150), sdk_usage("m2", 30, 100, 3500, 250),
        tool_call("t1", "x"), tool_call("t2", "y"), text_chunk(&receipt_block("done")),
    ]}])));
    let parent = h.core.start_session(&h.project()).unwrap();
    call_tool(&h.core, &parent, "delegate", brief("Where is auth handled?"));

    let events = h.core.events(&parent).unwrap();
    let started = payload(&events, "delegation_started");
    assert_eq!(started["profile"], "scout");
    assert!(started["brief_tokens"].as_u64().unwrap() > 5);
    let ended = payload(&events, "delegation_ended");
    assert_eq!(ended["status"], "done");
    assert_eq!(ended["turns"], 1);
    assert_eq!(ended["tool_calls"], 2);
    assert_eq!(ended["over_budget"], false);
    assert_eq!(ended["cold_start_cache_write"], 3000, "the first request's cache write is the cold start");
    assert_eq!(ended["tokens"], json!({"input": 50, "cache_write": 3100, "cache_read": 4000, "output": 400}));
    assert!(ended["receipt_tokens"].as_u64().unwrap() > 5);
    assert_eq!(ended["child"], started["child"]);
}

#[test]
fn an_invalid_receipt_is_asked_for_once_more_then_the_child_is_marked_failed() {
    let h = Harness::new(scout(json!([
        {"steps": [text_chunk("I looked around and found it.")]},
        {"steps": [text_chunk("```receipt\n{\"status\": \"fine\"}\n```")]},
        {"steps": [text_chunk(&receipt_block("never asked a third time"))]},
    ])));
    let parent = h.core.start_session(&h.project()).unwrap();

    let out = call_tool(&h.core, &parent, "delegate", brief("Where is auth handled?"));

    assert!(out.to_lowercase().contains("failed") && !out.contains("never asked"), "{out}");
    let sent = prompts(&h);
    assert_eq!(sent.len(), 2, "one retry, no more: {sent:?}");
    assert!(sent[1].to_lowercase().contains("receipt"), "the retry says what was wrong: {}", sent[1]);
    assert_eq!(payload(&h.core.events(&parent).unwrap(), "delegation_ended")["status"], "failed");
}

#[test]
fn a_receipt_fixed_on_the_retry_is_accepted() {
    let h = Harness::new(scout(json!([
        {"steps": [text_chunk("no block here")]},
        {"steps": [text_chunk(&receipt_block("second try"))]},
    ])));
    let parent = h.core.start_session(&h.project()).unwrap();
    let out = call_tool(&h.core, &parent, "delegate", brief("Where is auth handled?"));
    assert!(out.contains("second try"), "{out}");
    let ended = payload(&h.core.events(&parent).unwrap(), "delegation_ended");
    assert_eq!((ended["status"].as_str(), ended["turns"].as_u64()), (Some("done"), Some(2)));
}

#[test]
fn a_child_over_its_token_budget_is_cancelled_given_one_grace_prompt_and_marked() {
    let h = Harness::new(scout(json!([
        {"steps": [sdk_usage("m1", 10, 2000, 0, 70_000), {"wait_cancel": true}]},
        {"steps": [text_chunk(&receipt_block("what I had so far"))]},
    ])));
    let parent = h.core.start_session(&h.project()).unwrap();

    let out = call_tool(&h.core, &parent, "delegate", brief("Map the whole repo"));

    assert!(out.contains("over budget") && out.contains("what I had so far"), "{out}");
    let sent = prompts(&h);
    assert_eq!(sent.len(), 2, "the brief and one grace prompt: {sent:?}");
    assert!(sent[1].contains("budget"), "{}", sent[1]);
    assert_eq!(h.adapter_calls("session/cancel").len(), 1, "cancelled once");
    let ended = payload(&h.core.events(&parent).unwrap(), "delegation_ended");
    assert_eq!((ended["status"].as_str(), ended["over_budget"].as_bool()), (Some("over_budget"), Some(true)));
}

#[test]
fn cache_reads_do_not_count_against_the_budget() {
    let h = Harness::new(scout(json!([{"steps": [sdk_usage("m1", 10, 500, 900_000, 100), text_chunk(&receipt_block("cheap"))]}])));
    let parent = h.core.start_session(&h.project()).unwrap();
    let out = call_tool(&h.core, &parent, "delegate", brief("Read a lot of cached context"));
    assert!(out.contains("cheap") && !out.contains("over budget"), "{out}");
    assert_eq!(h.adapter_calls("session/cancel").len(), 0);
}

#[test]
fn a_message_streamed_in_pieces_is_counted_once() {
    let h = Harness::new(scout(json!([{"steps": [
        sdk_usage("m1", 10, 1000, 0, 20_000), sdk_usage("m1", 10, 1000, 0, 40_000), sdk_usage("m1", 10, 1000, 0, 50_000),
        text_chunk(&receipt_block("one message, three updates")),
    ]}])));
    let parent = h.core.start_session(&h.project()).unwrap();
    let out = call_tool(&h.core, &parent, "delegate", brief("Think hard"));
    assert!(out.contains("one message, three updates") && !out.contains("over budget"), "{out}");
}

#[test]
fn same_profile_siblings_start_one_after_the_other_so_the_first_prefix_is_cached() {
    let slow_start = json!([{"steps": [{"sleep_ms": 300}, text_chunk("starting"), text_chunk(&receipt_block("ok"))]}]);
    let h = Arc::new(Harness::new(scout(slow_start)));
    let rx = h.core.subscribe();
    let parent = h.core.start_session(&h.project()).unwrap();
    for call in in_parallel(&h, &parent, 2) {
        assert!(call.join().unwrap().contains("ok"));
    }

    let mut order = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if e.session != parent && e.kind == kind::SESSION_STARTED {
            order.push("start");
        } else if e.session != parent && e.kind == kind::AGENT_UPDATE && e.payload["sessionUpdate"] == "agent_message_chunk" {
            order.push("chunk");
        }
    }
    let pos = |what: &str, n: usize| order.iter().enumerate().filter(|(_, o)| **o == what).nth(n).map(|(i, _)| i).unwrap();
    assert!(pos("start", 1) > pos("chunk", 0), "the second child waited for the first one's first chunk: {order:?}");
}

#[test]
fn a_child_that_never_streams_holds_its_sibling_only_until_the_fallback() {
    let silent = json!([{"steps": [{"sleep_ms": 2500}, text_chunk(&receipt_block("late"))]}]);
    let h = Arc::new(Harness::with(scout(silent), |c| c.with_stagger_fallback(Duration::from_millis(300))));
    let rx = h.core.subscribe();
    let parent = h.core.start_session(&h.project()).unwrap();
    let began = std::time::Instant::now();
    let calls = in_parallel(&h, &parent, 2);
    let mut starts = 0;
    while starts < 2 {
        let e = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        starts += usize::from(e.kind == kind::SESSION_STARTED && e.session != parent);
    }
    assert!(began.elapsed() < Duration::from_millis(2200), "the second child did not wait for a chunk that never came: {:?}", began.elapsed());
    for c in calls {
        c.join().unwrap();
    }
}

#[test]
fn interrupting_the_parent_cancels_its_children() {
    let h = Arc::new(Harness::new(scout(json!([{"steps": [{"wait_cancel": true}]}]))));
    let rx = h.core.subscribe();
    let parent = h.core.start_session(&h.project()).unwrap();
    let call = in_parallel(&h, &parent, 1).pop().unwrap();
    until(&rx, "delegation_started");

    h.core.cancel(&parent).unwrap();

    let out = call.join().unwrap();
    assert!(out.to_lowercase().contains("cancelled"), "{out}");
    // The parent's adapter reads its notice on its own time.
    let began = std::time::Instant::now();
    while h.adapter_calls("session/cancel").len() < 2 && began.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(h.adapter_calls("session/cancel").len() >= 2, "the parent and its child were told to stop");
    assert_eq!(payload(&h.core.events(&parent).unwrap(), "delegation_ended")["status"], "cancelled");
}

#[test]
fn a_childs_access_request_is_the_parents_to_answer_and_covers_both() {
    let h = Arc::new(Harness::new(scout(json!([{"steps": [{"wait_cancel": true}]}]))));
    let rx = h.core.subscribe();
    let parent = h.core.start_session(&h.project()).unwrap();
    let call = in_parallel(&h, &parent, 1).pop().unwrap();
    let started = until(&rx, "delegation_started");
    let child = SessionId(started.last().unwrap().payload["child"].as_str().unwrap().to_string());
    let outside = h.dir.path().join("elsewhere").join("out.txt");

    let answer = call_tool(&h.core, &child, "request_access", json!({"path": outside.to_string_lossy(), "reason": "write the report"}));
    assert!(answer.contains("Pending"), "{answer}");

    let waiting = h.core.pending_access(&parent);
    assert_eq!(waiting.len(), 1, "it surfaces on the parent task");
    assert_eq!(h.core.pending_access(&child).len(), 0);
    let write = || decision(&pre(&h.core, &child, "Write", json!({"file_path": outside.to_string_lossy(), "content": "x"}))).map(String::from);
    assert_eq!(write().as_deref(), Some("deny"), "refused until the user answers");

    h.core.resolve_access(&parent, waiting[0].id, Decision::Task).unwrap();
    assert_eq!(write(), None, "the parent's grant lets the child through");

    h.core.cancel(&parent).unwrap();
    call.join().unwrap();
}

#[test]
fn a_background_delegation_returns_at_once_and_wakes_the_parent_with_the_receipt() {
    let h = Harness::new(scout(json!([{"steps": [text_chunk(&receipt_block("found it in the background"))]}])));
    let rx = h.core.subscribe();
    let parent = h.core.start_session(&h.project()).unwrap();

    let mut args = brief("Look around");
    args["background"] = json!(true);
    let out = call_tool(&h.core, &parent, "delegate", args);
    assert!(out.to_lowercase().contains("started") && !out.contains("found it"), "{out}");

    let woken = loop {
        let e = until(&rx, "user_prompt").pop().unwrap();
        if e.session == parent {
            break e;
        }
    };
    assert!(woken.payload["text"].as_str().unwrap().contains("found it in the background"), "{woken:?}");
}

#[test]
fn bad_calls_are_refused_in_one_line_and_start_nothing() {
    let h = Harness::new(json!({}));
    let parent = h.core.start_session(&h.project()).unwrap();

    let unknown = call_tool(&h.core, &parent, "delegate", json!({"profile": "wizard", "goal": "x", "done": "y"}));
    assert!(unknown.contains("scout") && unknown.contains("verifier"), "{unknown}");

    let huge = call_tool(&h.core, &parent, "delegate", json!({"profile": "scout", "goal": "x".repeat(8_000), "done": "y"}));
    assert!(huge.contains("1500") && huge.to_lowercase().contains("pointer"), "{huge}");

    let missing = call_tool(&h.core, &parent, "delegate", json!({"profile": "scout"}));
    assert!(missing.to_lowercase().contains("goal"), "{missing}");

    assert_eq!(h.adapter_calls("session/new").len(), 1, "no child was started");
}

#[test]
fn every_profile_has_its_own_tools_model_and_effort() {
    let quick = json!({"turns": [{"steps": [text_chunk(&receipt_block("ok"))]}]});
    let h = Harness::new(json!({"profiles": {"thinker": quick, "worker": quick, "verifier": quick}}));
    let parent = h.core.start_session(&h.project()).unwrap();
    let all = [
        ("thinker", json!(["Read", "Grep", "Glob"]), "opus", "high"),
        ("worker", json!(["Read", "Grep", "Glob", "Bash", "Edit", "Write"]), "sonnet", "medium"),
        ("verifier", json!(["Read", "Grep", "Glob", "Bash"]), "sonnet", "low"),
    ];
    for (profile, ..) in &all {
        call_tool(&h.core, &parent, "delegate", json!({"profile": profile, "goal": "g", "done": "d"}));
    }
    let created = h.adapter_calls("session/new");
    let set = h.adapter_calls("session/set_config_option");
    for (i, (profile, tools, model, effort)) in all.iter().enumerate() {
        assert_eq!(&created[i + 1]["_meta"]["claudeCode"]["options"]["tools"], tools, "{profile}");
        assert!(set.iter().any(|c| c["configId"] == "model" && c["value"] == *model), "{profile} model");
        assert!(set.iter().any(|c| c["configId"] == "effort" && c["value"] == *effort), "{profile} effort");
    }
}
