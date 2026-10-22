//! Talking ACP (Agent Client Protocol: newline-delimited JSON-RPC over stdio) to Zed's Claude adapter.
//!
//! The adapter is a small Node program that drives the Claude Agent SDK. Claudiu points it at the user's own
//! `claude` executable (`CLAUDE_CODE_EXECUTABLE`), so the login, settings, skills and commands are Claude Code's,
//! exactly as in a terminal session. It is installed once into Claudiu's data dir with npm; nothing is bundled.
//! One adapter process per chat. Closing its stdin makes it exit and take its `claude` child along.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
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

pub struct Agent {
    /// Lines for the writer thread; dropping it closes the adapter's stdin.
    tx: Option<Sender<String>>,
    child: Option<Child>,
    next: u64,
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

impl Agent {
    /// Blocking: starts the process and its reader threads. Call it off the UI thread.
    pub fn spawn(claude: &Path, cwd: &Path, env: Vec<(String, String)>) -> Result<(Self, UnboundedReceiver<Msg>), String> {
        let mut cmd = node_tool("node", vec![script().to_string_lossy().into_owned()]).ok_or("Node.js was not found")?;
        cmd.current_dir(if cwd.is_dir() { cwd } else { Path::new(".") })
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

        let (events, rx) = unbounded();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(msg) = classify(&line)
                    && events.unbounded_send(msg).is_err()
                {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(100)); // let the stderr reader catch up
            let _ = events.unbounded_send(Msg::Exit(tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n")));
        });
        Ok((Self { tx: Some(tx), child: Some(child), next: 0 }, rx))
    }

    fn send(&self, v: Value) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(v.to_string());
        }
    }

    /// Send a request; the answer arrives as `Msg::Reply` with the returned id.
    pub fn call(&mut self, method: &str, params: Value) -> u64 {
        self.next += 1;
        self.send(json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}));
        self.next
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn respond(&self, id: Value, result: Result<Value, &str>) {
        self.send(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(m) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": m}}),
        });
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.tx = None; // closes stdin: the adapter exits and ends its `claude`
        let Some(mut child) = self.child.take() else { return };
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
