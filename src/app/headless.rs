//! Drives the real workspace on gpui's test platform (no window opens) with a throwaway data folder, to catch
//! hangs and panics when sessions start and switch and a shell panel opens. Spawns the real `claude` and shell.
//! `cargo test -- --ignored headless`.
use super::*;
use gpui::{TestAppContext, VisualTestContext};

/// A stand-in ACP agent for tests that need a session up but no real harness: it opens any session it is asked for,
/// names a new one `fake-session`, and ends every prompt with the reply "ok". Needs python3 on the PATH.
fn fake_agent(dir: &std::path::Path) -> (PathBuf, Vec<String>) {
    let script = dir.join("fake_agent.py");
    // Its files are written with `newline="\n"`: Python on Windows would otherwise write CRLF, and the tests split
    // and compare them by "\n".
    std::fs::write(&script, r#"import json, sys, os
def send(m):
    sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m or "method" not in m:
        continue
    method, i = m["method"], m["id"]
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": i, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True, "sessionCapabilities": {"resume": {}}}}})
    elif method == "session/new":
        cwd = m["params"]["cwd"]
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "fake-session", "update": {"sessionUpdate": "available_commands_update", "availableCommands": [{"name": "compact", "description": "Compact context"}]}}})
        send({"jsonrpc": "2.0", "id": i, "result": {"sessionId": "fake-session"}})
    elif method == "session/prompt":
        s = m["params"]["sessionId"]
        text = m["params"]["prompt"][0]["text"]
        with open(__file__ + ".turns", "a", newline="\n") as log:
            log.write(json.dumps({"prompt": text, "pid": os.getpid()}) + "\n")
        # Every message is logged next to the script; one that says "edit" changes notes.txt, as an agent's turn would.
        with open(__file__ + ".prompts", "a", newline="\n") as log:
            log.write(text + "\n---\n")
        if "edit" in text:
            with open(cwd + "/notes.txt", "w", newline="\n") as f:
                f.write("one\nTWO\nthree\n")
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": s, "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "ok"}}}})
        if text in ["fill context", "/compact"]:
            send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": s, "update": {"sessionUpdate": "usage_update", "used": 950 if text == "fill context" else 200, "size": 1000}}})
        send({"jsonrpc": "2.0", "id": i, "result": {"stopReason": "end_turn"}})
    elif method == "session/list":
        send({"jsonrpc": "2.0", "id": i, "result": {"sessions": [{"sessionId": "from-terminal", "cwd": m["params"]["cwd"], "title": "Started in a terminal"}]}})
    elif method == "session/load":
        cwd = m["params"]["cwd"]
        with open(__file__ + ".prompts", "a", newline="\n") as log:
            log.write("loaded " + m["params"]["sessionId"] + "\n---\n")
        send({"jsonrpc": "2.0", "id": i, "result": {}})
    else:
        send({"jsonrpc": "2.0", "id": i, "result": {}})
"#.replace("\r\n", "\n")).unwrap();
    let candidates: &[&str] = if cfg!(windows) { &["python", "python3"] } else { &["python3", "python"] };
    let python = candidates
        .iter()
        .find_map(|name| crate::platform::find_executable(name))
        .expect("python3 for the stand-in agent");
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

/// Start an offline session for visual scenarios through the same ACP path as the headless tests.
#[cfg(feature = "screenshots")]
impl Workspace {
    pub(crate) fn start_screenshot_session(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.acp_server = Some(fake_agent(&cwd));
        self.claude_exe = Some("offline-screenshot-agent".into());
        self.new_session_from_home(Harness::Claude, None, cwd, None, None, None, window, cx);
        if let Some(id) = self.active_id().cloned() {
            if let Some(record) = self.store.session_mut(&id) {
                record.title = "Workspace review".into();
                let mut recent = record.clone();
                recent.id = new_id();
                recent.title = "Composer shortcut review".into();
                recent.last_active -= 3600;
                recent.exit_code = Some(0);
                self.store.data.sessions.push(recent);
            }
            self.store.data.layout.recent_open = true;
        }
    }
}
