use super::*;

fn request(agent: Agent, edits: bool) -> Request {
    Request { agent, model: Some("gpt-5.5".into()), edits, cwd: PathBuf::from("/work"), prompt: "Find the bug".into(), effort: None, live_messages: false }
}

#[test]
fn codex_gets_the_effort_claude_asked_for_and_max_becomes_xhigh() {
    assert_eq!(codex_config(Some("gpt-5.5"), Some("high")), Some(json!({ "model": "gpt-5.5", "model_reasoning_effort": "high" })));
    assert_eq!(codex_config(None, Some("max")), Some(json!({ "model_reasoning_effort": "xhigh" })));
    assert_eq!(codex_config(Some("gpt-5.5"), Some("turbo")), Some(json!({ "model": "gpt-5.5" })), "an unknown level is left to the user's config");
    assert_eq!(codex_config(None, None), None);
}

fn update(update: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s1", "update": update } })
}

fn chunk(id: &str, text: &str) -> Value {
    update(json!({ "sessionUpdate": "agent_message_chunk", "messageId": id, "content": { "type": "text", "text": text } }))
}

fn reply(id: i64, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Runs the conversation against what a fake agent says, in order. Returns the result, what was sent to the agent,
/// and the events.
fn talk_to(agent: Agent, agent_says: &[Value], edits: bool) -> (Result<String, String>, Vec<Value>, Vec<Event>) {
    talk_request(&request(agent, edits), agent_says)
}

fn talk_request(request: &Request, agent_says: &[Value]) -> (Result<String, String>, Vec<Value>, Vec<Event>) {
    let script: String = agent_says.iter().map(|m| format!("{m}\n")).collect();
    let mut sent = Vec::new();
    let mut events = vec![];
    let result = converse(std::io::Cursor::new(script), &mut sent, request, None, &mut |e| events.push(e));
    let sent = String::from_utf8(sent).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    (result, sent, events)
}

#[test]
fn antigravity_configures_each_option_before_prompting() {
    for effort in ["low", "medium", "high", "xhigh", "max", "unknown"] {
        for edits in [false, true] {
            let mut request = request(Agent::Antigravity, edits);
            request.model = Some("gemini-4-neon".into());
            request.effort = Some(effort.into());
            let mut expected = vec![("model", "gemini-4-neon")];
            if effort != "unknown" { expected.push(("thought_level", if effort == "max" { "xhigh" } else { effort })); }
            if edits { expected.push(("mode", "accept-edits")); }
            let mut says = opening();
            for i in 0..expected.len() { says.push(reply(2 + i as i64, json!({}))); }
            says.push(chunk("m", "Done"));
            says.push(reply(2 + expected.len() as i64, json!({ "stopReason": "end_turn" })));
            let (result, sent, _) = talk_request(&request, &says);
            assert_eq!(result, Ok("Done".into()));
            for (i, (config, value)) in expected.iter().enumerate() {
                assert_eq!(sent[i + 2], json!({ "jsonrpc": "2.0", "id": i + 2, "method": "session/set_config_option", "params": { "sessionId": "s1", "configId": config, "value": value } }));
            }
            assert_eq!(sent.last().unwrap()["method"], "session/prompt");
            assert_eq!(sent.len(), expected.len() + 3);
        }
    }
}

/// What agy 1.5.12 does: Gemini 3.8 Flash has no `xhigh`, and the answer to the model change says so.
#[test]
fn a_thinking_level_the_model_lacks_becomes_the_highest_it_offers() {
    let levels = |values: &[&str]| json!([{ "id": "thought_level", "category": "thought_level", "options": values.iter().map(|v| json!({ "value": v })).collect::<Vec<_>>() }]);
    let mut request = request(Agent::Antigravity, false);
    request.model = Some("gemini-3.8-flash".into());
    request.effort = Some("max".into());
    let says = [
        reply(0, json!({ "protocolVersion": 1 })),
        reply(1, json!({ "sessionId": "s1", "configOptions": levels(&["low", "medium", "high", "xhigh"]) })),
        reply(2, json!({ "configOptions": levels(&["low", "medium", "high"]) })),
        reply(3, json!({})),
        chunk("m", "Done"),
        reply(4, json!({ "stopReason": "end_turn" })),
    ];
    let (result, sent, _) = talk_request(&request, &says);
    assert_eq!(result, Ok("Done".into()));
    assert_eq!((&sent[3]["params"]["configId"], &sent[3]["params"]["value"]), (&json!("thought_level"), &json!("high")));
    assert_eq!(offered_value(&levels(&["medium", "high"]), "thought_level", "low"), None, "no level at or below: the model's default");
    assert_eq!(offered_value(&levels(&["low"]), "model", "x"), Some("x".into()), "other options go as asked");
}

#[test]
fn antigravity_config_failure_never_prompts() {
    let mut says = opening();
    says.push(json!({ "id": 2, "error": { "code": -32602, "message": "unknown model" } }));
    let (result, sent, _) = talk_to(Agent::Antigravity, &says, false);
    assert_eq!(result, Err("Antigravity could not set model: agy: unknown model".into()));
    assert!(sent.iter().all(|m| m["method"] != "session/prompt"));
}

#[test]
fn antigravity_denies_string_permission_ids_and_keeps_queued_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request(Agent::Antigravity, false);
    request.model = None;
    let mut says = opening();
    says.push(json!({ "jsonrpc": "2.0", "id": "1", "method": "session/request_permission", "params": { "options": [{ "optionId": "allow", "kind": "allow_once" }, { "optionId": "deny", "kind": "reject_once" }] } }));
    says.push(chunk("m", "Read only"));
    says.push(reply(2, json!({ "stopReason": "end_turn" })));
    let script: String = says.iter().map(|m| format!("{m}\n")).collect();
    let mut sent = vec![];
    let mut events = vec![];
    let result = converse(std::io::Cursor::new(script), &mut sent, &request, Some(test_inbox(dir.path())), &mut |e| events.push(e));
    assert_eq!(result, Ok("Read only".into()));
    let sent: Vec<Value> = String::from_utf8(sent).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(sent[3]["id"], "1");
    assert_eq!(sent[3]["result"]["outcome"]["optionId"], "deny");
    assert!(!events.iter().any(|e| matches!(e, Event::Inbox { .. })));
}

