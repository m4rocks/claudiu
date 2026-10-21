mod common;

use common::*;
use serde_json::{Value, json};

fn post_hook(h: &Harness, id: &claudiu_core::SessionId, tool: &str, input: Value, response: Value) -> Value {
    post(&h.core.hook_url(id, "post"), &json!({"hook_event_name": "PostToolUse", "tool_name": tool, "tool_input": input, "tool_response": response})).1
}

fn post_as(h: &Harness, id: &claudiu_core::SessionId, agent: &str, tool: &str, input: Value, response: Value) -> Value {
    post(&h.core.hook_url(id, "post"), &json!({"hook_event_name": "PostToolUse", "agent_id": agent, "tool_name": tool, "tool_input": input, "tool_response": response})).1
}

fn updated(reply: &Value) -> &Value {
    &reply["hookSpecificOutput"]["updatedToolOutput"]
}

fn lines(n: usize) -> String {
    (1..=n).map(|i| format!("line {i} of the output, with some words to make it a realistic length")).collect::<Vec<_>>().join("\n")
}

fn read_response(path: &str, content: &str) -> Value {
    json!({"file": {"content": content, "filePath": path, "numLines": content.lines().count(), "startLine": 1, "totalLines": content.lines().count()}, "type": "text"})
}

fn handle_in(text: &str) -> String {
    let at = text.find("handle ").expect("the footer names a handle") + "handle ".len();
    text[at..].chars().take_while(|c| c.is_ascii_hexdigit()).collect()
}

#[test]
fn small_output_is_left_alone() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let reply = post_hook(&h, &id, "Bash", json!({"command": "echo hi"}), json!({"stdout": "hi", "stderr": "", "interrupted": false}));
    assert_eq!(reply, json!({}));
}

#[test]
fn big_shell_output_is_compacted_and_every_line_stays_retrievable() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let full = lines(3000);

    let reply = post_hook(&h, &id, "Bash", json!({"command": "cargo build"}), json!({"stdout": full, "stderr": "", "interrupted": false, "isImage": false}));

    let out = updated(&reply);
    let shown = out["stdout"].as_str().unwrap();
    assert!(shown.len() < full.len() / 10, "{} of {} bytes", shown.len(), full.len());
    assert!(shown.starts_with("line 1 of the output"), "the start is kept");
    assert!(shown.contains("line 3000 of the output"), "so is the end");
    assert_eq!(out["interrupted"], false, "the rest of the response is untouched");
    let footer = shown.lines().find(|l| l.contains("handle ")).unwrap();
    assert!(footer.len() < 240, "the footer is cheap: {footer}");
    assert!(footer.contains("expand"), "and says how to get the rest");

    let handle = handle_in(shown);
    assert_eq!(handle.len(), 12, "a 12-hex handle");
    assert_eq!(call_tool(&h.core, &id, "expand", json!({"handle": handle})), full, "the whole text comes back");
    let range = call_tool(&h.core, &id, "expand", json!({"handle": handle, "from": 5, "to": 6}));
    assert_eq!(range.lines().collect::<Vec<_>>(), full.lines().skip(4).take(2).collect::<Vec<_>>());
    assert!(call_tool(&h.core, &id, "expand", json!({"handle": "000000000000"})).to_lowercase().contains("no such handle"));
}

#[test]
fn handles_follow_the_content() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let again = h.core.start_session(&h.project()).unwrap();
    let full = lines(500);
    let response = || json!({"stdout": full.clone(), "stderr": ""});
    let first = post_hook(&h, &id, "Bash", json!({}), response());
    let second = post_hook(&h, &again, "Bash", json!({}), response());
    assert_eq!(handle_in(updated(&first)["stdout"].as_str().unwrap()), handle_in(updated(&second)["stdout"].as_str().unwrap()));
}

#[test]
fn read_grep_and_glob_keep_their_shape() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();

    let read = post_hook(&h, &id, "Read", json!({"file_path": "big.rs"}), read_response("big.rs", &lines(2000)));
    let file = &updated(&read)["file"];
    assert_eq!(file["filePath"], "big.rs");
    assert!(file["content"].as_str().unwrap().len() < 20_000 && file["content"].as_str().unwrap().contains("handle "));
    assert_eq!(updated(&read)["type"], "text");

    let grep = post_hook(&h, &id, "Grep", json!({"pattern": "x"}), json!({"content": lines(2000), "filenames": [], "mode": "content", "numFiles": 0, "numLines": 2000}));
    assert!(updated(&grep)["content"].as_str().unwrap().contains("handle "));
    assert_eq!(updated(&grep)["mode"], "content");

    let names: Vec<String> = (0..1000).map(|i| format!("src/module_{i}/file.rs")).collect();
    let glob = post_hook(&h, &id, "Glob", json!({"pattern": "**/*.rs"}), json!({"filenames": names, "numFiles": 1000, "truncated": false}));
    let shown = updated(&glob)["filenames"].as_array().unwrap();
    assert!(shown.len() < 200 && shown.len() > 50, "{} names kept", shown.len());
    assert!(shown.last().unwrap().as_str().unwrap().contains("handle "));
    assert_eq!(updated(&glob)["numFiles"], 1000);
}

#[test]
fn a_file_the_model_already_has_is_not_sent_again_until_it_changes() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let text = lines(40);
    let read = |agent: &str, body: &str| post_as(&h, &id, agent, "Read", json!({"file_path": "a.rs"}), read_response("a.rs", body));

    assert_eq!(read("main", &text), json!({}), "the first read passes untouched");
    let repeat = read("main", &text);
    let pointer = updated(&repeat)["file"]["content"].as_str().unwrap().to_string();
    assert!(pointer.contains("unchanged") && pointer.contains("handle ") && pointer.len() < 300, "{pointer}");
    assert_eq!(call_tool(&h.core, &id, "expand", json!({"handle": handle_in(&pointer)})), text, "and the text is one call away");

    assert_eq!(read("main", &format!("{text}\nnew line")), json!({}), "a changed file is sent in full");
    assert_eq!(read("sub-1", &text), json!({}), "each agent has its own ledger");
}

#[test]
fn compaction_of_the_conversation_resets_the_ledger() {
    let compact = json!({"update": {"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Compact conversation", "status": "in_progress", "_meta": {"claudeCode": {"toolName": "compact"}}}});
    let h = Harness::new(json!({"turns": [{"steps": [compact]}]}));
    let rx = h.core.subscribe();
    let id = h.core.start_session(&h.project()).unwrap();
    let text = lines(40);
    let read = || post_hook(&h, &id, "Read", json!({"file_path": "a.rs"}), read_response("a.rs", &text));
    assert_eq!(read(), json!({}));
    assert_ne!(read(), json!({}), "remembered");

    h.core.send_prompt(&id, "/compact").unwrap();
    until(&rx, "turn_ended");

    assert_eq!(read(), json!({}), "the model no longer has the file, so it is sent in full again");
}

#[test]
fn the_firewall_fails_open_and_never_blocks() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let (status, reply) = post(&h.core.hook_url(&id, "post"), &json!({"nonsense": true}));
    assert_eq!((status, reply), (200, json!({})));
    let (status, _) = post(&h.core.hook_url(&id, "post"), &json!("not an object"));
    assert_eq!(status, 200);

    let post_hook = &h.adapter_calls("session/new")[0]["_meta"]["claudeCode"]["options"]["settings"]["hooks"]["PostToolUse"][0];
    assert_eq!(post_hook["matcher"], "Read|Grep|Glob|Bash");
    assert!(post_hook["hooks"][0].get("onFailure").is_none(), "a firewall that fails must let the output through");
}
