//! Talking ACP (Agent Client Protocol: newline-delimited JSON-RPC over stdio) to Zed's Claude adapter.
//!
//! The adapter is a small Node program that drives the Claude Agent SDK. Claudiu points it at the user's own
//! `claude` executable (`CLAUDE_CODE_EXECUTABLE`), so the login, settings, skills and commands are Claude Code's,
//! exactly as in a terminal session. It is installed once into Claudiu's data dir with npm; nothing is bundled.
//! One adapter process serves every chat (each chat is a session in it) and is started ahead of time, so a chat opens
//! without waiting for Node. Closing its stdin makes it exit and take its `claude` children along.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::{Value, json};

/// The adapter release Claudiu was written against. A different installed version is replaced.
pub const ADAPTER_VERSION: &str = "0.94.0";
const PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";

fn dir() -> PathBuf {
    crate::datadir::data_dir().join("acp")
}

fn package_dir() -> PathBuf {
    dir().join("node_modules").join(PACKAGE)
}

pub fn script() -> PathBuf {
    package_dir().join("dist").join("index.js")
}

pub fn installed() -> bool {
    std::fs::read_to_string(package_dir().join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| v["version"] == ADAPTER_VERSION)
        && script().is_file()
}

/// A tool of the user's Node install, run through their login shell on macOS (a GUI app has a minimal PATH there).
fn node_tool(name: &str, args: Vec<String>) -> Option<Command> {
    #[cfg(target_os = "macos")]
    let (program, args) = crate::platform::through_login_shell(name.to_string(), args);
    #[cfg(not(target_os = "macos"))]
    let program = crate::platform::find_executable(name)?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    crate::platform::no_window(&mut cmd);
    Some(cmd)
}

pub fn has_node() -> bool {
    node_tool("node", vec!["--version".into()]).is_some_and(|mut c| c.stdin(Stdio::null()).output().is_ok_and(|o| o.status.success()))
}

/// `npm ci` of the pinned adapter from the lockfile shipped with Claudiu, so every dependency is the exact version
/// that was reviewed, and no package may run an install script. The SDK's bundled `claude` binary (300 MB) is
/// optional and skipped: the user's own executable is always used.
pub fn install() -> Result<(), String> {
    // Two chats opened together on first use must not run two installs into one folder.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    if installed() {
        return Ok(());
    }
    let root = dir();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    for (name, text) in [("package.json", include_str!("adapter/package.json")), ("package-lock.json", include_str!("adapter/package-lock.json"))] {
        std::fs::write(root.join(name), text).map_err(|e| e.to_string())?;
    }
    let args = ["ci", "--prefix"].map(String::from).into_iter().chain([
        root.to_string_lossy().into_owned(),
        "--ignore-scripts".into(),
        "--omit=optional".into(),
        "--no-audit".into(),
        "--no-fund".into(),
        "--loglevel=error".into(),
    ]);
    let mut cmd = node_tool("npm", args.collect()).ok_or("npm was not found")?;
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| format!("could not run npm: {e}"))?;
    if out.status.success() && installed() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(format!("npm install failed: {}", err.trim().lines().last().unwrap_or("no output")))
}

/// What the adapter sent.
#[derive(Debug)]
pub enum Msg {
    Notification { method: String, params: Value },
    /// The adapter asks us something and waits (a permission prompt); answer with [`Agent::respond`].
    Request { id: Value, method: String, params: Value },
    Reply { id: u64, result: Result<Value, String> },
    /// The process ended (or its output closed); the text is the tail of its stderr.
    Exit(String),
}

fn classify(line: &str) -> Option<Msg> {
    let v: Value = serde_json::from_str(line).ok()?;
    let id = v.get("id").cloned();
    let method = v.get("method").and_then(Value::as_str).map(String::from);
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    Some(match (id, method) {
        (Some(id), Some(method)) => Msg::Request { id, method, params },
        (None, Some(method)) => Msg::Notification { method, params },
        (Some(id), None) => Msg::Reply {
            id: id.as_u64()?,
            result: match v.get("error") {
                Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("request failed").to_string()),
                None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            },
        },
        (None, None) => return None,
    })
}