#[test]
fn antigravity_file_targets_and_sign_in_hint() {
    assert_eq!(tool_target(&json!({ "Path": "Cargo.toml" })), Some("Cargo.toml"));
    assert_eq!(error_reason(Agent::Antigravity, &json!({ "code": AUTH_REQUIRED })), "Antigravity isn't signed in. Run `agy` and sign in, then try again.");
}

fn talk(agent_says: &[Value], edits: bool) -> (Result<String, String>, Vec<Value>, Vec<Event>) {
    talk_to(Agent::Codex, agent_says, edits)
}

/// What agy 1.5 does with a command that outlasts its wait: it ends the turn while the command runs, then carries
/// on by itself once it finishes, ending with a usage update and no stop reason.
#[test]
fn antigravity_finishes_its_turn_after_a_command_it_moved_to_the_background() {
    let command = |status: &str| update(json!({ "sessionUpdate": if status == "in_progress" { "tool_call" } else { "tool_call_update" }, "toolCallId": "c1", "title": "cargo test", "status": status }));
    let usage = update(json!({ "sessionUpdate": "usage_update", "used": 9837, "size": 1048576 }));
    // Antigravity sets its model and accept-edits mode first, so its prompt is request 4; Codex's is request 2.
    let says = |prompt: i64| {
        let mut says = opening();
        says.extend((2..prompt).map(|id| reply(id, json!({}))));
        says.extend([command("in_progress"), chunk("m1", "Waiting for the command to finish."), usage.clone(), reply(prompt, json!({ "stopReason": "end_turn" })), command("completed"), chunk("m2", "All 540 tests pass."), usage.clone()]);
        says
    };
    let (result, _, events) = talk_to(Agent::Antigravity, &says(4), true);
    assert_eq!(result, Ok("All 540 tests pass.".into()));
    assert!(events.iter().any(|e| matches!(e, Event::Progress { text } if text.contains("background"))));
    // Codex's turn ends at its stop reason, even with a tool call left unfinished.
    let (result, _, _) = talk_to(Agent::Codex, &says(2), true);
    assert_eq!(result, Ok("Waiting for the command to finish.".into()));
}

