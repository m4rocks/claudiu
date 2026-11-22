//! Drives the real workspace on gpui's test platform (no window opens) with a throwaway data folder, to catch
//! hangs and panics when sessions start and switch and a shell panel opens. Spawns the real `claude` and shell.
//! `cargo test -- --ignored headless`.
use super::*;
use gpui::{TestAppContext, VisualTestContext};

/// A stand-in ACP agent for tests that need a session up but no real harness: it opens any session it is asked for,
/// names a new one `fake-session`, and ends every prompt with the reply "ok". Needs python3 on the PATH.
fn fake_agent(dir: &std::path::Path) -> (PathBuf, Vec<String>) {
    let script = dir.join("fake_agent.py");
    std::fs::write(&script, r#"import json, sys
def send(m):
    sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m or "method" not in m:
        continue
    method, i = m["method"], m["id"]
    if method == "session/new":
        cwd = m["params"]["cwd"]
        send({"jsonrpc": "2.0", "id": i, "result": {"sessionId": "fake-session"}})
    elif method == "session/prompt":
        s = m["params"]["sessionId"]
        text = m["params"]["prompt"][0]["text"]
        # Every message is logged next to the script; one that says "edit" changes notes.txt, as an agent's turn would.
        with open(__file__ + ".prompts", "a") as log:
            log.write(text + "\n---\n")
        if "edit" in text:
            with open(cwd + "/notes.txt", "w") as f:
                f.write("one\nTWO\nthree\n")
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": s, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "ok"}}}})
        send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "end_turn"}})
    elif method == "session/list":
        send({"jsonrpc": "2.0", "id": i, "result": {"sessions": [{"sessionId": "from-terminal", "cwd": m["params"]["cwd"], "title": "Started in a terminal"}]}})
    elif method == "session/load":
        cwd = m["params"]["cwd"]
        with open(__file__ + ".prompts", "a") as log:
            log.write("loaded " + m["params"]["sessionId"] + "\n---\n")
        send({"jsonrpc": "2.0", "id": i, "result": {}})
    else:
        send({"jsonrpc": "2.0", "id": i, "result": {}})
"#).unwrap();
    let python = crate::platform::find_executable("python3").expect("python3 for the stand-in agent");
    (python, vec![script.to_string_lossy().into_owned()])
}


mod sessions;
mod editor;
mod language_servers;
mod scrolling;
mod worktrees;
mod agents;
mod pickers;
mod skills;