/// Where the handshake with the adapter stands. It is done once per process, for all chats.
enum Init {
    /// `initialize` was sent (request `id`); these chat requests are answered when it is.
    Pending { id: u64, waiters: Vec<(u64, String)> },
    Done,
}

/// Who gets what: the one adapter process serves every chat, so its output is sorted into the chats' inboxes.
struct Routes {
    /// Chat id -> (which attach it is, inbox).
    chats: HashMap<String, (u64, UnboundedSender<Msg>)>,
    /// Request id -> chat id, and whether its answer names a new session.
    calls: HashMap<u64, (String, bool)>,
    /// Claude's session id -> chat id.
    sessions: HashMap<String, String>,
    /// Session messages that arrived before their session was known (a new session announces itself in its answer).
    orphans: Vec<(String, Msg)>,
    init: Init,
    exited: bool,
}

impl Routes {
    fn send(&self, chat: &str, msg: Msg) {
        if let Some((_, tx)) = self.chats.get(chat) {
            let _ = tx.unbounded_send(msg);
        }
    }

    fn know(&mut self, session: &str, chat: &str) {
        self.sessions.insert(session.to_string(), chat.to_string());
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.orphans).into_iter().partition(|(s, _)| s == session);
        self.orphans = rest;
        for (_, msg) in mine {
            self.send(chat, msg);
        }
    }

    fn route(&mut self, msg: Msg) {
        match msg {
            Msg::Reply { id, result } => {
                if matches!(&self.init, Init::Pending { id: init, .. } if *init == id) {
                    let Init::Pending { waiters, .. } = std::mem::replace(&mut self.init, Init::Done) else { return };
                    self.exited |= result.is_err();
                    for (id, chat) in waiters {
                        self.send(&chat, Msg::Reply { id, result: result.clone() });
                    }
                    return;
                }
                let Some((chat, creates)) = self.calls.remove(&id) else { return };
                if creates && let Ok(r) = &result && let Some(sid) = r.get("sessionId").and_then(Value::as_str) {
                    self.know(sid, &chat);
                }
                self.send(&chat, Msg::Reply { id, result });
            }
            Msg::Notification { ref params, .. } | Msg::Request { ref params, .. } => {
                let Some(sid) = params.get("sessionId").and_then(Value::as_str).map(String::from) else { return };
                match self.sessions.get(&sid).cloned() {
                    Some(chat) => self.send(&chat, msg),
                    None => {
                        self.orphans.push((sid, msg));
                        if self.orphans.len() > 100 {
                            self.orphans.remove(0);
                        }
                    }
                }
            }
            Msg::Exit(tail) => {
                self.exited = true;
                for (_, tx) in self.chats.values() {
                    let _ = tx.unbounded_send(Msg::Exit(tail.clone()));
                }
            }
        }
    }
}

/// The adapter process, shared by all chats that run the same `claude` under the same login.
struct Shared {
    key: String,
    /// Lines for the writer thread; dropping it closes the adapter's stdin.
    tx: Mutex<Option<Sender<String>>>,
    child: Mutex<Option<Child>>,
    next: AtomicU64,
    routes: Mutex<Routes>,
}

/// The process everyone uses. Replaced when the `claude` or the account changes; the old one ends once its last chat has left it.
static CURRENT: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