/// A command that is still running when the wait ends leaves only the interim text, which isn't the report.
#[test]
fn antigravity_fails_when_a_command_it_moved_to_the_background_outlasts_the_wait() {
    let command = update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "cargo test", "status": "in_progress" }));
    let mut says = opening();
    says.extend((2..4).map(|id| reply(id, json!({}))));
    says.extend([command, chunk("m1", "Waiting for the command to finish."), reply(4, json!({ "stopReason": "end_turn" })), chunk("m2", "Still running the tests.")]);
    let script: String = says.iter().map(|m| format!("{m}\n")).collect();
    // The agent stays connected and quiet, as it does while the command runs.
    let (from_agent, mut agent) = std::io::pipe().unwrap();
    agent.write_all(script.as_bytes()).unwrap();
    let result = converse_waiting(BufReader::new(from_agent), std::io::sink(), &request(Agent::Antigravity, true), None, &mut |_| {}, Duration::from_millis(300));
    assert_eq!(result, Err("Antigravity's command running in the background didn't finish within the wait".into()));
}

fn opening() -> Vec<Value> {
    vec![reply(0, json!({ "protocolVersion": 1 })), reply(1, json!({ "sessionId": "s1" }))]
}

fn test_inbox(dir: &Path) -> Inbox {
    Inbox { id: Uuid::new_v4(), path: dir.join("inbox.json"), seen: 0 }
}

#[test]
fn the_inbox_rejects_partial_oversized_out_of_order_and_replayed_deliveries() {
    let dir = tempfile::tempdir().unwrap();
    let mut inbox = test_inbox(dir.path());
    for body in [
        r#"{"seq":1,"text":"unfinished"#.to_string(),
        json!({ "seq": 1, "text": "   " }).to_string(),
        json!({ "seq": 1, "text": 15 }).to_string(),
        json!({ "seq": 2, "text": "skipped one" }).to_string(),
        json!({ "seq": 1, "text": "a".repeat(INBOX_TEXT_MAX + 1) }).to_string(),
        " ".repeat(INBOX_BYTES as usize + 1),
    ] {
        std::fs::write(&inbox.path, body).unwrap();
        assert_eq!(inbox.take(), None);
    }
    let message = json!({ "seq": 1, "text": "Check the tests too" }).to_string();
    std::fs::write(&inbox.path, &message).unwrap();
    assert_eq!(inbox.take(), Some((1, "Check the tests too".to_string())));
    assert!(!inbox.path.exists());
    std::fs::write(&inbox.path, message).unwrap();
    assert_eq!(inbox.take(), None);
    let path = inbox.path.clone();
    drop(inbox);
    assert!(!path.exists());
}

/// Wait for a steer without sending any ACP output after the original prompt.
fn steer_a_quiet_turn(agent: Agent, parks_message: bool) {
    let dir = tempfile::tempdir().unwrap();
    let inbox = test_inbox(dir.path());
    let path = inbox.path.clone();
    let (from_agent, mut replies) = std::io::pipe().unwrap();
    let (commands, to_agent) = std::io::pipe().unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(commands).lines().map_while(Result::ok) {
            if sent.send(serde_json::from_str::<Value>(&line).unwrap()).is_err() {
                break;
            }
        }
    });
    let task = std::thread::spawn(move || {
        let mut events = Vec::new();
        let report = converse(BufReader::new(from_agent), to_agent, &request(agent, false), Some(inbox), &mut |event| events.push(event));
        (report, events)
    });
    let next = || received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(next()["method"], "initialize");
    writeln!(replies, "{}", reply(0, json!({ "protocolVersion": 1, "_meta": { "steering": { "supported": true } } }))).unwrap();
    assert_eq!(next()["method"], "session/new");
    writeln!(replies, "{}", opening()[1]).unwrap();
    assert_eq!(next()["method"], "session/prompt");
    std::fs::write(&path, json!({ "seq": 1, "text": "Check the tests too" }).to_string()).unwrap();
    let steer = next();
    assert_eq!(steer["method"], "_session/steering");
    assert_eq!(steer["params"]["sessionId"], "s1");
    assert_eq!(steer["params"]["prompt"], json!([{ "type": "text", "text": "Check the tests too" }]));
    let idle_behavior = &steer["params"]["_meta"]["steering"]["idleBehavior"];
    if agent == Agent::Codex {
        assert_eq!(idle_behavior, "promptRequired");
    } else {
        assert!(idle_behavior.is_null());
    }
    let steer_id = steer["id"].as_i64().unwrap();
    let finished = json!({ "stopReason": "end_turn" });
    let final_id = if parks_message {
        // The original prompt may finish before the adapter replies to the steer.
        writeln!(replies, "{}", chunk("m1", "Original answer.")).unwrap();
        writeln!(replies, "{}", reply(2, finished.clone())).unwrap();
        writeln!(replies, "{}", reply(steer_id, json!({ "outcome": "promptRequired", "reason": "noRunningTurn" }))).unwrap();
        let follow_up = next();
        assert_eq!(follow_up["method"], "session/prompt");
        assert_eq!(follow_up["params"]["sessionId"], "s1");
        assert_eq!(follow_up["params"]["prompt"], steer["params"]["prompt"]);
        follow_up["id"].as_i64().unwrap()
    } else {
        writeln!(replies, "{}", reply(steer_id, json!({ "outcome": "injected" }))).unwrap();
        2
    };
    writeln!(replies, "{}", chunk("m2", "Checked the tests too.")).unwrap();
    writeln!(replies, "{}", reply(final_id, finished)).unwrap();
    drop(replies);
    let (report, events) = task.join().unwrap();
    assert_eq!(report.as_deref(), Ok("Checked the tests too."));
    assert!(events.contains(&Event::Accepted { seq: 1 }));
    assert!(events.contains(&Event::Text { text: "Checked the tests too.".into() }));
    assert!(!path.exists());
}