impl Shared {
    fn spawn(claude: &Path, env: Vec<(String, String)>, key: String) -> Result<Arc<Self>, String> {
        let mut cmd = node_tool("node", vec![script().to_string_lossy().into_owned()]).ok_or("Node.js was not found")?;
        cmd.current_dir(dir())
            // shortcut: `claude` is passed as is; a `.cmd` shim (npm-global install on Windows) is probably not
            // runnable by the SDK. Resolve it to the real binary if reports come in.
            .env("CLAUDE_CODE_EXECUTABLE", claude)
            .env("CLAUDE_CODE_AUTO_CONNECT_IDE", "false")
            .env("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL", "1")
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("could not start the Claude adapter: {e}"))?;
        let (mut stdin, stdout, stderr) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());

        let (tx, lines) = channel::<String>();
        std::thread::spawn(move || {
            for line in lines {
                if writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });

        // The adapter logs progress to stderr; only the tail matters, for the message when it dies.
        let tail = Arc::new(Mutex::new(std::collections::VecDeque::<String>::new()));
        let tail_w = tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut t = tail_w.lock().unwrap();
                if t.len() == 6 {
                    t.pop_front();
                }
                t.push_back(line);
            }
        });

        let init = 1;
        let shared = Arc::new(Self {
            key,
            tx: Mutex::new(Some(tx)),
            child: Mutex::new(Some(child)),
            next: AtomicU64::new(init),
            routes: Mutex::new(Routes { chats: HashMap::new(), calls: HashMap::new(), sessions: HashMap::new(), orphans: vec![], init: Init::Pending { id: init, waiters: vec![] }, exited: false }),
        });
        shared.send(json!({"jsonrpc": "2.0", "id": init, "method": "initialize", "params": {"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}}}}));

        // Weak: the reader must not keep the process alive once nobody uses it.
        let weak = Arc::downgrade(&shared);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Some(shared) = weak.upgrade() else { return };
                if let Some(msg) = classify(&line) {
                    shared.routes.lock().unwrap_or_else(|e| e.into_inner()).route(msg);
                }
            }
            std::thread::sleep(Duration::from_millis(100)); // let the stderr reader catch up
            if let Some(shared) = weak.upgrade() {
                let tail = tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
                shared.routes.lock().unwrap_or_else(|e| e.into_inner()).route(Msg::Exit(tail));
            }
        });
        Ok(shared)
    }

    fn send(&self, v: Value) {
        if let Some(tx) = self.tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(v.to_string());
        }
    }

    /// The running process for this `claude` and login, started if there is none (or it died). Blocking.
    fn ensure(claude: &Path, env: Vec<(String, String)>) -> Result<Arc<Self>, String> {
        let key = format!("{}|{env:?}", claude.display());
        let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = current.as_ref().filter(|s| s.key == key && !s.routes.lock().unwrap_or_else(|e| e.into_inner()).exited) {
            return Ok(s.clone());
        }
        let s = Self::spawn(claude, env, key)?;
        *current = Some(s.clone());
        Ok(s)
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        *self.tx.lock().unwrap_or_else(|e| e.into_inner()) = None; // closes stdin: the adapter exits and ends its `claude`s
        let Some(mut child) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        // shortcut: a stuck adapter is killed after 3 s, but on Windows only the node process, not a `claude`
        // it started; harden with a job object if orphans ever show up.
        std::thread::spawn(move || {
            use wait_timeout::ChildExt;
            if !matches!(child.wait_timeout(Duration::from_secs(3)), Ok(Some(_))) {
                let _ = child.kill();
                let _ = child.wait();
            }
        });
    }
}

/// Start the shared adapter ahead of time, so the first chat only has to open its conversation. Blocking.
pub fn warm(claude: &Path, env: Vec<(String, String)>) {
    if has_node() && installed() {
        let _ = Shared::ensure(claude, env);
    }
}

/// One chat's line to the adapter. Dropping it leaves the adapter (and the other chats) running.
pub struct Agent {
    shared: Arc<Shared>,
    chat: String,
    /// Tells this attach from a later one of the same chat (a restart), so a late drop does not remove the new inbox.
    token: u64,
}

impl Agent {
    /// Blocking: joins the shared adapter, starting it if needed. Call it off the UI thread.
    pub fn attach(claude: &Path, env: Vec<(String, String)>, chat: &str) -> Result<(Self, UnboundedReceiver<Msg>), String> {
        let shared = Shared::ensure(claude, env)?;
        let (tx, rx) = unbounded();
        let token = shared.next.fetch_add(1, Ordering::SeqCst) + 1;
        shared.routes.lock().unwrap_or_else(|e| e.into_inner()).chats.insert(chat.to_string(), (token, tx));
        Ok((Self { shared, chat: chat.to_string(), token }, rx))
    }