/// The mod gives a message back to Claude Code's queue unless it is acknowledged, so one that never reached the
/// agent must not be: the agent exits before answering the steer, or refuses it with an error and then exits.
fn exit_before_taking_a_message(refuses_first: bool) {
    let dir = tempfile::tempdir().unwrap();
    let inbox = test_inbox(dir.path());
    let path = inbox.path.clone();
    let (from_agent, mut replies) = std::io::pipe().unwrap();
    let (commands, to_agent) = std::io::pipe().unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(commands).lines().map_while(Result::ok) {
            if sent.send(serde_json::from_str::<Value>(&line).unwrap()).is_err() {
                break;
            }
        }
    });
    let task = std::thread::spawn(move || {
        let mut events = Vec::new();
        let report = converse(BufReader::new(from_agent), to_agent, &request(Agent::Codex, false), Some(inbox), &mut |event| events.push(event));
        (report, events)
    });
    let next = || received.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(next()["method"], "initialize");
    writeln!(replies, "{}", reply(0, json!({ "protocolVersion": 1, "_meta": { "steering": { "supported": true } } }))).unwrap();
    assert_eq!(next()["method"], "session/new");
    writeln!(replies, "{}", opening()[1]).unwrap();
    assert_eq!(next()["method"], "session/prompt");
    std::fs::write(&path, json!({ "seq": 1, "text": "Check the tests too" }).to_string()).unwrap();
    let steer = next();
    assert_eq!(steer["method"], "_session/steering");
    if refuses_first {
        writeln!(replies, "{}", json!({ "jsonrpc": "2.0", "id": steer["id"], "error": { "code": -32603, "message": "busy" } })).unwrap();
    }
    drop(replies);
    let (report, events) = task.join().unwrap();
    assert!(report.is_err());
    assert!(!events.iter().any(|e| matches!(e, Event::Accepted { .. })), "{events:?}");
}

#[test]
fn a_message_is_not_acknowledged_when_the_agent_exits_before_taking_it() {
    exit_before_taking_a_message(false);
}

#[test]
fn a_message_is_not_acknowledged_when_the_agent_refuses_the_steer_and_exits() {
    exit_before_taking_a_message(true);
}

#[test]
fn codex_can_be_steered_while_acp_is_quiet() {
    steer_a_quiet_turn(Agent::Codex, false);
}

#[test]
fn codex_follows_up_in_the_same_session_when_steering_finds_an_idle_turn() {
    steer_a_quiet_turn(Agent::Codex, true);
}

#[test]
fn opencode_can_be_steered_when_it_advertises_support() {
    steer_a_quiet_turn(Agent::OpenCode, false);
}

#[test]
fn opencode_without_steering_leaves_delivery_to_claude() {
    let dir = tempfile::tempdir().unwrap();
    let mut says = opening();
    says.extend([chunk("m1", "Original answer."), reply(2, json!({ "stopReason": "end_turn" }))]);
    let script: String = says.iter().map(|m| format!("{m}\n")).collect();
    let mut events = Vec::new();
    let report = converse(std::io::Cursor::new(script), std::io::sink(), &request(Agent::OpenCode, false), Some(test_inbox(dir.path())), &mut |event| events.push(event));
    assert_eq!(report.as_deref(), Ok("Original answer."));
    assert!(!events.iter().any(|event| matches!(event, Event::Inbox { .. })));
}