    /// Send a request; the answer arrives as `Msg::Reply` with the returned id.
    pub fn call(&mut self, method: &str, params: Value) -> u64 {
        let id = self.shared.next.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut r = self.shared.routes.lock().unwrap_or_else(|e| e.into_inner());
            if method == "initialize" {
                // Done once for the process; a chat that comes later is told it is.
                match &mut r.init {
                    Init::Done => r.send(&self.chat, Msg::Reply { id, result: Ok(Value::Null) }),
                    Init::Pending { waiters, .. } => waiters.push((id, self.chat.clone())),
                }
                return id;
            }
            if method == "session/load"
                && let Some(sid) = params.get("sessionId").and_then(Value::as_str)
            {
                // The history is replayed before the answer comes, so the session is known up front.
                r.know(sid, &self.chat);
            }
            r.calls.insert(id, (self.chat.clone(), method == "session/new"));
        }
        self.shared.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.shared.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn respond(&self, id: Value, result: Result<Value, &str>) {
        self.shared.send(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(m) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": m}}),
        });
    }

    /// The chat is gone: end its conversation in the adapter, which also ends the `claude` behind it.
    pub fn close(&self, session: &str) {
        self.shared.routes.lock().unwrap_or_else(|e| e.into_inner()).sessions.remove(session);
        let id = self.shared.next.fetch_add(1, Ordering::SeqCst) + 1;
        self.shared.send(json!({"jsonrpc": "2.0", "id": id, "method": "session/close", "params": {"sessionId": session}}));
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let mut r = self.shared.routes.lock().unwrap_or_else(|e| e.into_inner());
        if r.chats.get(&self.chat).is_some_and(|(t, _)| *t == self.token) {
            r.chats.remove(&self.chat);
            r.calls.retain(|_, (c, _)| *c != self.chat);
        }
    }
}

/// Tools a chat never gets (product owner decision): it can read and search the project and talk about it, but
/// changes happen in a Code session. Denied tools are taken out of the model's context, whatever the permission mode.
pub const READ_ONLY_DENY: [&str; 8] = ["Bash", "PowerShell", "Monitor", "Edit", "Write", "NotebookEdit", "EnterWorktree", "ExitWorktree"];

/// `_meta.claudeCode.options` for `session/new` and `session/load`: the Claudiu tools are pre-allowed (Claudiu asks the
/// user itself before it acts on one) and the file and shell tools are denied.
pub fn session_options(chat_id: &str) -> serde_json::Value {
    let mut o = serde_json::json!({ "allowedTools": crate::mcp::allowed_tools("chat"), "disallowedTools": READ_ONLY_DENY });
    // The same per-process mod the terminal sessions get: it reports auto-compact and the advisor, and applies changes
    // made in the sidebar. It runs inside this chat's own `claude`, so nothing else is touched.
    if let Ok(dir) = crate::mcp::prepare_mod() {
        o["plugins"] = serde_json::json!([{ "type": "local", "path": dir }]);
        o["env"] = serde_json::json!({
            crate::usage::USAGE_DIR_ENV: crate::usage::snapshot_dir(),
            crate::usage::CTL_ENV: crate::usage::ctl_file(chat_id),
        });
    }
    o
}

/// What a chat is told about being read-only and how to hand work to a Code session. `tied` is whether it has a project.
pub fn handoff_prompt(chat_id: &str, tied: bool) -> String {
    let folder = if tied {
        "the folder defaults to this chat's project"
    } else {
        "this chat has no project, so ask the user which project or folder, and pass it as the folder argument"
    };
    format!(
        "# This chat is read-only\n\nYou can read and search the project's files, but you cannot edit files or run commands: those tools are switched off here. Coding happens in a Claude Code session. When the user wants to start coding (or sends \"Start coding\"), call the start_code_session tool with a prompt that is a self-contained implementation brief of **under 2,000 characters**: the goal, the decisions made in this chat, the files and places involved, constraints, and how to check the result ({folder}). End the brief with: \"Background: the chat {chat_id} (read_chat) has the full conversation.\" Claudiu shows the brief to the user, who confirms before anything starts. This chat's id is {chat_id}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_cannot_edit_or_run_commands_and_its_pre_allowed_tools_do_not_undo_that() {
        let o = session_options("c1");
        let denied: Vec<_> = o["disallowedTools"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
        for tool in ["Bash", "Monitor", "Edit", "Write", "NotebookEdit"] {
            assert!(denied.contains(&tool), "{tool} must be denied");
        }
        // Pre-allowed tools are Claudiu's own MCP tools only.
        assert!(o["allowedTools"].as_array().unwrap().iter().all(|t| t.as_str().unwrap().starts_with("mcp__claudiu__")));
    }

    #[test]
    fn the_handoff_prompt_names_the_chat_and_the_cap() {
        let p = handoff_prompt("c-1", false);
        assert!(p.contains("c-1") && p.contains("2,000") && p.contains("ask the user which project"));
        assert!(handoff_prompt("c-1", true).contains("defaults to this chat's project"));
    }

    #[test]
    fn the_shipped_lockfile_pins_the_adapter_version_in_this_file() {
        let pkg: Value = serde_json::from_str(include_str!("adapter/package.json")).unwrap();
        let lock: Value = serde_json::from_str(include_str!("adapter/package-lock.json")).unwrap();
        assert_eq!(pkg["dependencies"][PACKAGE], ADAPTER_VERSION);
        assert_eq!(lock["packages"][format!("node_modules/{PACKAGE}")]["version"], ADAPTER_VERSION);
        assert!(lock["packages"].as_object().unwrap().values().all(|p| p.get("hasInstallScript").is_none()), "a dependency with an install script");
    }

    #[test]
    fn lines_are_told_apart() {
        assert!(matches!(classify(r#"{"jsonrpc":"2.0","method":"session/update","params":{"a":1}}"#), Some(Msg::Notification { .. })));
        assert!(matches!(classify(r#"{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{}}"#), Some(Msg::Request { .. })));
        assert!(matches!(classify(r#"{"jsonrpc":"2.0","id":3,"result":{"x":1}}"#), Some(Msg::Reply { id: 3, result: Ok(_) })));
        match classify(r#"{"jsonrpc":"2.0","id":4,"error":{"code":-1,"message":"nope"}}"#) {
            Some(Msg::Reply { id: 4, result: Err(m) }) => assert_eq!(m, "nope"),
            other => panic!("{other:?}"),
        }
        assert!(classify("[adapter log line]").is_none());
    }

    /// Needs Node, the installed adapter and `claude`; opens sessions but sends no prompt, so it spends no tokens.
    /// Run with `cargo test -- --ignored two_chats_share_one_adapter`.
    #[test]
    #[ignore]
    fn two_chats_share_one_adapter() {
        use futures::StreamExt as _;
        let claude = crate::claude::discover(None).expect("claude not found");
        warm(&claude, vec![]);
        let tmp = tempfile::tempdir().unwrap();
        let mut chats = vec![];
        for name in ["a", "b"] {
            let (mut agent, rx) = Agent::attach(&claude, vec![], name).unwrap();
            let init = agent.call("initialize", json!({}));
            let cwd = tmp.path().join(name);
            std::fs::create_dir_all(&cwd).unwrap();
            let new = agent.call("session/new", json!({"cwd": cwd, "mcpServers": []}));
            chats.push((name, agent, rx, init, new));
        }
        let mut sessions = vec![];
        for (name, _agent, rx, init, new) in &mut chats {
            let (mut got_init, mut sid) = (false, None);
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while !(got_init && sid.is_some()) {
                assert!(std::time::Instant::now() < deadline, "chat {name} got no answers");
                match futures::executor::block_on(rx.next()) {
                    Some(Msg::Reply { id, result }) if id == *init => got_init = result.is_ok(),
                    Some(Msg::Reply { id, result }) if id == *new => sid = Some(result.unwrap()["sessionId"].as_str().unwrap().to_string()),
                    Some(Msg::Exit(t)) => panic!("adapter exited: {t}"),
                    _ => {}
                }
            }
            sessions.push(sid.unwrap());
        }
        assert_ne!(sessions[0], sessions[1], "each chat has its own conversation");
        for (_, agent, ..) in &chats {
            agent.close(&sessions[if agent.chat == "a" { 0 } else { 1 }]);
        }
    }
}