#[test]
fn the_report_is_the_last_message_and_every_piece_streams() {
    let mut says = opening();
    says.extend([
        chunk("m1", "Looking at "),
        chunk("m1", "the parser."),
        update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Read src/parse.rs", "kind": "read", "status": "pending" })),
        chunk("m2", "The bug is on line 12."),
        reply(2, json!({ "stopReason": "end_turn" })),
    ]);
    let (result, sent, events) = talk(&says, false);
    assert_eq!(result, Ok("The bug is on line 12.".into()));
    assert_eq!(
        events,
        vec![
            Event::Progress { text: "Session ready. Sending the task.".into() },
            Event::Text { text: "Looking at ".into() },
            Event::Text { text: "the parser.".into() },
            Event::Step { text: "Read src/parse.rs".into() },
            Event::Text { text: "The bug is on line 12.".into() },
        ]
    );
    assert_eq!(sent.iter().map(|m| m["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "session/new", "session/prompt"]);
    assert_eq!(sent[1]["params"]["cwd"], "/work");
    assert_eq!(sent[2]["params"]["sessionId"], "s1");
    assert_eq!(sent[2]["params"]["prompt"][0]["text"], "Find the bug");
    assert_eq!(sent[0]["params"]["clientCapabilities"]["terminal"], false);
}

#[test]
fn thoughts_plans_and_tool_updates_stream_without_replacing_the_report() {
    let mut says = opening();
    says.extend([
        update(json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "Check the parser." } })),
        update(json!({ "sessionUpdate": "plan", "entries": [{ "content": "Run tests", "status": "in_progress" }] })),
        update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Run tests", "status": "in_progress" })),
        update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "_meta": { "terminal_output_delta": { "data": "running\n" } } })),
        update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "rawOutput": { "formatted_output": "started\n" } })),
        update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "failed", "content": [
            { "type": "content", "content": { "type": "text", "text": "started\nfailed\n" } },
        ] })),
        update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "failed", "rawOutput": "started\nfailed\n" })),
        chunk("m1", "The parser test failed."),
        update(json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "Thought after the report." } })),
        reply(2, json!({ "stopReason": "end_turn" })),
    ]);
    let (report, _, events) = talk(&says, false);
    assert_eq!(report, Ok("The parser test failed.".into()));
    assert_eq!(events, [
        Event::Progress { text: "Session ready. Sending the task.".into() },
        Event::Thought { text: "Check the parser.".into() },
        Event::Progress { text: "Plan\n- Run tests (in progress)".into() },
        Event::Step { text: "Run tests".into() },
        Event::Output { text: "running\n".into() },
        Event::Output { text: "started\n".into() },
        Event::Progress { text: "Run tests · failed".into() },
        Event::Output { text: "failed\n".into() },
        Event::Text { text: "The parser test failed.".into() },
        Event::Thought { text: "Thought after the report.".into() },
    ]);
}

/// OpenCode names only the tool when the call starts and sends what it acts on in the call's first update.
#[test]
fn a_step_named_only_by_its_tool_waits_for_what_it_acts_on() {
    let call = |id: &str, title: &str| update(json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": title, "status": "pending", "rawInput": {} }));
    let running = |id: &str, title: &str, input: Value| update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "title": title, "status": "in_progress", "rawInput": input }));
    let done = |id: &str| update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "title": null, "status": "completed", "rawInput": null }));
    let mut says = opening();
    says.extend([
        call("c1", "read"),
        call("c2", "shell"),
        call("c3", "todowrite"),
        call("c4", "grep"),
        running("c4", "grep", json!({ "pattern": "fn main", "path": "/work/src" })),
        running("c1", "read", json!({ "path": "note.json" })),
        running("c2", "echo hi", json!({ "command": "echo hi", "cwd": "/work" })),
        done("c1"),
        done("c3"),
        done("c2"),
        chunk("m1", "Done."),
        reply(2, json!({ "stopReason": "end_turn" })),
    ]);
    let steps: Vec<String> = talk_to(Agent::OpenCode, &says, false).2.into_iter().filter_map(|e| if let Event::Step { text } = e { Some(text) } else { None }).collect();
    assert_eq!(steps, ["grep fn main", "read note.json", "shell echo hi", "todowrite"]);
}

#[test]
fn messages_without_ids_split_at_tool_calls() {
    let mut says = opening();
    says.extend([
        update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "First." } })),
        update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Ran ls" })),
        update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Second." } })),
        reply(2, json!({ "stopReason": "end_turn" })),
    ]);
    assert_eq!(talk(&says, false).0, Ok("Second.".into()));
}

#[test]
fn a_child_session_message_is_not_the_report() {
    let mut says = opening();
    says.extend([
        chunk("m1", "Asked a helper."),
        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "child", "update": {
            "sessionUpdate": "agent_message_chunk", "messageId": "c1", "content": { "type": "text", "text": "Helper notes." } } } }),
        reply(2, json!({ "stopReason": "end_turn" })),
    ]);
    let (result, _, events) = talk_to(Agent::OpenCode, &says, true);
    assert_eq!(result, Ok("Asked a helper.".into()));
    assert_eq!(events, vec![Event::Progress { text: "Session ready. Sending the task.".into() }, Event::Text { text: "Asked a helper.".into() }]);
}

fn asks_permission() -> Vec<Value> {
    let mut says = opening();
    says.extend([
        json!({ "jsonrpc": "2.0", "id": 7, "method": "session/request_permission", "params": { "sessionId": "s1", "toolCall": { "toolCallId": "c1" }, "options": [
            { "optionId": "yes", "name": "Allow once", "kind": "allow_once" },
            { "optionId": "no", "name": "Reject", "kind": "reject_once" },
        ]}}),
        chunk("m1", "Done."),
        reply(2, json!({ "stopReason": "end_turn" })),
    ]);
    says
}

#[test]
fn a_turn_ended_by_a_refusal_says_what_was_refused() {
    let mut says = opening();
    says.extend([
        json!({ "jsonrpc": "2.0", "id": 0, "method": "session/request_permission", "params": { "sessionId": "s1",
            "toolCall": { "toolCallId": "c1", "title": "printf hi > made.txt" },
            "options": [{ "optionId": "allow_once", "kind": "allow_once" }, { "optionId": "cancel", "kind": "reject_once" }] } }),
        reply(2, json!({ "stopReason": "cancelled" })),
    ]);
    let (result, sent, _) = talk(&says, false);
    assert_eq!(
        result,
        Err("Codex stopped when read-only mode refused `printf hi > made.txt`. Turn on Allow edits in Settings → Custom subagents to let it.".into())
    );
    assert_eq!(sent.last().unwrap()["result"]["outcome"]["optionId"], "cancel");
}

#[test]
#[cfg(unix)]
fn a_program_only_the_login_shell_put_on_the_path_is_found() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("m4code-acp-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tool = dir.join("m4code-acp-path-probe");
    std::fs::write(&tool, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

    let _only_test_using_login_path = crate::platform::LOGIN_PATH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = crate::platform::LOGIN_PATH.lock().unwrap().clone();
    *crate::platform::LOGIN_PATH.lock().unwrap() = Some(dir.as_os_str().to_os_string());
    let found = locate_reloading("m4code-acp-path-probe");
    *crate::platform::LOGIN_PATH.lock().unwrap() = previous;
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(found, Some(tool));
}

#[test]
fn read_only_refuses_permission_requests() {
    let (_, sent, _) = talk(&asks_permission(), false);
    let answer = sent.iter().find(|m| m["id"] == 7).unwrap();
    assert_eq!(answer["result"]["outcome"], json!({ "outcome": "selected", "optionId": "no" }));
}

#[test]
fn with_edits_on_permission_requests_are_allowed() {
    let (_, sent, _) = talk(&asks_permission(), true);
    let answer = sent.iter().find(|m| m["id"] == 7).unwrap();
    assert_eq!(answer["result"]["outcome"], json!({ "outcome": "selected", "optionId": "yes" }));
}

#[test]
fn read_only_declines_rather_than_ending_codex_turn() {
    let options = json!([
        { "optionId": "allow_once", "kind": "allow_once" },
        { "optionId": "cancel", "kind": "reject_once" },
        { "optionId": "decline", "kind": "reject_once" },
    ]);
    assert_eq!(permission(&options, false), json!({ "outcome": "selected", "optionId": "decline" }));
}

#[test]
fn opencode_permission_options_are_answered_by_kind() {
    // What opencode 2.3 offers.
    let options = json!([
        { "optionId": "once", "kind": "allow_once", "name": "Allow once" },
        { "optionId": "always", "kind": "allow_always", "name": "Always allow" },
        { "optionId": "reject", "kind": "reject_once", "name": "Reject" },
    ]);
    assert_eq!(permission(&options, false), json!({ "outcome": "selected", "optionId": "reject" }));
    assert_eq!(permission(&options, true), json!({ "outcome": "selected", "optionId": "once" }));
}

#[test]
fn a_permission_request_with_no_refusing_option_is_cancelled() {
    let options = json!([{ "optionId": "yes", "kind": "allow_once" }]);
    assert_eq!(permission(&options, false), json!({ "outcome": "cancelled" }));
}

#[test]
fn file_and_terminal_requests_are_refused() {
    let mut says = opening();
    says.extend([
        json!({ "jsonrpc": "2.0", "id": 9, "method": "fs/write_text_file", "params": { "sessionId": "s1", "path": "/etc/x", "content": "" } }),
        chunk("m1", "Done."),
        reply(2, json!({ "stopReason": "end_turn" })),
    ]);
    let (result, sent, _) = talk(&says, true);
    assert_eq!(result, Ok("Done.".into()));
    assert_eq!(sent.iter().find(|m| m["id"] == 9).unwrap()["error"]["code"], -32601);
}

#[test]
fn signed_out_says_how_to_sign_in() {
    let says = vec![
        reply(0, json!({ "protocolVersion": 1 })),
        json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32000, "message": "Authentication required" } }),
    ];
    assert_eq!(talk(&says, false).0, Err("Codex isn't signed in. Run `codex login`, then try again.".into()));
    assert_eq!(talk_to(Agent::OpenCode, &says, false).0, Err("OpenCode isn't signed in. Run `opencode auth login`, then try again.".into()));
}

#[test]
fn an_agent_that_exits_midway_fails() {
    let mut says = opening();
    says.push(chunk("m1", "Starting"));
    assert_eq!(talk(&says, false).0, Err("codex-acp exited before Codex answered".into()));
    assert_eq!(talk_to(Agent::OpenCode, &says, false).0, Err("opencode exited before OpenCode answered".into()));
}

#[test]
fn a_turn_without_a_message_fails() {
    let mut says = opening();
    says.push(reply(2, json!({ "stopReason": "end_turn" })));
    assert_eq!(talk(&says, false).0, Err("Codex finished without writing a report".into()));
    assert_eq!(talk_to(Agent::OpenCode, &says, false).0, Err("OpenCode finished without writing a report".into()));
}

#[test]
fn running_out_of_tokens_still_reports_the_last_message() {
    let mut says = opening();
    says.extend([chunk("m1", "Partial findings."), reply(2, json!({ "stopReason": "max_tokens" }))]);
    assert_eq!(talk(&says, false).0, Ok("Partial findings.".into()));
}

#[test]
fn read_only_opencode_denies_all_but_reading_even_in_its_build_agent() {
    let read_only = json!({ "*": "deny", "read": "allow", "glob": "allow", "grep": "allow", "list": "allow" });
    assert_eq!(
        opencode_config(Some("openai/gpt-6.1-sol"), false),
        json!({ "model": "openai/gpt-6.1-sol", "default_agent": "build", "permission": read_only, "agent": { "build": { "permission": read_only } } })
    );
    assert_eq!(opencode_config(Some("openai/gpt-6.1-sol"), true), json!({ "model": "openai/gpt-6.1-sol" }));
    assert_eq!(opencode_config(None, true), json!({}));
}

#[test]
fn a_request_without_an_agent_is_for_codex() {
    let r: Request = serde_json::from_value(json!({ "model": "gpt-5.5", "edits": false, "cwd": "/w", "prompt": "x" })).unwrap();
    assert_eq!(r.agent, Agent::Codex);
    let r: Request = serde_json::from_value(json!({ "agent": "opencode", "model": "openai/gpt-5.5", "edits": false, "cwd": "/w", "prompt": "x" })).unwrap();
    assert_eq!(r.agent, Agent::OpenCode);
}

fn picker_session() -> Value {
    reply(1, json!({ "sessionId": "s1", "configOptions": [
        { "id": "mode", "category": "mode", "options": [{ "value": "read-only", "name": "Read-only" }] },
        { "id": "model", "category": "model", "options": [
            { "value": "gpt-6.1-sol", "name": "6.1 Sol" },
            { "value": "gpt-5.6-luna", "name": "5.6 Luna" },
        ]},
    ]}))
}

fn list(agent: Agent, says: &[Value]) -> (Result<Vec<AgentModel>, String>, Vec<Value>) {
    let script: String = says.iter().map(|m| format!("{m}\n")).collect();
    let mut sent = Vec::new();
    let models = list_models(agent, script.as_bytes(), &mut sent, Path::new("/work"));
    (models, String::from_utf8(sent).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect())
}

#[test]
fn the_models_come_from_the_session_model_picker() {
    let says = [reply(0, json!({ "protocolVersion": 1 })), update(json!({ "sessionUpdate": "available_commands_update", "availableCommands": [] })), picker_session()];
    let (models, sent) = list(Agent::Codex, &says);
    assert_eq!(
        models.unwrap(),
        vec![AgentModel { id: "gpt-6.1-sol".into(), name: "6.1 Sol".into() }, AgentModel { id: "gpt-5.6-luna".into(), name: "5.6 Luna".into() }]
    );
    assert_eq!(sent.iter().map(|m| m["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "session/new"], "no prompt, and Codex's session is left alone");
}

#[test]
fn antigravity_lists_its_model_config_option_without_prompting() {
    let says = [reply(0, json!({ "protocolVersion": 1, "agentInfo": { "name": "Antigravity ACP Server" } })), reply(1, json!({
        "sessionId": "s1",
        "configOptions": [
            { "id": "thought_level", "category": "thought_level", "options": [{ "value": "high", "name": "High" }] },
            { "id": "model", "category": "model", "options": [{ "value": "gemini-4-neon", "name": "Gemini 4 Neon" }] },
            { "id": "mode", "category": "mode", "options": [{ "value": "default", "name": "Default" }] }
        ],
        "_meta": { "antigravity": { "tier": { "name": "Google AI Pro" } } }
    }))];
    let (models, sent) = list(Agent::Antigravity, &says);
    assert_eq!(models.unwrap(), vec![AgentModel { id: "gemini-4-neon".into(), name: "Gemini 4 Neon".into() }]);
    assert_eq!(sent.iter().map(|m| m["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "session/new"]);
}

#[test]
fn opencode_deletes_the_session_it_read_the_models_from() {
    let says = [reply(0, json!({ "protocolVersion": 1 })), picker_session(), reply(2, json!({}))];
    let (models, sent) = list(Agent::OpenCode, &says);
    assert_eq!(models.unwrap().len(), 2);
    assert_eq!(sent.iter().map(|m| m["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "session/new", "session/delete"]);
    assert_eq!(sent[2]["params"]["sessionId"], "s1");
    // A delete opencode refuses, or never answers before exiting, still gives the models.
    let says = [reply(0, json!({ "protocolVersion": 1 })), picker_session()];
    assert_eq!(list(Agent::OpenCode, &says).0.unwrap().len(), 2);
}

#[test]
fn claude_code_lists_its_models_without_persisting_a_session() {
    let says = [
        reply(0, json!({ "protocolVersion": 1 })),
        reply(1, json!({
            "sessionId": "s1",
            "configOptions": [
                { "id": "model", "category": "model", "options": [
                    { "value": "default", "name": "Default (Opus)" },
                    { "value": "sonnet", "name": "Sonnet 4.6" },
                ]}
            ]
        })),
    ];
    let script: String = says.iter().map(|m| format!("{m}\n")).collect();
    let mut sent = Vec::new();
    let models = list_models_for(crate::harness::Harness::Claude, script.as_bytes(), &mut sent, Path::new("/work")).unwrap();
    let sent: Vec<Value> = String::from_utf8(sent).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(models, vec![AgentModel { id: "sonnet".into(), name: "Sonnet 4.6".into() }]);
    assert_eq!(sent.iter().map(|m| m["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "session/new"]);
    assert_eq!(sent[1]["params"]["_meta"]["claudeCode"]["options"]["persistSession"], json!(false));
}

#[test]
fn no_model_picker_is_an_error() {
    let says = [reply(0, json!({})), reply(1, json!({ "sessionId": "s1" }))];
    assert_eq!(list(Agent::Codex, &says).0, Err("codex-acp listed no models".into()));
    assert_eq!(list(Agent::OpenCode, &says).0, Err("opencode listed no models".into()));
    let script: String = says.iter().map(|m| format!("{m}\n")).collect();
    let mut sent = Vec::new();
    assert_eq!(
        list_models_for(crate::harness::Harness::Claude, script.as_bytes(), &mut sent, Path::new("/work")),
        Err("claude-bridge listed no models".into())
    );
}

#[test]
fn events_are_json_lines_the_mod_reads() {
    assert_eq!(serde_json::to_value(Event::Step { text: "Ran ls".into() }).unwrap(), json!({ "kind": "step", "text": "Ran ls" }));
    assert_eq!(serde_json::to_value(Event::Failed { reason: "x".into() }).unwrap(), json!({ "kind": "failed", "reason": "x" }));
}
